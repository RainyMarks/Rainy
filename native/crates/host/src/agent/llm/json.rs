//! Tool call arguments as models write them: JSON, sometimes with raw control characters or stray backslashes in
//! strings, and cut short when the reply hit its output limit (pi-ai `parseStreamingJson`).

use serde_json::{Map, Value};

/// The arguments of a finished tool call: parsed, repaired, or completed; an empty object when nothing is usable.
pub fn parse_arguments(text: &str) -> Map<String, Value> {
    let text = text.trim();
    if text.is_empty() {
        return Map::new();
    }
    let parsed = serde_json::from_str::<Value>(text).ok().or_else(|| serde_json::from_str::<Value>(&repair(text)).ok()).or_else(|| serde_json::from_str::<Value>(&complete(&repair(text))).ok());
    match parsed {
        Some(Value::Object(map)) => map,
        _ => Map::new(),
    }
}

/// Escape raw control characters inside strings and double backslashes before characters JSON cannot escape.
fn repair(json: &str) -> String {
    let chars: Vec<char> = json.chars().collect();
    let mut repaired = String::with_capacity(json.len());
    let mut in_string = false;
    let mut index = 0;
    while index < chars.len() {
        let char = chars[index];
        if !in_string {
            repaired.push(char);
            in_string = char == '"';
            index += 1;
            continue;
        }
        match char {
            '"' => {
                repaired.push(char);
                in_string = false;
            }
            '\\' => match chars.get(index + 1) {
                None => repaired.push_str("\\\\"),
                Some('u') if chars.len() >= index + 6 && chars[index + 2..index + 6].iter().all(char::is_ascii_hexdigit) => {
                    repaired.extend(&chars[index..index + 6]);
                    index += 5;
                }
                Some(next) if matches!(next, '"' | '\\' | '/' | 'b' | 'f' | 'n' | 'r' | 't') => {
                    repaired.push('\\');
                    repaired.push(*next);
                    index += 1;
                }
                Some(_) => repaired.push_str("\\\\"),
            },
            '\n' => repaired.push_str("\\n"),
            '\r' => repaired.push_str("\\r"),
            '\t' => repaired.push_str("\\t"),
            '\u{8}' => repaired.push_str("\\b"),
            '\u{c}' => repaired.push_str("\\f"),
            char if (char as u32) < 0x20 => repaired.push_str(&format!("\\u{:04x}", char as u32)),
            char => repaired.push(char),
        }
        index += 1;
    }
    repaired
}

/// Close what a cut-off document left open: its string, a dangling key or separator, and its arrays and objects.
fn complete(json: &str) -> String {
    let mut stack: Vec<char> = Vec::new();
    let mut in_string = false;
    let mut escaped = false;
    for char in json.chars() {
        if in_string {
            match char {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match char {
            '"' => in_string = true,
            '{' => stack.push('}'),
            '[' => stack.push(']'),
            '}' | ']' => {
                stack.pop();
            }
            _ => {}
        }
    }
    let mut text = json.to_owned();
    if escaped {
        text.pop();
    }
    if in_string {
        text.push('"');
    }
    // A value cut before it started cannot be guessed: drop its key, or the separator waiting for it.
    loop {
        let trimmed = text.trim_end();
        let (cut, key) = (trimmed.len().saturating_sub(1), trimmed.ends_with(':'));
        if !trimmed.ends_with(',') && !key {
            break;
        }
        text.truncate(cut);
        // A dangling key goes with its colon.
        if key && let Some(start) = text.trim_end().strip_suffix('"').and_then(|rest| rest.rfind('"')) {
            text.truncate(start);
        }
    }
    // A literal cut in half (`tru`, `nul`) is dropped with its key.
    let trimmed = text.trim_end().to_owned();
    if let Some(last) = trimmed.rsplit([',', ':', '[', '{']).next()
        && let word = last.trim()
        && !word.is_empty()
        && word.chars().all(|char| char.is_ascii_alphabetic())
        && !["true", "false", "null"].contains(&word)
    {
        text.truncate(trimmed.len() - last.len());
        return complete(&text);
    }
    while let Some(close) = stack.pop() {
        text.push(close);
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_broken_and_cut_arguments() {
        assert_eq!(Value::Object(parse_arguments(r#"{"path":"a.txt"}"#)), json!({ "path": "a.txt" }));
        assert_eq!(Value::Object(parse_arguments("{\"text\":\"a\nb\",\"p\":\"C:\\x\"}")), json!({ "text": "a\nb", "p": "C:\\x" }));
        assert_eq!(Value::Object(parse_arguments(r#"{"path":"a.txt","content":"hel"#)), json!({ "path": "a.txt", "content": "hel" }));
        assert_eq!(Value::Object(parse_arguments(r#"{"items":[1,2"#)), json!({ "items": [1, 2] }));
        assert_eq!(Value::Object(parse_arguments(r#"{"a":1,"b":"#)), json!({ "a": 1 }));
        assert_eq!(Value::Object(parse_arguments(r#"{"a":1,"b":tr"#)), json!({ "a": 1 }));
        assert!(parse_arguments("not json").is_empty());
    }
}
