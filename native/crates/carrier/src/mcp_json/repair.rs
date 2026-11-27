//! Repairs to pasted MCP JSON text: JSONC comments, trailing commas, full-width typography,
//! single Windows backslashes, Markdown fences, and non-JSON format detection.

use std::sync::LazyLock;

use regex::Regex;

use super::js_json::{Json, describe, is_js_space, js_regex, js_trim, parse_json};
use super::{MAX_BLOCKS, McpImportMessage};

/// Drop `//` and `/* */` comments outside strings (VS Code's JSONC).
fn strip_comments(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut index = 0;
    let mut in_string = false;
    while index < chars.len() {
        let c = chars[index];
        if in_string {
            out.push(c);
            if c == '\\' {
                if let Some(next) = chars.get(index + 1) {
                    out.push(*next);
                }
                index += 2;
                continue;
            }
            if c == '"' {
                in_string = false;
            }
            index += 1;
            continue;
        }
        if c == '"' {
            in_string = true;
            out.push(c);
            index += 1;
            continue;
        }
        if c == '/' && chars.get(index + 1) == Some(&'/') {
            while index < chars.len() && chars[index] != '\n' {
                index += 1;
            }
            continue;
        }
        if c == '/' && chars.get(index + 1) == Some(&'*') {
            index = (index + 2..chars.len().saturating_sub(1)).find(|&at| chars[at] == '*' && chars[at + 1] == '/').map_or(chars.len(), |at| at + 2);
            continue;
        }
        out.push(c);
        index += 1;
    }
    out
}

/// Drop commas outside strings that close nothing: before `}`, `]` or the end.
fn strip_trailing_commas(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut in_string = false;
    let mut index = 0;
    while index < chars.len() {
        let c = chars[index];
        index += 1;
        if in_string {
            out.push(c);
            if c == '\\' {
                if let Some(next) = chars.get(index) {
                    out.push(*next);
                }
                index += 1;
                continue;
            }
            if c == '"' {
                in_string = false;
            }
            continue;
        }
        if c == '"' {
            in_string = true;
            out.push(c);
            continue;
        }
        if c == ',' {
            let mut next = index;
            while next < chars.len() && is_js_space(chars[next]) {
                next += 1;
            }
            if next >= chars.len() || chars[next] == '}' || chars[next] == ']' {
                continue;
            }
        }
        out.push(c);
    }
    out
}

/// Straighten what web pages and chat apps do to copied JSON: curly quotes, full-width punctuation, invisible spaces.
fn straighten(text: &str) -> String {
    text.chars()
        .filter(|c| !matches!(c, '\u{200B}'..='\u{200D}' | '\u{2060}' | '\u{FEFF}'))
        .map(|c| match c {
            '\u{A0}' | '\u{3000}' => ' ',
            '“' | '”' | '„' | '‟' | '″' | '＂' => '"',
            '：' => ':',
            '，' => ',',
            '｛' => '{',
            '｝' => '}',
            '［' => '[',
            '］' => ']',
            other => other,
        })
        .collect()
}

/// Double the backslashes of Windows paths written with single ones (`"C:\Users\me\new\server.py"`, `.\build\index.js`),
/// which JSON reads as escapes. Only tried once the text failed to parse: MCP values hold no tabs or line breaks, so `\n`,
/// `\t`, `\b`, `\f` and `\r` are taken as folder names; `\\`, `\"`, `\/` and `\uXXXX` stay escapes.
fn double_backslashes(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    // A path that ends in a backslash, `"D:\Projects\"`, would otherwise escape its own closing quote.
    let mut closed: Vec<char> = Vec::with_capacity(chars.len() + 16);
    let mut index = 0;
    while index < chars.len() {
        if chars[index] == '\\' && chars.get(index + 1) == Some(&'"') && (index == 0 || chars[index - 1] != '\\') {
            let mut next = index + 2;
            while next < chars.len() && matches!(chars[next], ' ' | '\t') {
                next += 1;
            }
            if next < chars.len() && matches!(chars[next], ',' | ']' | '}' | '\r' | '\n') {
                closed.extend(['\\', '\\', '"']);
                index += 2;
                continue;
            }
        }
        closed.push(chars[index]);
        index += 1;
    }
    // One pass, string by string: a string runs to the next unescaped quote on its line; one left open is copied as it is.
    let mut out = String::with_capacity(closed.len() + 16);
    let mut index = 0;
    loop {
        let Some(quote) = closed[index..].iter().position(|c| *c == '"').map(|at| index + at) else {
            out.extend(&closed[index..]);
            break;
        };
        out.extend(&closed[index..=quote]);
        let mut end = quote + 1;
        let mut body = String::new();
        while end < closed.len() && closed[end] != '"' && closed[end] != '\n' {
            let c = closed[end];
            if c != '\\' {
                body.push(c);
                end += 1;
                continue;
            }
            let escape = match closed.get(end + 1) {
                Some('"' | '\\' | '/') => 2,
                Some('u') if closed.len() >= end + 6 && closed[end + 2..end + 6].iter().all(char::is_ascii_hexdigit) => 6,
                _ => 0,
            };
            if escape == 0 {
                body.push_str("\\\\");
                end += 1;
            } else {
                body.extend(&closed[end..end + escape]);
                end += escape;
            }
        }
        if closed.get(end) == Some(&'"') {
            out.push_str(&body);
            out.push('"');
            index = end + 1;
        } else {
            out.extend(&closed[quote + 1..end]);
            index = end;
        }
    }
    out
}

/// Read one block as JSON, forgiving what copying commonly does to it: comments, trailing commas, a fragment such as
/// `"github": { … }` without its outer braces, typography from web pages, and single backslashes in Windows paths.
/// The value comes with whether backslashes were doubled; the error is the plain text's.
fn read_block(text: &str) -> Result<(Json, bool), String> {
    let prepare = |body: &str| strip_trailing_commas(&strip_comments(js_trim(body)));
    let wrap = |body: String| {
        if body.starts_with(['[', '{']) {
            vec![body]
        } else {
            let wrapped = format!("{{{body}}}");
            vec![body, wrapped]
        }
    };
    let mut error = None;
    for attempt in wrap(prepare(text)) {
        match parse_json(&attempt) {
            Ok(value) => return Ok((value, false)),
            Err((chars, failure)) => {
                error.get_or_insert_with(|| describe(&chars, failure));
            }
        }
    }
    // The repairs that change values are tried last, and typography last of all: it also rewrites quotes inside values.
    type RepairStep = (fn(&str) -> String, bool);
    let repairs: [RepairStep; 3] =
        [(double_backslashes, true), (straighten, false), (|text| double_backslashes(&straighten(text)), true)];
    for (repair, backslashes) in repairs {
        for attempt in wrap(prepare(&repair(text))) {
            if let Ok(value) = parse_json(&attempt) {
                return Ok((value, backslashes));
            }
        }
    }
    Err(js_trim(&error.unwrap_or_default()).to_owned())
}

fn has_quote(text: &str) -> bool {
    text.contains(['"', '“', '”', '＂'])
}

/// The balanced `{…}` blocks of a text, outside strings, skipping the prose between them. Lists are skipped too: in prose
/// or TOML they are arguments, not configurations.
fn top_level_blocks(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut blocks = Vec::new();
    let mut depth = 0usize;
    let mut start = 0;
    let mut in_string = false;
    let mut index = 0;
    while index < chars.len() {
        let c = chars[index];
        if in_string {
            if c == '\\' {
                index += 1;
            } else if c == '"' {
                in_string = false;
            }
        } else if c == '"' && depth > 0 {
            in_string = true;
        } else if c == '{' || c == '[' {
            if depth == 0 {
                start = index;
            }
            depth += 1;
        } else if (c == '}' || c == ']') && depth > 0 {
            depth -= 1;
            if depth == 0 && chars[start] == '{' {
                blocks.push(chars[start..=index].iter().collect());
            }
        }
        index += 1;
    }
    blocks
}

/// Markdown code blocks: where each starts and ends, and where its content does.
fn fences(text: &str) -> Vec<(usize, usize, usize, usize)> {
    let bytes = text.as_bytes();
    let mut found = Vec::new();
    let mut from = 0;
    while let Some(at) = text[from..].find("```").map(|at| from + at) {
        let mut info = at + 3;
        // The info string holds no backticks, so a long run of them is not scanned again from every position.
        while info < bytes.len() && bytes[info] != b'\n' && bytes[info] != b'`' {
            info += 1;
        }
        if bytes.get(info) == Some(&b'\n') {
            match text[info + 1..].find("```") {
                Some(close) => {
                    let close = info + 1 + close;
                    found.push((at, close + 3, info + 1, close));
                    from = close + 3;
                    continue;
                }
                None => break,
            }
        }
        from = at + 1;
    }
    found
}

/// The JSON values of the text: its Markdown code blocks and what lies outside them, each whole or block by block.
pub(super) fn read_values(text: &str) -> Result<(Vec<Json>, bool), String> {
    let fences = fences(text);
    // Only blocks with a quote can hold a server; a paste has a handful of them, so a flood of empty ones is cut short.
    let mut sources: Vec<String> =
        fences.iter().map(|(_, _, start, end)| &text[*start..*end]).filter(|source| has_quote(source)).take(MAX_BLOCKS).map(str::to_owned).collect();
    // Outside the code blocks: the whole paste when it has none, else JSON written after an install command and the like.
    let mut outside = String::new();
    let mut end = 0;
    for (start, stop, _, _) in &fences {
        outside.push_str(&text[end..*start]);
        outside.push('\n');
        end = *stop;
    }
    outside.push_str(&text[end..]);
    if fences.is_empty() || outside.contains(['{', '[']) {
        sources.push(outside);
    }
    let mut values = Vec::new();
    let mut backslashes = false;
    let mut first: Option<String> = None;
    let mut take = |read: Result<(Json, bool), String>| -> bool {
        match read {
            Ok((value, doubled)) => {
                values.push(value);
                backslashes |= doubled;
                true
            }
            Err(_) => false,
        }
    };
    let blocks = |body: &str| -> Vec<String> { top_level_blocks(body).into_iter().filter(|block| has_quote(block)).take(MAX_BLOCKS).collect() };
    for source in &sources {
        let whole = read_block(source);
        if let Err(error) = &whole {
            first.get_or_insert_with(|| error.clone());
        }
        if take(whole) {
            continue;
        }
        // Prose around the JSON, or several blocks pasted one after another; full-width braces only when nothing else reads.
        let found = blocks(source).iter().filter(|block| take(read_block(block))).count();
        if found == 0 {
            for block in blocks(&straighten(source)) {
                take(read_block(&block));
            }
        }
    }
    if values.is_empty() { Err(first.unwrap_or_else(|| "JSON.parse failed".into())) } else { Ok((values, backslashes)) }
}

static YAML_KEY: LazyLock<Regex> = LazyLock::new(|| js_regex(r"^mcpServers\s*:$"));
static YAML_ITEM: LazyLock<Regex> = LazyLock::new(|| js_regex(r"^-\s+name\s*:"));
static INSTALL_COMMAND: LazyLock<Regex> = LazyLock::new(|| js_regex(r"^(?:claude|codex|gemini|qwen|kimi|code)\s+(?:mcp\s+add|--add-mcp)(?-u:\b)"));

/// What the text is when it is not JSON: a TOML or YAML file, or a command line from a README.
pub(super) fn not_json(text: &str) -> Option<McpImportMessage> {
    let lines: Vec<&str> = text.split('\n').map(js_trim).collect();
    if lines.iter().any(|line| line.starts_with("[mcp_servers.") || line.starts_with("[mcp_servers]")) {
        return Some(McpImportMessage::new("mcpJsonToml"));
    }
    if lines.iter().any(|line| YAML_KEY.is_match(line) || YAML_ITEM.is_match(line)) {
        return Some(McpImportMessage::new("mcpJsonYaml"));
    }
    if lines.iter().any(|line| INSTALL_COMMAND.is_match(line) || line.contains("@smithery/cli")) {
        return Some(McpImportMessage::new("mcpJsonCommandLine"));
    }
    None
}

static ADD_JSON: LazyLock<Regex> = LazyLock::new(|| {
    js_regex(r#"^\s*claude\s+mcp\s+add-json\s+(?:(?:-s|--scope)\s+\S+\s+)*("[^"]+"|\S+)\s+'((?s:.)*)'(?:\s+(?:-s|--scope)\s+\S+)*\s*$"#)
});

/// `claude mcp add-json <name> '<json>'`: the server's name and its object.
pub(super) fn add_json_command(text: &str) -> Option<(String, &str)> {
    let found = ADD_JSON.captures(text)?;
    let name = found.get(1)?.as_str();
    let name = name.strip_prefix('"').unwrap_or(name);
    let name = name.strip_suffix('"').unwrap_or(name);
    Some((name.to_owned(), found.get(2)?.as_str()))
}
