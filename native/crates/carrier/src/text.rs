//! Small conversions shared by the carrier and the interface.

/// A NUL-terminated UTF-16 string for a Windows API.
pub fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain([0]).collect()
}

/// Percent-encode a URL query value: everything but RFC 3986's unreserved characters.
pub fn percent_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// Lowercase hexadecimal.
pub fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    bytes.iter().flat_map(|byte| [DIGITS[usize::from(byte >> 4)], DIGITS[usize::from(byte & 15)]]).map(char::from).collect()
}

/// Milliseconds since 1970, as records and the Host store them.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_millis() as u64)
}

/// Whether `text` is exactly `length` lowercase ASCII hexadecimal digits.
pub(crate) fn is_hex(text: &str, length: usize) -> bool {
    text.len() == length && text.bytes().all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

/// Length of `text` in UTF-16 code units (as JavaScript `string.length` counts it).
pub(crate) fn utf16_len(text: &str) -> usize {
    text.chars().map(char::len_utf16).sum()
}

/// Non-recursive wildcard match (`*`, and optionally `?`), O(n*m) and immune to backtracking.
pub(crate) fn wildcard_match(pattern: &str, text: &str, case_insensitive: bool, allow_question: bool) -> bool {
    let pat: Vec<char> = pattern.chars().collect();
    let txt: Vec<char> = text.chars().collect();
    let (mut p, mut t) = (0usize, 0usize);
    let mut star_p: Option<usize> = None;
    let mut star_t = 0usize;
    let eq = |a: char, b: char| if case_insensitive { a.eq_ignore_ascii_case(&b) } else { a == b };
    while t < txt.len() {
        if p < pat.len() && ((allow_question && pat[p] == '?') || eq(pat[p], txt[t])) {
            p += 1;
            t += 1;
        } else if p < pat.len() && pat[p] == '*' {
            star_p = Some(p);
            p += 1;
            star_t = t;
        } else if let Some(sp) = star_p {
            p = sp + 1;
            star_t += 1;
            t = star_t;
        } else {
            return false;
        }
    }
    while p < pat.len() && pat[p] == '*' {
        p += 1;
    }
    p == pat.len()
}

/// Fill `bytes` from the operating system's cryptographic random generator.
pub(crate) fn fill_random(bytes: &mut [u8]) -> Result<(), ring::error::Unspecified> {
    use ring::rand::{SecureRandom as _, SystemRandom};
    SystemRandom::new().fill(bytes)
}

/// A random version-4 UUID (`xxxxxxxx-xxxx-4xxx-yxxx-xxxxxxxxxxxx`).
#[cfg(test)]
pub(crate) fn uuid_v4() -> std::io::Result<String> {
    let mut bytes = [0u8; 16];
    fill_random(&mut bytes).map_err(|error| std::io::Error::other(error.to_string()))?;
    Ok(format_uuid_v4(bytes))
}

/// A random version-4 UUID with a best-effort timestamp+PID fallback when system entropy fails.
pub(crate) fn uuid_v4_lossy() -> String {
    let mut bytes = [0u8; 16];
    if fill_random(&mut bytes).is_err() {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_nanos());
        bytes = (nanos ^ (u128::from(std::process::id()) << 96)).to_le_bytes();
    }
    format_uuid_v4(bytes)
}

fn format_uuid_v4(mut bytes: [u8; 16]) -> String {
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let h = hex(&bytes);
    format!("{}-{}-{}-{}-{}", &h[..8], &h[8..12], &h[12..16], &h[16..20], &h[20..])
}

/// Extract the quoted program path at the start of a Windows shell command (`"C:\...\app.exe" ...`).
pub(crate) fn quoted_executable(command: &str) -> Option<&str> {
    let rest = command.trim().strip_prefix('"')?;
    let (exe, _) = rest.split_once('"')?;
    (!exe.is_empty()).then_some(exe)
}

#[cfg(test)]
mod tests {
    use ::core::prelude::v1::test;

    use super::*;

    #[test]
    fn converts() {
        assert_eq!(wide("a雨"), [97, 0x96e8, 0]);
        assert_eq!(percent_encode("a b/雨-_.~"), "a%20b%2F%E9%9B%A8-_.~");
        assert_eq!(hex(&[0, 0xab, 0x10]), "00ab10");
        assert!(now_ms() > 1_700_000_000_000);
        assert!(is_hex("0123456789abcdef", 16));
        assert!(!is_hex("0123456789ABCDEF", 16));
        assert_eq!(utf16_len("a😀"), 3);
        assert!(wildcard_match("*.corp.*", "git.corp.example", true, false));
        assert!(wildcard_match("a?c", "abc", false, true));
        assert!(!wildcard_match("a?c", "abc", false, false));
        let u = uuid_v4().unwrap();
        assert_eq!((u.len(), &u[14..15], u.matches('-').count()), (36, "4", 4));
        assert_eq!(quoted_executable(r#""C:\Rainy\Rainy.exe" "%1""#), Some(r"C:\Rainy\Rainy.exe"));
    }
}
