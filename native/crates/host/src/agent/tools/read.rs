//! `read`: a line-numbered window of a text file.

use std::io::BufRead as _;

use futures::future::BoxFuture;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::encoding::{Encoding, decode};
use super::{Tool, ToolContext, ToolOutput, Updates, params, spec, version_of};
use crate::agent::llm::ToolSpec;

/// Lines returned by one call.
pub const MAX_LINES: usize = 2000;
/// Characters kept per line.
pub const MAX_LINE_LENGTH: usize = 2000;
/// Bytes of selected output per call.
pub const MAX_BYTES: usize = 50 * 1024;
/// Files up to this size are read whole, so their encoding is recognized; larger ones are streamed as UTF-8.
const DECODE_MAX_BYTES: u64 = 16 * 1024 * 1024;

/// Selected lines of a file.
pub struct Window {
    pub lines: Vec<(usize, String)>,
    pub total: usize,
    pub capped: bool,
}

/// The window `offset..offset+limit` (1-based) of a sequence of lines.
pub fn window(lines: impl Iterator<Item = String>, offset: usize, limit: usize, display: &str) -> Result<Window, String> {
    let mut window = Window { lines: Vec::new(), total: 0, capped: false };
    let mut bytes = 0;
    for raw in lines {
        window.total += 1;
        if window.capped || window.total < offset || window.lines.len() >= limit {
            continue;
        }
        let line = raw.strip_suffix('\r').unwrap_or(&raw);
        let text = if line.chars().count() > MAX_LINE_LENGTH { format!("{}... (line truncated to {MAX_LINE_LENGTH} chars)", crate::text::truncate_chars(line, MAX_LINE_LENGTH)) } else { line.to_owned() };
        let size = text.len() + usize::from(!window.lines.is_empty());
        if bytes + size > MAX_BYTES {
            window.capped = true;
            continue;
        }
        bytes += size;
        window.lines.push((window.total, text));
    }
    if !window.capped && offset > window.total && !(window.total == 0 && offset == 1) {
        return Err(format!("offset {offset} is out of range for \"{display}\" ({} lines)", window.total));
    }
    Ok(window)
}

/// `<path>…</path><type>file</type><content>…</content>`.
pub fn format(display: &str, offset: usize, window: &Window, encoding: Option<&str>) -> String {
    let end = window.lines.last().map_or(offset.saturating_sub(1), |(number, _)| *number);
    let footer = if window.capped {
        format!("(Output capped. Showing lines {offset}-{end}. Use offset={} to continue.)", end + 1)
    } else if end < window.total {
        format!("(Showing lines {offset}-{end} of {}. Use offset={} to continue.)", window.total, end + 1)
    } else {
        format!("(End of file - total {} lines)", window.total)
    };
    let body = if window.lines.is_empty() { footer } else { format!("{}\n\n{footer}", window.lines.iter().map(|(number, text)| format!("{number}: {text}")).collect::<Vec<_>>().join("\n")) };
    let encoding = encoding.map_or_else(String::new, |encoding| format!("<encoding>{encoding}</encoding>\n"));
    format!("<path>{display}</path>\n<type>file</type>\n{encoding}<content>\n{body}\n</content>")
}

/// Lines of text the way a file splits them: a final newline ends the last line rather than starting another.
fn split_lines(text: &str) -> impl Iterator<Item = String> + '_ {
    let trimmed = text.strip_suffix('\n').unwrap_or(text);
    let empty = text.is_empty();
    trimmed.split('\n').map(str::to_owned).filter(move |_| !empty)
}

#[derive(Deserialize)]
struct Params {
    file_path: String,
    offset: Option<f64>,
    limit: Option<f64>,
}

fn positive(value: Option<f64>, name: &str, fallback: usize) -> Result<usize, String> {
    match value {
        None => Ok(fallback),
        Some(number) if number.fract() == 0.0 && number >= 1.0 => Ok(number as usize),
        Some(_) => Err(format!("invalid {name}: expected a positive integer")),
    }
}

pub struct Read {
    context: ToolContext,
    spec: ToolSpec,
}

impl Read {
    pub fn new(context: ToolContext) -> Self {
        let spec = spec(
            "read",
            "Read a file or a bounded line range. Use small ranges for large files.",
            json!({
                "type": "object",
                "properties": {
                    "file_path": { "type": "string", "description": "Path to read, resolved by the filesystem backend." },
                    "offset": { "type": "number", "description": "1-based first line to return. Defaults to 1." },
                    "limit": { "type": "number", "description": format!("Maximum number of lines to return. Defaults to {MAX_LINES}.") },
                },
                "required": ["file_path"],
            }),
        );
        Self { context, spec }
    }

    fn run(&self, args: Value) -> Result<ToolOutput, String> {
        let params: Params = params(args)?;
        let offset = positive(params.offset, "offset", 1)?;
        let limit = positive(params.limit, "limit", MAX_LINES)?;
        if limit > MAX_LINES {
            return Err(format!("invalid limit: must be at most {MAX_LINES}"));
        }
        let (absolute, display) = self.context.resolve(&params.file_path)?;
        let Some(version) = version_of(&absolute)? else {
            self.context.observations.record(&absolute, None);
            return Err(format!("cannot read \"{display}\": not found"));
        };
        let info = std::fs::metadata(&absolute).map_err(|error| error.to_string())?;
        if !info.is_file() {
            return Err(format!("cannot read \"{display}\": not a regular file"));
        }
        let (window, encoding) = if info.len() <= DECODE_MAX_BYTES {
            let bytes = std::fs::read(&absolute).map_err(|error| error.to_string())?;
            let decoded = decode(&bytes).ok_or_else(|| format!("cannot read \"{display}\": it is a binary file ({} bytes) or its text encoding is not supported", info.len()))?;
            let encoding = if decoded.encoding != Encoding::Utf8 {
                Some(decoded.encoding.label().to_owned())
            } else if decoded.damaged {
                Some("UTF-8 with invalid bytes, shown as U+FFFD; edit will refuse this file".to_owned())
            } else {
                None
            };
            (window(split_lines(&decoded.text), offset, limit, &display)?, encoding)
        } else {
            let file = std::fs::File::open(&absolute).map_err(|error| error.to_string())?;
            let lines = std::io::BufReader::new(file).split(b'\n').map_while(Result::ok).map(|line| String::from_utf8_lossy(&line).into_owned());
            (window(lines, offset, limit, &display)?, None)
        };
        self.context.observations.record(&absolute, Some(version));
        self.context.touched(&absolute);
        let lines: Vec<Value> = window.lines.iter().map(|(number, text)| json!({ "number": number, "text": text })).collect();
        Ok(ToolOutput::text(format(&display, offset, &window, encoding.as_deref()), json!({ "path": display, "offset": offset, "lines": lines, "totalLines": window.total })))
    }
}

impl Tool for Read {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn execute(&self, _call_id: String, args: Value, _cancel: CancellationToken, _updates: Updates) -> BoxFuture<'_, Result<ToolOutput, String>> {
        Box::pin(async move { self.run(args) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_lines_like_3x() {
        let text = "one\r\ntwo\nthree\n";
        let window = window(split_lines(text), 2, 1, "a.txt").unwrap();
        assert_eq!((window.lines.clone(), window.total), (vec![(2, "two".to_owned())], 3));
        assert_eq!(format("a.txt", 2, &window, None), "<path>a.txt</path>\n<type>file</type>\n<content>\n2: two\n\n(Showing lines 2-2 of 3. Use offset=3 to continue.)\n</content>");
        assert!(super::window(split_lines("a\n"), 5, 10, "a.txt").is_err());
        assert_eq!(super::window(split_lines(""), 1, 10, "e").unwrap().total, 0);
    }
}
