//! Small conversions the Host shares.

use std::io;
use std::path::{Path, PathBuf};

/// Milliseconds since 1970.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_millis() as u64)
}

/// Lowercase hexadecimal.
pub fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    bytes.iter().flat_map(|byte| [DIGITS[usize::from(byte >> 4)], DIGITS[usize::from(byte & 15)]]).map(char::from).collect()
}

/// SHA-256 of text, in hexadecimal.
pub fn sha256_hex(data: impl AsRef<[u8]>) -> String {
    hex(ring::digest::digest(&ring::digest::SHA256, data.as_ref()).as_ref())
}

/// Fill `bytes` from the operating system's cryptographic random generator.
pub fn fill_random(bytes: &mut [u8]) {
    use ring::rand::{SecureRandom as _, SystemRandom};
    SystemRandom::new().fill(bytes).expect("random bytes");
}

/// Cryptographically random URL-safe base64 token (256 bits).
pub fn random_token() -> String {
    use base64::Engine as _;
    let mut bytes = [0u8; 32];
    fill_random(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// Constant-time comparison for secret tokens of matching length.
pub fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    left.len() == right.len() && left.iter().zip(right).fold(0u8, |acc, (a, b)| acc | (a ^ b)) == 0
}

/// The ISO 8601 UTC time of milliseconds since 1970, as JavaScript's `toISOString` writes it.
pub fn iso_time(ms: u64) -> String {
    let seconds = ms / 1000;
    let (days, rest) = (seconds / 86_400, seconds % 86_400);
    let (hour, minute, second) = (rest / 3600, rest % 3600 / 60, rest % 60);
    // Civil date from days since 1970 (Howard Hinnant's algorithm).
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{:03}Z", ms % 1000)
}

/// Parse an RFC 3339 timestamp (`YYYY-MM-DDTHH:MM:SS[.frac](Z|+HH:MM|-HH:MM)`) into Unix seconds.
pub fn parse_rfc3339_secs(text: &str) -> Option<i64> {
    parse_rfc3339_parts(text).map(|(secs, _)| secs)
}

/// Parse an RFC 3339 timestamp into Unix milliseconds.
pub fn parse_rfc3339_ms(text: &str) -> Option<f64> {
    parse_rfc3339_parts(text).map(|(secs, millis)| (secs as f64) * 1000.0 + f64::from(millis))
}

fn parse_rfc3339_parts(text: &str) -> Option<(i64, u32)> {
    let raw = text.trim().as_bytes();
    if raw.len() < 20 {
        return None;
    }
    if raw[4] != b'-' || raw[7] != b'-' || !matches!(raw[10], b'T' | b't' | b' ') || raw[13] != b':' || raw[16] != b':' {
        return None;
    }
    let digits = |slice: &[u8]| -> Option<u32> {
        if slice.is_empty() || !slice.iter().all(u8::is_ascii_digit) {
            return None;
        }
        std::str::from_utf8(slice).ok()?.parse::<u32>().ok()
    };
    let year = i64::from(digits(&raw[0..4])?);
    let month = digits(&raw[5..7])?;
    let day = digits(&raw[8..10])?;
    let hour = digits(&raw[11..13])?;
    let minute = digits(&raw[14..16])?;
    let second = digits(&raw[17..19])?;
    if !(1..=12).contains(&month) || hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    let max_day = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return None,
    };
    if day == 0 || day > max_day {
        return None;
    }
    let mut index = 19;
    let mut millis = 0u32;
    if index < raw.len() && raw[index] == b'.' {
        index += 1;
        let frac_start = index;
        while index < raw.len() && raw[index].is_ascii_digit() {
            index += 1;
        }
        if index == frac_start {
            return None;
        }
        let frac = &raw[frac_start..index];
        let mut scale = 100u32;
        for &byte in frac.iter().take(3) {
            millis += u32::from(byte - b'0') * scale;
            scale /= 10;
        }
    }
    let tz_rest = &raw[index..];
    let offset_secs: i64 = match tz_rest {
        [b'Z'] | [b'z'] => 0,
        [sign @ (b'+' | b'-'), h1, h2, b':', m1, m2] => {
            let tz_h = digits(&[*h1, *h2])?;
            let tz_m = digits(&[*m1, *m2])?;
            if tz_h > 23 || tz_m > 59 {
                return None;
            }
            let total = i64::from(tz_h) * 3600 + i64::from(tz_m) * 60;
            if *sign == b'+' { total } else { -total }
        }
        [sign @ (b'+' | b'-'), h1, h2, m1, m2] => {
            let tz_h = digits(&[*h1, *h2])?;
            let tz_m = digits(&[*m1, *m2])?;
            if tz_h > 23 || tz_m > 59 {
                return None;
            }
            let total = i64::from(tz_h) * 3600 + i64::from(tz_m) * 60;
            if *sign == b'+' { total } else { -total }
        }
        _ => return None,
    };
    // Days since 1970-01-01 (Howard Hinnant's civil-from-days inverse).
    let y = year - i64::from(month <= 2);
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = i64::from(month);
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let secs = days * 86_400 + i64::from(hour) * 3600 + i64::from(minute) * 60 + i64::from(second.min(59)) - offset_secs;
    Some((secs, millis))
}

/// At most `limit` characters of `text`, cut at a character boundary.
pub fn truncate_chars(text: &str, limit: usize) -> &str {
    match text.char_indices().nth(limit) {
        Some((index, _)) => &text[..index],
        None => text,
    }
}

/// At most `max_bytes` UTF-8 bytes of `text`, cut at a valid character boundary.
#[allow(dead_code)]
pub fn truncate_bytes(text: &str, max_bytes: usize) -> &str {
    if text.len() <= max_bytes {
        return text;
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// At most `max_chars` characters; appends `…` when truncated.
pub fn clip_chars(text: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    if text.chars().nth(max_chars).is_none() {
        return text.to_owned();
    }
    let kept = truncate_chars(text, max_chars.saturating_sub(1));
    format!("{kept}…")
}

/// Zero-allocation writer that counts serialized bytes and aborts once `limit` is exceeded.
pub struct CountingWriter {
    written: usize,
    limit: usize,
}

impl CountingWriter {
    pub fn new(limit: usize) -> Self {
        Self { written: 0, limit }
    }

    pub fn written(&self) -> usize {
        self.written
    }
}

impl io::Write for CountingWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let next = self.written.saturating_add(buf.len());
        if next > self.limit {
            return Err(io::Error::new(io::ErrorKind::WriteZero, "byte limit exceeded"));
        }
        self.written = next;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Measure serialized JSON byte length without allocating a String; returns `Ok(None)` when it exceeds `limit`.
pub fn json_byte_len_capped(value: &impl serde::Serialize, limit: usize) -> Result<Option<usize>, serde_json::Error> {
    let mut counter = CountingWriter::new(limit);
    match serde_json::to_writer(&mut counter, value) {
        Ok(()) => Ok(Some(counter.written())),
        Err(error) if error.io_error_kind() == Some(io::ErrorKind::WriteZero) => Ok(None),
        Err(error) => Err(error),
    }
}

/// Move a corrupt persisted file aside as `<name>.damaged-<ms>` so startup recovers without losing user data.
pub fn backup_damaged(path: &Path) -> Option<PathBuf> {
    let name = path.file_name()?.to_string_lossy();
    let backup = path.with_file_name(format!("{name}.damaged-{}", now_ms()));
    if std::fs::rename(path, &backup).is_ok() {
        return Some(backup);
    }
    if std::fs::copy(path, &backup).is_ok() {
        let _ = std::fs::remove_file(path);
        return Some(backup);
    }
    None
}

/// The length JavaScript gives a string, in UTF-16 units.
pub fn js_length(text: &str) -> usize {
    text.encode_utf16().count()
}

/// JavaScript's `encodeURIComponent`.
pub fn encode_uri_component(text: &str) -> String {
    let mut encoded = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&byte) {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_iso_times() {
        assert_eq!(iso_time(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(iso_time(1_792_473_414_685), "2026-10-20T05:16:54.685Z");
        assert_eq!(truncate_chars("雨天abc", 2), "雨天");
    }

    #[test]
    fn truncates_and_clips_on_utf8_boundaries() {
        assert_eq!(truncate_bytes("雨天abc", 4), "雨");
        assert_eq!(truncate_bytes("雨天abc", 6), "雨天");
        assert_eq!(truncate_bytes("雨天abc", 100), "雨天abc");
        assert_eq!(clip_chars("雨天abc", 4), "雨天a…");
        assert_eq!(clip_chars("雨天abc", 5), "雨天abc");
    }

    #[test]
    fn parses_rfc3339_and_rejects_malformed() {
        assert_eq!(parse_rfc3339_secs("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_rfc3339_ms("2026-10-20T05:16:54.685Z"), Some(1_792_473_414_685.0));
        assert_eq!(parse_rfc3339_secs("2026-10-20T13:16:54+08:00"), Some(1_792_473_414));
        assert_eq!(parse_rfc3339_secs("2026-02-29T00:00:00Z"), None);
        assert_eq!(parse_rfc3339_secs("2024-02-29T00:00:00Z"), Some(1_709_164_800));
        assert_eq!(parse_rfc3339_secs("2026-10-20T-1:00:00Z"), None);
        assert_eq!(parse_rfc3339_secs("2026-10-20T24:00:00Z"), None);
    }

    #[test]
    fn counts_json_bytes_with_early_stop() {
        let payload = serde_json::json!({ "text": "hello world" });
        let exact = payload.to_string().len();
        assert_eq!(json_byte_len_capped(&payload, exact).unwrap(), Some(exact));
        assert_eq!(json_byte_len_capped(&payload, exact - 1).unwrap(), None);
    }
}
