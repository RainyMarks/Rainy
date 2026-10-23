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
    }
}
