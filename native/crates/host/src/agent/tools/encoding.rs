//! Encodings the file tools read and keep (3.x `text-encoding.ts`): UTF-8, UTF-16 with a byte-order mark (what Windows
//! PowerShell 5.1 redirects to) and GB18030, the legacy code page of Chinese Windows. Text that is not UTF-8 is written
//! back in its own encoding only when its bytes convert back unchanged, so a change never damages the rest of a file.

/// A text encoding the file tools keep.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoding {
    Utf8,
    Utf16Le,
    Utf16Be,
    Gb18030,
}

impl Encoding {
    /// Its name as the model and the user know it.
    pub fn label(self) -> &'static str {
        match self {
            Self::Utf8 => "UTF-8",
            Self::Utf16Le => "UTF-16LE",
            Self::Utf16Be => "UTF-16BE",
            Self::Gb18030 => "GB18030 (GBK)",
        }
    }
}

/// A file's text and how it was stored.
#[derive(Clone, Debug, PartialEq)]
pub struct Decoded {
    /// Text without the byte-order mark.
    pub text: String,
    pub encoding: Encoding,
    pub bom: bool,
    /// UTF-8 with a few invalid bytes, shown replaced; writing it back would change them.
    pub damaged: bool,
}

/// Bytes searched for NUL to tell a binary file from text that is not UTF-16.
const SNIFF_BYTES: usize = 8192;

/// A file's text; nothing for binary content and bytes that are neither UTF-8, UTF-16 with a mark nor GB18030.
pub fn decode(bytes: &[u8]) -> Option<Decoded> {
    let utf16 = |body: &[u8], little: bool| -> String {
        let units: Vec<u16> = body.chunks_exact(2).map(|pair| if little { u16::from_le_bytes([pair[0], pair[1]]) } else { u16::from_be_bytes([pair[0], pair[1]]) }).collect();
        let mut text = String::from_utf16_lossy(&units);
        if body.len() % 2 == 1 {
            text.push('\u{fffd}');
        }
        text
    };
    if bytes.starts_with(&[0xff, 0xfe]) {
        return Some(Decoded { text: utf16(&bytes[2..], true), encoding: Encoding::Utf16Le, bom: true, damaged: false });
    }
    if bytes.starts_with(&[0xfe, 0xff]) {
        return Some(Decoded { text: utf16(&bytes[2..], false), encoding: Encoding::Utf16Be, bom: true, damaged: false });
    }
    if bytes[..bytes.len().min(SNIFF_BYTES)].contains(&0) {
        return None;
    }
    let bom = bytes.starts_with(&[0xef, 0xbb, 0xbf]);
    let body = if bom { &bytes[3..] } else { bytes };
    match std::str::from_utf8(body) {
        Ok(text) => return Some(Decoded { text: text.to_owned(), encoding: Encoding::Utf8, bom, damaged: false }),
        // A UTF-8 file with a stray invalid byte is not GB18030: reading it as such would store new text wrongly.
        Err(_) if bom || mostly_utf8(body) => return Some(Decoded { text: String::from_utf8_lossy(body).into_owned(), encoding: Encoding::Utf8, bom, damaged: true }),
        Err(_) => {}
    }
    let text = encoding_rs::GB18030.decode_without_bom_handling_and_without_replacement(bytes)?;
    Some(Decoded { text: text.into_owned(), encoding: Encoding::Gb18030, bom: false, damaged: false })
}

/// Whether nearly all non-ASCII bytes of text that is not valid UTF-8 still form UTF-8 sequences, as in a UTF-8 file
/// with a damaged byte. GBK text does not: only about one in six of its two-byte codes happens to.
fn mostly_utf8(bytes: &[u8]) -> bool {
    let (mut valid, mut invalid, mut index) = (0usize, 0usize, 0usize);
    while index < bytes.len() {
        let lead = bytes[index];
        if lead < 0x80 {
            index += 1;
            continue;
        }
        let length = match lead {
            0xc2..=0xdf => 2,
            0xe0..=0xef => 3,
            0xf0..=0xf4 => 4,
            _ => 0,
        };
        let ok = length > 0 && index + length <= bytes.len() && bytes[index + 1..index + length].iter().all(|byte| byte & 0xc0 == 0x80);
        if ok {
            valid += 1;
            index += length;
        } else {
            invalid += 1;
            index += 1;
        }
    }
    invalid * 4 < valid
}

/// Text in a file's encoding; nothing when the encoding cannot store some character (GBK has no four-byte codes here).
pub fn encode(encoding: Encoding, bom: bool, text: &str) -> Option<Vec<u8>> {
    match encoding {
        Encoding::Utf8 => {
            let mut bytes = if bom { vec![0xef, 0xbb, 0xbf] } else { Vec::new() };
            bytes.extend_from_slice(text.as_bytes());
            Some(bytes)
        }
        Encoding::Utf16Le => Some([0xff, 0xfe].into_iter().chain(text.encode_utf16().flat_map(u16::to_le_bytes)).collect()),
        Encoding::Utf16Be => Some([0xfe, 0xff].into_iter().chain(text.encode_utf16().flat_map(u16::to_be_bytes)).collect()),
        Encoding::Gb18030 => {
            let (bytes, _, unmappable) = encoding_rs::GBK.encode(text);
            (!unmappable).then(|| bytes.into_owned())
        }
    }
}

/// Whether a decoded file converts back to exactly its bytes; undamaged UTF-8 always does.
pub fn round_trips(bytes: &[u8], file: &Decoded) -> bool {
    if file.encoding == Encoding::Utf8 {
        return !file.damaged;
    }
    encode(file.encoding, file.bom, &file.text).is_some_and(|encoded| encoded == bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_encodings() {
        let gbk = encoding_rs::GBK.encode("中文注释").0.into_owned();
        let decoded = decode(&gbk).unwrap();
        assert_eq!((decoded.text.as_str(), decoded.encoding), ("中文注释", Encoding::Gb18030));
        assert!(round_trips(&gbk, &decoded));
        let utf16 = encode(Encoding::Utf16Le, true, "hi 雨").unwrap();
        assert_eq!(decode(&utf16).unwrap().text, "hi 雨");
        let damaged = [b"caf\xc3\xa9 \xc3\xa9\xc3\xa9\xc3\xa9\xc3\xa9 ".as_slice(), &[0xff], b" \xc3\xa9"].concat();
        assert!(decode(&damaged).unwrap().damaged);
        assert!(decode(b"a\0b").is_none());
        assert!(encode(Encoding::Gb18030, false, "😀").is_none());
    }
}
