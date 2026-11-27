//! JavaScript text rules and iterative `JSON.parse` matching V8 key order and error messages.

use std::collections::HashMap;

use regex::Regex;

use super::MAX_DEPTH;

/// JavaScript's `\s`: its white space and line terminators.
pub(super) fn is_js_space(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{0B}' | '\u{0C}' | '\r' | ' ' | '\u{A0}' | '\u{1680}' | '\u{2000}'..='\u{200A}' | '\u{2028}' | '\u{2029}' | '\u{202F}' | '\u{205F}' | '\u{3000}' | '\u{FEFF}'
    )
}

/// The characters of [`is_js_space`], for a regular expression class.
const JS_SPACE: &str = r"\t\n\x0B\x0C\r \x{A0}\x{1680}\x{2000}-\x{200A}\x{2028}\x{2029}\x{202F}\x{205F}\x{3000}\x{FEFF}";

/// `String.prototype.trim`.
pub(super) fn js_trim(text: &str) -> &str {
    text.trim_matches(is_js_space)
}

/// Whether text is empty once trimmed as JavaScript trims it.
pub fn is_blank(text: &str) -> bool {
    js_trim(text).is_empty()
}

pub(super) fn has_js_space(text: &str) -> bool {
    text.contains(is_js_space)
}

/// A regular expression written with JavaScript's `\s` and `\S`.
pub(super) fn js_regex(pattern: &str) -> Regex {
    let pattern = pattern.replace(r"\S", &format!("[^{JS_SPACE}]")).replace(r"\s", &format!("[{JS_SPACE}]"));
    Regex::new(&pattern).expect("valid pattern")
}

/// JavaScript's `String(number)`.
pub(super) fn number_text(value: f64) -> String {
    if value.is_nan() {
        return "NaN".into();
    }
    if value == 0.0 {
        return "0".into();
    }
    if value.is_infinite() {
        return if value > 0.0 { "Infinity" } else { "-Infinity" }.into();
    }
    let sign = if value < 0.0 { "-" } else { "" };
    // The shortest digits that read back as the same number, and where the point goes.
    let scientific = format!("{:e}", value.abs());
    let (mantissa, exponent) = scientific.split_once('e').unwrap_or((&scientific, "0"));
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let point = exponent.parse::<i32>().unwrap_or(0) + 1;
    let count = digits.len() as i32;
    let body = if count <= point && point <= 21 {
        format!("{digits}{}", "0".repeat((point - count) as usize))
    } else if 0 < point && point <= 21 {
        format!("{}.{}", &digits[..point as usize], &digits[point as usize..])
    } else if -6 < point && point <= 0 {
        format!("0.{}{digits}", "0".repeat((-point) as usize))
    } else {
        let exponent = point - 1;
        let sign = if exponent < 0 { '-' } else { '+' };
        let (first, rest) = digits.split_at(1);
        if rest.is_empty() { format!("{first}e{sign}{}", exponent.abs()) } else { format!("{first}.{rest}e{sign}{}", exponent.abs()) }
    };
    format!("{sign}{body}")
}

/// Order of UTF-16 units, as JavaScript compares strings.
pub(super) fn compare_js(a: &str, b: &str) -> std::cmp::Ordering {
    a.encode_utf16().cmp(b.encode_utf16())
}

// JSON as `JSON.parse` reads it.

#[derive(Debug, Clone)]
pub(super) enum Json {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<Json>),
    Object(JsonObject),
}

/// An object's own properties; a repeated key keeps its first place and its last value.
#[derive(Debug, Clone, Default)]
pub(super) struct JsonObject {
    pub(super) entries: Vec<(String, Json)>,
    index: HashMap<String, usize>,
}

/// `key` as an array index: a canonical decimal integer below 2³² − 1.
fn array_index(key: &str) -> Option<u32> {
    if key.is_empty() || key.len() > 10 || !key.bytes().all(|b| b.is_ascii_digit()) || (key.len() > 1 && key.starts_with('0')) {
        return None;
    }
    key.parse::<u64>().ok().filter(|index| *index < u64::from(u32::MAX)).map(|index| index as u32)
}

impl JsonObject {
    pub(super) fn insert(&mut self, key: String, value: Json) {
        match self.index.get(&key) {
            Some(&at) => self.entries[at].1 = value,
            None => {
                self.index.insert(key.clone(), self.entries.len());
                self.entries.push((key, value));
            }
        }
    }

    pub(super) fn get(&self, key: &str) -> Option<&Json> {
        self.index.get(key).map(|&at| &self.entries[at].1)
    }

    pub(super) fn has(&self, key: &str) -> bool {
        self.index.contains_key(key)
    }

    /// The properties in JavaScript's order: array indices first, ascending, then the other keys as they came.
    pub(super) fn ordered(&self) -> Vec<(&str, &Json)> {
        let mut indices = Vec::new();
        let mut others = Vec::new();
        for (key, value) in &self.entries {
            match array_index(key) {
                Some(index) => indices.push((index, key.as_str(), value)),
                None => others.push((key.as_str(), value)),
            }
        }
        indices.sort_by_key(|(index, _, _)| *index);
        indices.into_iter().map(|(_, key, value)| (key, value)).chain(others).collect()
    }
}

/// Pairs in the order `Object.fromEntries` leaves them.
pub(super) fn js_ordered(pairs: Vec<(String, String)>) -> Vec<(String, String)> {
    let (mut indices, others): (Vec<_>, Vec<_>) = pairs.into_iter().partition(|(key, _)| array_index(key).is_some());
    indices.sort_by_key(|(key, _)| array_index(key));
    indices.extend(others);
    indices
}

/// Why `JSON.parse` failed, as V8 says it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum JsonFailure {
    /// "Unexpected end of JSON input".
    End,
    /// "Unexpected token 'x'": the character at this place.
    Token(usize),
    /// A message that names the place.
    At(&'static str, usize),
}

struct JsonParser<'a> {
    chars: &'a [char],
    pos: usize,
}

enum Frame {
    Array(Vec<Json>),
    Object(JsonObject, String),
}

impl JsonParser<'_> {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn skip_whitespace(&mut self) {
        while matches!(self.peek(), Some(' ' | '\t' | '\n' | '\r')) {
            self.pos += 1;
        }
    }

    /// The character here where it cannot stand.
    fn unexpected(&self) -> JsonFailure {
        match self.peek() {
            None => JsonFailure::End,
            Some('"') => JsonFailure::At("Unexpected string in JSON", self.pos),
            Some('-' | '0'..='9') => JsonFailure::At("Unexpected number in JSON", self.pos),
            Some(_) => JsonFailure::Token(self.pos),
        }
    }

    fn check(&mut self, expected: char) -> bool {
        self.skip_whitespace();
        if self.peek() == Some(expected) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn expect(&mut self, expected: char, message: &'static str) -> Result<(), JsonFailure> {
        if self.peek() == Some(expected) {
            self.pos += 1;
            Ok(())
        } else {
            Err(JsonFailure::At(message, self.pos))
        }
    }

    fn expect_next(&mut self, expected: char, message: &'static str) -> Result<(), JsonFailure> {
        self.skip_whitespace();
        self.expect(expected, message)
    }

    /// A property name and its colon, the opening quote already read.
    fn property(&mut self) -> Result<String, JsonFailure> {
        let key = self.string()?;
        self.expect_next(':', "Expected ':' after property name in JSON")?;
        Ok(key)
    }

    /// A string, the opening quote already read.
    fn string(&mut self) -> Result<String, JsonFailure> {
        let mut out = String::new();
        let mut high: Option<u32> = None;
        let flush = |out: &mut String, high: &mut Option<u32>| {
            if high.take().is_some() {
                out.push('\u{FFFD}');
            }
        };
        loop {
            let Some(c) = self.peek() else { return Err(JsonFailure::At("Unterminated string in JSON", self.pos)) };
            if c == '"' {
                self.pos += 1;
                flush(&mut out, &mut high);
                return Ok(out);
            }
            if c == '\\' {
                self.pos += 1;
                let Some(escape) = self.peek() else { return Err(JsonFailure::End) };
                if escape as u32 > 0xFF {
                    return Err(JsonFailure::Token(self.pos));
                }
                let decoded = match escape {
                    '"' | '\\' | '/' => escape,
                    'b' => '\u{8}',
                    'f' => '\u{C}',
                    'n' => '\n',
                    'r' => '\r',
                    't' => '\t',
                    'u' => {
                        let mut value = 0;
                        for _ in 0..4 {
                            self.pos += 1;
                            match self.peek().and_then(|digit| digit.to_digit(16)) {
                                Some(digit) => value = value * 16 + digit,
                                None => return Err(JsonFailure::At("Bad Unicode escape in JSON", self.pos)),
                            }
                        }
                        self.pos += 1;
                        match value {
                            0xD800..=0xDBFF => {
                                flush(&mut out, &mut high);
                                high = Some(value);
                            }
                            0xDC00..=0xDFFF => match high.take() {
                                Some(first) => out.push(char::from_u32(0x10000 + ((first - 0xD800) << 10) + (value - 0xDC00)).unwrap_or('\u{FFFD}')),
                                None => out.push('\u{FFFD}'),
                            },
                            _ => {
                                flush(&mut out, &mut high);
                                out.push(char::from_u32(value).unwrap_or('\u{FFFD}'));
                            }
                        }
                        continue;
                    }
                    _ => return Err(JsonFailure::At("Bad escaped character in JSON", self.pos)),
                };
                flush(&mut out, &mut high);
                out.push(decoded);
                self.pos += 1;
                continue;
            }
            if (c as u32) < 0x20 {
                return Err(JsonFailure::At("Bad control character in string literal in JSON", self.pos));
            }
            flush(&mut out, &mut high);
            out.push(c);
            self.pos += 1;
        }
    }

    fn skip_digits(&mut self) {
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.pos += 1;
        }
    }

    fn number(&mut self) -> Result<f64, JsonFailure> {
        let start = self.pos;
        if self.peek() == Some('-') {
            self.pos += 1;
        }
        match self.peek() {
            Some('0') => {
                self.pos += 1;
                if self.peek().is_some_and(|c| c.is_ascii_digit()) {
                    return Err(JsonFailure::At("Unexpected number in JSON", self.pos));
                }
            }
            Some('1'..='9') => self.skip_digits(),
            _ => return Err(JsonFailure::At("No number after minus sign in JSON", self.pos)),
        }
        if self.peek() == Some('.') {
            self.pos += 1;
            if !self.peek().is_some_and(|c| c.is_ascii_digit()) {
                return Err(JsonFailure::At("Unterminated fractional number in JSON", self.pos));
            }
            self.skip_digits();
        }
        if matches!(self.peek(), Some('e' | 'E')) {
            self.pos += 1;
            if matches!(self.peek(), Some('-' | '+')) {
                self.pos += 1;
            }
            if !self.peek().is_some_and(|c| c.is_ascii_digit()) {
                return Err(JsonFailure::At("Exponent part is missing a number in JSON", self.pos));
            }
            self.skip_digits();
        }
        let text: String = self.chars[start..self.pos].iter().collect();
        Ok(text.parse::<f64>().unwrap_or(f64::NAN))
    }

    /// `true`, `false` or `null`, its first letter already seen.
    fn literal(&mut self, word: &str) -> Result<(), JsonFailure> {
        let word: Vec<char> = word.chars().collect();
        let remaining = self.chars.len() - self.pos;
        if remaining >= word.len() && self.chars[self.pos + 1..self.pos + word.len()] == word[1..] {
            self.pos += word.len();
            return Ok(());
        }
        self.pos += 1;
        for expected in word[1..].iter().take(remaining - 1) {
            if self.chars[self.pos] != *expected {
                return Err(self.unexpected());
            }
            self.pos += 1;
        }
        Err(JsonFailure::End)
    }

    fn parse(&mut self) -> Result<Json, JsonFailure> {
        let mut stack: Vec<Frame> = Vec::new();
        loop {
            self.skip_whitespace();
            let mut value = match self.peek() {
                Some('"') => {
                    self.pos += 1;
                    Json::String(self.string()?)
                }
                Some('-' | '0'..='9') => Json::Number(self.number()?),
                Some('{') => {
                    self.pos += 1;
                    if self.check('}') {
                        Json::Object(JsonObject::default())
                    } else {
                        self.expect_next('"', "Expected property name or '}' in JSON")?;
                        let key = self.property()?;
                        stack.push(Frame::Object(JsonObject::default(), key));
                        continue;
                    }
                }
                Some('[') => {
                    self.pos += 1;
                    if self.check(']') {
                        Json::Array(Vec::new())
                    } else {
                        stack.push(Frame::Array(Vec::new()));
                        continue;
                    }
                }
                Some('t') => {
                    self.literal("true")?;
                    Json::Bool(true)
                }
                Some('f') => {
                    self.literal("false")?;
                    Json::Bool(false)
                }
                Some('n') => {
                    self.literal("null")?;
                    Json::Null
                }
                _ => return Err(self.unexpected()),
            };
            loop {
                let depth = stack.len();
                match stack.last_mut() {
                    None => {
                        self.skip_whitespace();
                        if self.pos < self.chars.len() {
                            return Err(JsonFailure::At("Unexpected non-whitespace character after JSON", self.pos));
                        }
                        return Ok(value);
                    }
                    Some(Frame::Object(object, key)) => {
                        object.insert(std::mem::take(key), if depth > MAX_DEPTH { Json::Null } else { value });
                        if self.check(',') {
                            self.expect_next('"', "Expected double-quoted property name in JSON")?;
                            *key = self.property()?;
                            break;
                        }
                        self.expect('}', "Expected ',' or '}' after property value in JSON")?;
                        let Some(Frame::Object(object, _)) = stack.pop() else { unreachable!() };
                        value = Json::Object(object);
                    }
                    Some(Frame::Array(items)) => {
                        items.push(if depth > MAX_DEPTH { Json::Null } else { value });
                        if self.check(',') {
                            break;
                        }
                        self.expect(']', "Expected ',' or ']' after array element in JSON")?;
                        let Some(Frame::Array(items)) = stack.pop() else { unreachable!() };
                        value = Json::Array(items);
                    }
                }
            }
        }
    }
}

pub(super) fn parse_json(text: &str) -> Result<Json, (Vec<char>, JsonFailure)> {
    let chars: Vec<char> = text.chars().collect();
    let result = JsonParser { chars: &chars, pos: 0 }.parse();
    result.map_err(|failure| (chars, failure))
}

/// V8's message without the text it quotes, which may hold a token: what the renderer shows.
pub(super) fn describe(chars: &[char], failure: JsonFailure) -> String {
    match failure {
        JsonFailure::End => "Unexpected end of JSON input".into(),
        JsonFailure::Token(at) => {
            // `JSON.parse` of these values as text says only that the text is not JSON.
            let source: String = chars.iter().collect();
            if matches!(source.as_str(), "[object Object]" | "undefined" | "Infinity" | "NaN") {
                String::new()
            } else {
                // V8 names one UTF-16 unit: of a character beyond the BMP only its first half, which shows as U+FFFD.
                format!("Unexpected token '{}'", if chars[at].len_utf16() > 1 { '\u{FFFD}' } else { chars[at] })
            }
        }
        JsonFailure::At(message, at) => {
            let (mut offset, mut line, mut line_start) = (0, 1, 0);
            let mut index = 0;
            while index < at {
                if chars[index] == '\r' && index + 1 < at && chars[index + 1] == '\n' {
                    offset += 1;
                    index += 1;
                }
                offset += chars[index].len_utf16();
                if matches!(chars[index], '\r' | '\n') {
                    line += 1;
                    line_start = offset;
                }
                index += 1;
            }
            format!("{message} at position {offset} (line {line} column {})", 1 + offset - line_start)
        }
    }
}
