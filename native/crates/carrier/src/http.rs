//! The loopback HTTP/1.1 answers the carrier reads itself, from the Host (its launch redirect and file bytes) and from
//! Strata's status page: read until the server closes the connection, a chunked body joined.

use anyhow::{Result, bail};

/// One answer: its status, its headers with lowercased names, and its body.
pub(crate) struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

/// Parse a whole answer; anything that is not one fails with `invalid()`. The body is never larger than `raw`.
pub(crate) fn parse(raw: &[u8], invalid: impl Fn() -> String) -> Result<Response> {
    let Some(split) = raw.windows(4).position(|window| window == b"\r\n\r\n") else { bail!(invalid()) };
    let head = String::from_utf8_lossy(&raw[..split]);
    let mut lines = head.split("\r\n");
    let Some(status) = lines.next().and_then(|line| line.split_whitespace().nth(1)).and_then(|code| code.parse().ok()) else { bail!(invalid()) };
    let headers: Vec<(String, String)> = lines
        .filter_map(|line| line.split_once(':').map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_owned())))
        .collect();
    let mut body = raw[split + 4..].to_vec();
    if headers.iter().any(|(name, value)| name == "transfer-encoding" && value.to_ascii_lowercase().contains("chunked")) {
        let Some(joined) = dechunk(&body) else { bail!(invalid()) };
        body = joined;
    }
    Ok(Response { status, headers, body })
}

/// The body of a chunked transfer, or `None` when it is malformed or cut short.
fn dechunk(mut data: &[u8]) -> Option<Vec<u8>> {
    let mut body = Vec::new();
    loop {
        let line = data.windows(2).position(|window| window == b"\r\n")?;
        let size = usize::from_str_radix(std::str::from_utf8(&data[..line]).ok()?.split(';').next()?.trim(), 16).ok()?;
        data = &data[line + 2..];
        if size == 0 {
            return Some(body);
        }
        // Compared without adding to it: a size near usize::MAX must not wrap around.
        if size > data.len() {
            return None;
        }
        body.extend_from_slice(&data[..size]);
        data = data.get(size + 2..).unwrap_or_default();
    }
}

#[cfg(test)]
mod tests {
    use ::core::prelude::v1::test;

    use super::*;

    #[test]
    fn reads_plain_and_chunked_answers() {
        let invalid = || "invalid".to_owned();
        let plain = parse(b"HTTP/1.1 302 Found\r\nSet-Cookie: a=b; HttpOnly\r\nLocation: /\r\n\r\n", invalid).unwrap();
        assert_eq!(plain.status, 302);
        assert_eq!(plain.headers[0], ("set-cookie".to_owned(), "a=b; HttpOnly".to_owned()));
        assert!(plain.body.is_empty());
        let chunked = parse(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n2;x=1\r\nde\r\n0\r\n\r\n", invalid).unwrap();
        assert_eq!(chunked.body, b"abcde");
        for bad in [&b"HTTP/1.1 200 OK\r\n"[..], b"garbage\r\n\r\n", b"HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\n5\r\nab"] {
            assert_eq!(parse(bad, invalid).err().map(|error| error.to_string()).as_deref(), Some("invalid"));
        }
        // A size that would wrap around when added to is refused, not sliced.
        assert!(dechunk(b"ffffffffffffffff\r\nab\r\n0\r\n\r\n").is_none());
        assert!(dechunk(b"zz\r\n").is_none());
    }
}
