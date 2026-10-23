//! Server-sent events: `event:` and `data:` lines, an empty line ending each event.

/// Splits a byte stream into events; a line may arrive in pieces and a character across chunks.
#[derive(Default)]
pub struct Events {
    buffer: Vec<u8>,
    name: String,
    data: Vec<String>,
}

impl Events {
    /// The events a chunk completes, as `(event name, data)`.
    pub fn push(&mut self, chunk: &[u8]) -> Vec<(String, String)> {
        self.buffer.extend_from_slice(chunk);
        let mut events = Vec::new();
        while let Some(end) = self.buffer.iter().position(|byte| *byte == b'\n' || *byte == b'\r') {
            // A CR LF pair is one line break; a lone CR at the end may still be followed by its LF.
            if self.buffer[end] == b'\r' && end + 1 == self.buffer.len() {
                break;
            }
            let line = String::from_utf8_lossy(&self.buffer[..end]).into_owned();
            let skip = if self.buffer[end] == b'\r' && self.buffer.get(end + 1) == Some(&b'\n') { 2 } else { 1 };
            self.buffer.drain(..end + skip);
            self.line(&line, &mut events);
        }
        events
    }

    /// The last event of a stream that ended without an empty line.
    pub fn finish(&mut self) -> Vec<(String, String)> {
        let mut events = Vec::new();
        if !self.buffer.is_empty() {
            let line = String::from_utf8_lossy(&std::mem::take(&mut self.buffer)).trim_end_matches('\r').to_owned();
            self.line(&line, &mut events);
        }
        self.line("", &mut events);
        events
    }

    fn line(&mut self, line: &str, events: &mut Vec<(String, String)>) {
        if line.is_empty() {
            if !self.data.is_empty() {
                events.push((std::mem::take(&mut self.name), self.data.join("\n")));
                self.data.clear();
            }
            self.name.clear();
            return;
        }
        if line.starts_with(':') {
            return;
        }
        let (field, value) = line.split_once(':').map_or((line, ""), |(field, value)| (field, value.strip_prefix(' ').unwrap_or(value)));
        match field {
            "event" => self.name = value.to_owned(),
            "data" => self.data.push(value.to_owned()),
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
}
