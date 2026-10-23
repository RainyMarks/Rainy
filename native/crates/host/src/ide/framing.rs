//! `Content-Length` framing of JSON-RPC messages, shared by language servers (LSP) and debug adapters (DAP)
//! (3.x `ide/jsonrpc-framing.ts`).

use serde_json::Value;

/// A peer that never sends the separator cannot grow the header buffer past this size.
const MAX_HEADER_BYTES: usize = 1 << 16;

/// `Content-Length: N\r\n\r\n<UTF-8 JSON>`.
pub fn encode(message: &Value) -> Vec<u8> {
    let body = message.to_string();
    let mut framed = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    framed.extend_from_slice(body.as_bytes());
    framed
}

/// Yields each complete message body; headers other than `Content-Length` are ignored.
pub struct Decoder {
    buffer: Vec<u8>,
    max_message: usize,
}

impl Decoder {
    pub fn new(max_message: usize) -> Self {
        Self { buffer: Vec::new(), max_message }
    }

    /// Append received bytes; every message they complete, in order.
    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<Value>, String> {
        self.buffer.extend_from_slice(chunk);
        let mut messages = Vec::new();
        loop {
            let Some(separator) = self.buffer.windows(4).position(|window| window == b"\r\n\r\n") else {
                if self.buffer.len() > MAX_HEADER_BYTES {
                    return Err(format!("Message header exceeded {MAX_HEADER_BYTES} bytes without a terminator"));
                }
                return Ok(messages);
            };
            if separator > MAX_HEADER_BYTES {
                return Err(format!("Message header exceeded {MAX_HEADER_BYTES} bytes"));
            }
            let header = String::from_utf8_lossy(&self.buffer[..separator]).into_owned();
            let length = header
                .split("\r\n")
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.trim().eq_ignore_ascii_case("content-length").then(|| value.trim().parse::<usize>().map_err(|_| format!("invalid Content-Length header: {line:?}")))
                })
                .ok_or_else(|| format!("Message header block missing Content-Length: {header:?}"))??;
            if length > self.max_message {
                return Err(format!("Message length {length} exceeds the {}-byte limit", self.max_message));
            }
            let start = separator + 4;
            if self.buffer.len() < start + length {
                return Ok(messages);
            }
            let body: Vec<u8> = self.buffer[start..start + length].to_vec();
            self.buffer.drain(..start + length);
            messages.push(serde_json::from_slice(&body).map_err(|error| format!("Message body was not valid JSON: {error}"))?);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn frames_and_reads_split_messages() {
        let mut framed = encode(&json!({ "a": "雨" }));
        framed.extend(encode(&json!(2)));
        let mut decoder = Decoder::new(1024);
        let (first, second) = framed.split_at(10);
        assert!(decoder.push(first).unwrap().is_empty());
        assert_eq!(decoder.push(second).unwrap(), vec![json!({ "a": "雨" }), json!(2)]);
        assert!(Decoder::new(4).push(&encode(&json!("long text"))).is_err());
        assert!(Decoder::new(64).push(b"X: 1\r\n\r\n").is_err());
    }
}
