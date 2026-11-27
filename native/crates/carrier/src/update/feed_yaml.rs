//! Parser for the subset of YAML electron-builder writes to `latest.yml`.

use super::{Feed, FeedFile, UpdateError, invalid, parse_version};

/// One line that carries content: its number, indentation and text without trailing spaces.
#[derive(Debug, Clone, Copy)]
struct Line<'a> {
    number: usize,
    indent: usize,
    text: &'a str,
}

/// A `key: value` of a block mapping, with the lines nested below it.
struct Entry<'a> {
    line: usize,
    key: String,
    value: &'a str,
    nested: Vec<Line<'a>>,
}

impl Entry<'_> {
    /// A single-line scalar value.
    fn scalar(&self) -> Result<String, UpdateError> {
        if self.value.is_empty() || !self.nested.is_empty() {
            return Err(invalid(format!("line {}: {} is not a single value", self.line, self.key)));
        }
        scalar(self.value).map_err(|why| invalid(format!("line {}: {} {why}", self.line, self.key)))
    }

    fn number(&self) -> Result<u64, UpdateError> {
        let text = self.scalar()?;
        match text.parse::<u64>() {
            Ok(number) if text.bytes().all(|byte| byte.is_ascii_digit()) => Ok(number),
            _ => Err(invalid(format!("line {}: {} is not a whole number", self.line, self.key))),
        }
    }
}

/// Read `latest.yml` (the subset of YAML electron-builder writes: a block mapping of scalars and the `files` list of
/// mappings; scalars plain, single- or double-quoted). Unknown keys are ignored; `packages` is refused.
pub fn parse_feed(text: &str) -> Result<Feed, UpdateError> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut lines = Vec::new();
    for (index, raw) in text.lines().enumerate() {
        let content = raw.trim_start_matches(' ');
        let indent = raw.len() - content.len();
        let content = content.trim_end();
        if content.is_empty() || content.starts_with('#') {
            continue;
        }
        if content.starts_with('\t') {
            return Err(invalid(format!("line {}: tabs are not allowed for indentation", index + 1)));
        }
        if indent == 0 && (content == "---" || content.starts_with("--- ")) {
            if lines.is_empty() {
                continue;
            }
            return Err(invalid("latest.yml holds more than one document"));
        }
        if indent == 0 && content == "..." {
            break;
        }
        lines.push(Line { number: index + 1, indent, text: content });
    }
    let entries = mapping(&lines, 0).map_err(invalid)?;
    if entries.iter().any(|entry| entry.key == "packages") {
        return Err(UpdateError::WebInstaller);
    }
    let find = |key: &str| entries.iter().find(|entry| entry.key == key);
    let optional = |key: &str| find(key).map(Entry::scalar).transpose();
    let version_text = find("version").ok_or_else(|| invalid("version is missing"))?.scalar()?;
    let version = parse_version(&version_text).ok_or_else(|| invalid(format!("version is not a semantic version: {version_text}")))?;
    let files = match find("files") {
        None => Vec::new(),
        Some(entry) if entry.value.is_empty() && !entry.nested.is_empty() => {
            let items = sequence(&entry.nested).map_err(|why| invalid(format!("files: {why}")))?;
            items.iter().map(|item| feed_file(item)).collect::<Result<_, _>>()?
        }
        Some(entry) => return Err(invalid(format!("line {}: files is not a list", entry.line))),
    };
    Ok(Feed {
        version,
        files,
        path: optional("path")?,
        sha512: optional("sha512")?,
        release_date: optional("releaseDate")?,
        minimum_system_version: optional("minimumSystemVersion")?,
    })
}

fn feed_file(item: &[Entry<'_>]) -> Result<FeedFile, UpdateError> {
    let find = |key: &str| item.iter().find(|entry| entry.key == key);
    let first = item.first().map_or(0, |entry| entry.line);
    let required = |key: &str| find(key).ok_or_else(|| invalid(format!("line {first}: a file has no {key}")))?.scalar();
    Ok(FeedFile {
        url: required("url")?,
        sha512: required("sha512")?,
        size: find("size").map(Entry::number).transpose()?,
        block_map_size: find("blockMapSize").map(Entry::number).transpose()?,
    })
}

fn is_item(text: &str) -> bool {
    text == "-" || text.starts_with("- ")
}

/// The entries of a block mapping whose keys start at `column`. Deeper lines belong to the entry above them, as do list
/// items at the same column (`key:` followed by `- item`).
fn mapping<'a>(lines: &[Line<'a>], column: usize) -> Result<Vec<Entry<'a>>, String> {
    let mut entries: Vec<Entry<'a>> = Vec::new();
    for line in lines {
        let below = line.indent > column || (line.indent == column && is_item(line.text) && entries.last().is_some_and(|entry| entry.value.is_empty()));
        if below {
            entries.last_mut().ok_or_else(|| format!("line {}: unexpected indentation", line.number))?.nested.push(*line);
            continue;
        }
        if line.indent < column {
            return Err(format!("line {}: unexpected indentation", line.number));
        }
        let (key, value) = key_value(line.text).ok_or_else(|| format!("line {}: expected `key: value`", line.number))?;
        if entries.iter().any(|entry| entry.key == key) {
            return Err(format!("line {}: {key} appears twice", line.number));
        }
        entries.push(Entry { line: line.number, key, value, nested: Vec::new() });
    }
    Ok(entries)
}

/// The mappings of a block sequence (`- key: value` items).
fn sequence<'a>(lines: &[Line<'a>]) -> Result<Vec<Vec<Entry<'a>>>, String> {
    let column = lines.first().map_or(0, |line| line.indent);
    let mut items: Vec<Vec<Line<'a>>> = Vec::new();
    for line in lines {
        if line.indent > column {
            items.last_mut().ok_or_else(|| format!("line {}: expected a list item", line.number))?.push(*line);
            continue;
        }
        if line.indent < column || !is_item(line.text) {
            return Err(format!("line {}: expected a list item", line.number));
        }
        let rest = &line.text[1..];
        let content = rest.trim_start();
        let mut item = Vec::new();
        if !content.is_empty() {
            item.push(Line { number: line.number, indent: column + 1 + rest.len() - content.len(), text: content });
        }
        items.push(item);
    }
    items
        .iter()
        .map(|item| {
            let first = item.first().ok_or("a list item is empty")?;
            if is_item(first.text) {
                return Err(format!("line {}: nested lists are not supported", first.number));
            }
            mapping(item, first.indent)
        })
        .collect()
}

/// `key: value` or `key:`, with a plain or quoted key.
fn key_value(text: &str) -> Option<(String, &str)> {
    let (key, rest) = if text.starts_with(['\'', '"']) {
        let (key, rest) = quoted(text).ok()?;
        (key, rest.strip_prefix(':')?)
    } else {
        let end = text.find(": ").or_else(|| text.ends_with(':').then(|| text.len() - 1))?;
        let key = text[..end].trim_end();
        if key.is_empty() || key.starts_with(['-', '?', ':', '#', '&', '*', '!', '|', '>', '[', ']', '{', '}', ',', '@', '`', '%']) {
            return None;
        }
        (key.to_owned(), &text[end + 1..])
    };
    if !(rest.is_empty() || rest.starts_with(' ')) {
        return None;
    }
    let value = rest.trim_start();
    // `key: # comment` has no value.
    Some((key, if value.starts_with('#') { "" } else { value }))
}

/// One scalar on one line: plain (up to a ` #` comment), single-quoted or double-quoted.
fn scalar(value: &str) -> Result<String, String> {
    if value.starts_with(['\'', '"']) {
        let (text, rest) = quoted(value)?;
        let rest = rest.trim_start();
        if !(rest.is_empty() || rest.starts_with('#')) {
            return Err("has text after its closing quote".into());
        }
        return Ok(text);
    }
    if value.starts_with(['|', '>', '[', '{', '&', '*', '!', '%', '@', '`']) {
        return Err("is not a plain value".into());
    }
    let end = value.find(" #").unwrap_or(value.len());
    Ok(value[..end].trim_end().to_owned())
}

/// A quoted scalar at the start of `text` and what follows its closing quote.
fn quoted(text: &str) -> Result<(String, &str), String> {
    let mut chars = text.char_indices();
    let quote = chars.next().map(|(_, quote)| quote).ok_or("is empty")?;
    let mut out = String::new();
    while let Some((index, c)) = chars.next() {
        match (quote, c) {
            ('\'', '\'') => {
                if text[index + 1..].starts_with('\'') {
                    chars.next();
                    out.push('\'');
                } else {
                    return Ok((out, &text[index + 1..]));
                }
            }
            ('"', '"') => return Ok((out, &text[index + 1..])),
            ('"', '\\') => {
                let (_, escape) = chars.next().ok_or("ends inside an escape")?;
                let hex = |chars: &mut std::str::CharIndices<'_>, digits: usize| -> Result<char, String> {
                    let code: String = (0..digits).filter_map(|_| chars.next().map(|(_, c)| c)).collect();
                    u32::from_str_radix(&code, 16)
                        .ok()
                        .filter(|_| code.len() == digits)
                        .and_then(char::from_u32)
                        .ok_or_else(|| "has an invalid escape".to_owned())
                };
                out.push(match escape {
                    '\\' => '\\',
                    '"' => '"',
                    '/' => '/',
                    ' ' => ' ',
                    'n' => '\n',
                    'r' => '\r',
                    't' => '\t',
                    '0' => '\0',
                    'b' => '\u{8}',
                    'f' => '\u{c}',
                    'e' => '\u{1b}',
                    'a' => '\u{7}',
                    'v' => '\u{b}',
                    'x' => hex(&mut chars, 2)?,
                    'u' => hex(&mut chars, 4)?,
                    'U' => hex(&mut chars, 8)?,
                    _ => return Err("has an invalid escape".into()),
                });
            }
            _ => out.push(c),
        }
    }
    Err("has no closing quote".into())
}
