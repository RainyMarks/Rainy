//! Server-sent events: `event:` and `data:` lines, an empty line ending each event.

/// Hard ceiling on one buffered SSE line (16 MiB) and one accumulated event (32 MiB) to prevent OOM on malformed streams.
const MAX_LINE_BYTES: usize = 16 * 1024 * 1024;
const MAX_EVENT_BYTES: usize = 32 * 1024 * 1024;

/// Splits a byte stream into events; a line may arrive in pieces and a character across chunks.
#[derive(Default)]
pub struct Events {
    buffer: Vec<u8>,
    name: String,
    data: Vec<String>,
    data_bytes: usize,
    discarding_line: bool,
    discarding_event: bool,
    /// True right after consuming a trailing `\r` at a chunk or buffer slice boundary so a following `\n` is absorbed.
    pending_lf: bool,
}

impl Events {
    /// The events a chunk completes, as `(event name, data)`.
    pub fn push(&mut self, chunk: &[u8]) -> Vec<(String, String)> {
        let mut events = Vec::new();
        let mut offset = 0;
        while offset < chunk.len() {
            if self.pending_lf {
                self.pending_lf = false;
                if chunk[offset] == b'\n' {
                    offset += 1;
                    continue;
                }
            }
            if self.discarding_line {
                match chunk[offset..].iter().position(|byte| *byte == b'\n' || *byte == b'\r') {
                    Some(end) => {
                        let at = offset + end;
                        if chunk[at] == b'\r' {
                            self.pending_lf = true;
                        }
                        offset = at + 1;
                        self.discarding_line = false;
                    }
                    None => break,
                }
                continue;
            }
            let room = MAX_LINE_BYTES.saturating_sub(self.buffer.len());
            let take = (chunk.len() - offset).min(room.max(1));
            self.buffer.extend_from_slice(&chunk[offset..offset + take]);
            offset += take;
            while let Some(end) = self.buffer.iter().position(|byte| *byte == b'\n' || *byte == b'\r') {
                let is_cr = self.buffer[end] == b'\r';
                let line = String::from_utf8_lossy(&self.buffer[..end]).into_owned();
                let skip = if is_cr && self.buffer.get(end + 1) == Some(&b'\n') {
                    2
                } else {
                    if is_cr && end + 1 == self.buffer.len() {
                        self.pending_lf = true;
                    }
                    1
                };
                self.buffer.drain(..end + skip);
                self.line(&line, &mut events);
            }
            if self.buffer.len() >= MAX_LINE_BYTES {
                self.buffer.clear();
                self.discarding_line = true;
                self.discarding_event = true;
                self.data.clear();
                self.data_bytes = 0;
            }
        }
        events
    }

    /// The last event of a stream that ended without an empty line.
    pub fn finish(&mut self) -> Vec<(String, String)> {
        let mut events = Vec::new();
        if !self.discarding_line && !self.buffer.is_empty() {
            let line = String::from_utf8_lossy(&std::mem::take(&mut self.buffer)).trim_end_matches('\r').to_owned();
            self.line(&line, &mut events);
        }
        self.buffer.clear();
        self.discarding_line = false;
        self.pending_lf = false;
        self.line("", &mut events);
        events
    }

    fn line(&mut self, line: &str, events: &mut Vec<(String, String)>) {
        if line.is_empty() {
            if !self.discarding_event && !self.data.is_empty() {
                events.push((std::mem::take(&mut self.name), self.data.join("\n")));
            }
            self.data.clear();
            self.data_bytes = 0;
            self.name.clear();
            self.discarding_event = false;
            return;
        }
        if self.discarding_event || line.starts_with(':') {
            return;
        }
        let (field, value) = line.split_once(':').map_or((line, ""), |(field, value)| (field, value.strip_prefix(' ').unwrap_or(value)));
        match field {
            "event" => self.name = value.to_owned(),
            "data" => {
                if self.data_bytes.saturating_add(value.len() + 1) > MAX_EVENT_BYTES {
                    self.discarding_event = true;
                    self.data.clear();
                    self.data_bytes = 0;
                    return;
                }
                self.data_bytes += value.len() + 1;
                self.data.push(value.to_owned());
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_events_across_chunks() {
        let mut events = Events::default();
        assert!(events.push(b"event: a\r\ndata: {\"x\"").is_empty());
        assert_eq!(events.push(b":1}\r\n\r\ndata: [DONE]\n\n"), vec![("a".to_owned(), "{\"x\":1}".to_owned()), (String::new(), "[DONE]".to_owned())]);
        assert!(events.push("data: 雨".as_bytes()).is_empty());
        assert_eq!(events.finish(), vec![(String::new(), "雨".to_owned())]);
    }

    #[test]
    fn discarding_oversized_line_absorbs_crlf_split_across_chunks() {
        let mut events = Events::default();
        let mut oversized = b"data: ".to_vec();
        oversized.resize(MAX_LINE_BYTES + 16, b'x');
        oversized.push(b'\r');
        // `\r` ends chunk 1; `\n` starts chunk 2 and must NOT end the discarded event prematurely.
        assert!(events.push(&oversized).is_empty());
        assert!(events.push(b"\ndata: still-part-of-oversized-event\r\n\r\n").is_empty());
        assert_eq!(events.push(b"data: next-ok\r\n\r\n"), vec![(String::new(), "next-ok".to_owned())]);
    }

    #[test]
    fn max_line_boundary_does_not_split_crlf_into_blank_line() {
        let mut events = Events::default();
        // Line 1 ends with `\r` at exact byte index MAX_LINE_BYTES - 1, while its `\n` remains in `chunk[offset]`.
        let mut payload = b"data: ".to_vec();
        payload.resize(MAX_LINE_BYTES - 1, b'a');
        payload.extend_from_slice(b"\r\ndata: tail\r\n\r\n");
        let parsed = events.push(&payload);
        assert_eq!(parsed.len(), 1);
        assert!(parsed[0].1.ends_with("\ntail"));
        assert_eq!(parsed[0].1.len(), (MAX_LINE_BYTES - 1 - 6) + 1 + 4);
    }
}
