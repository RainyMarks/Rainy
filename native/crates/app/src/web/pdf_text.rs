//! Make the text of a PDF that WebView2 printed copy and search as written (`src/main/pdf-text.ts`). Chinese fonts such as
//! Microsoft YaHei draw a Kangxi radical and its ideograph (⼆ U+2F06 and 二 U+4E8C) with one glyph, and Chromium's
//! ToUnicode maps then name the radical, so "第二节" copies as "第⼆节" and is not found by search. The maps are rewritten
//! to the ideographs, which Unicode's compatibility mapping (NFKC) gives, as an incremental update appended to the file:
//! nothing already in the file moves. A file this code does not understand is returned unchanged.

use std::collections::BTreeSet;
use std::io::{Read as _, Write as _};

use regex::bytes::Regex;

/// The NFKC mapping of the radicals that have one, `(radical, ideograph)` sorted; generated with Python's `unicodedata`.
const RADICALS: &[(u16, u16)] = &include!("radicals.in");

/// The ideograph a Kangxi radical or a CJK radical supplement character stands for.
fn ideograph_of(unit: u16) -> Option<u16> {
    RADICALS.binary_search_by_key(&unit, |(radical, _)| *radical).ok().map(|index| RADICALS[index].1)
}

/// UTF-16BE hex of a mapping's text with radicals replaced.
fn fix_destination(destination: &str) -> String {
    if destination.len() % 4 != 0 || !destination.is_ascii() {
        return destination.to_owned();
    }
    let mut fixed = String::with_capacity(destination.len());
    for index in (0..destination.len()).step_by(4) {
        match u16::from_str_radix(&destination[index..index + 4], 16) {
            Ok(unit) => fixed.push_str(&format!("{:04X}", ideograph_of(unit).unwrap_or(unit))),
            Err(_) => return destination.to_owned(),
        }
    }
    fixed
}

/// A ToUnicode CMap with every destination that names a radical changed to its ideograph, or `None` when none does.
pub fn fix_to_unicode(cmap: &str) -> Option<String> {
    let mut changed = false;
    let chars = regex::Regex::new(r"beginbfchar([\s\S]*?)endbfchar").ok()?;
    let pair = regex::Regex::new(r"<([0-9A-Fa-f]+)>\s*<([0-9A-Fa-f]*)>").ok()?;
    let fixed = chars.replace_all(cmap, |section: &regex::Captures| {
        let body = pair.replace_all(&section[1], |found: &regex::Captures| {
            let destination = found[2].to_uppercase();
            let next = fix_destination(&destination);
            if next == destination {
                return found[0].to_owned();
            }
            changed = true;
            format!("<{}> <{next}>", &found[1])
        });
        format!("beginbfchar{body}endbfchar")
    });
    let ranges = regex::Regex::new(r"beginbfrange([\s\S]*?)endbfrange").ok()?;
    let fixed = ranges.replace_all(&fixed, |section: &regex::Captures| match fix_ranges(&section[1]) {
        Some(body) => {
            changed = true;
            format!("beginbfrange{body}endbfrange")
        }
        None => section[0].to_owned(),
    });
    changed.then(|| fixed.into_owned())
}

/// The entries of a bfrange section (`<low> <high> <start>` or `<low> <high> [<d1> <d2> …]`) with radicals replaced, a
/// range that reaches one written out as a list; `None` when none does or the section is not understood.
fn fix_ranges(body: &str) -> Option<String> {
    let token = regex::Regex::new(r"<([0-9A-Fa-f]*)>|\[|\]").ok()?;
    let tokens: Vec<String> =
        token.captures_iter(body).map(|found| found.get(1).map_or_else(|| found[0].to_owned(), |hex| hex.as_str().to_owned())).collect();
    let bracket = |value: &str| value == "[" || value == "]";
    let mut entries = Vec::new();
    let mut changed = false;
    let mut index = 0;
    while index < tokens.len() {
        let (low, high, next) = (tokens.get(index)?, tokens.get(index + 1)?, tokens.get(index + 2)?);
        if bracket(low) || bracket(high) {
            return None;
        }
        if next == "[" {
            let close = tokens[index + 3..].iter().position(|value| value == "]")? + index + 3;
            let list = &tokens[index + 3..close];
            let fixed: Vec<String> = list.iter().map(|destination| fix_destination(&destination.to_uppercase())).collect();
            if fixed.iter().zip(list).any(|(value, original)| *value != original.to_uppercase()) {
                changed = true;
            }
            entries.push(format!("<{low}> <{high}> [{}]", fixed.iter().map(|value| format!("<{value}>")).collect::<Vec<_>>().join(" ")));
            index = close + 1;
            continue;
        }
        if next == "]" {
            return None;
        }
        let (Ok(low_value), Ok(high_value)) = (u32::from_str_radix(low, 16), u32::from_str_radix(high, 16)) else { return None };
        let count = high_value as i64 - low_value as i64 + 1;
        let first = u32::from_str_radix(next, 16).unwrap_or(0);
        let units: Vec<u16> = if next.len() == 4 && (1..=256).contains(&count) && (first & 0xff) as i64 + count <= 0x100 {
            (0..count as u32).map(|offset| (first + offset) as u16).collect()
        } else {
            Vec::new()
        };
        if units.iter().any(|unit| ideograph_of(*unit).is_some()) {
            // Consecutive codes map to consecutive characters; the radicals among them break the run, so each gets its own.
            changed = true;
            let list: Vec<String> = units.iter().map(|unit| format!("<{:04X}>", ideograph_of(*unit).unwrap_or(*unit))).collect();
            entries.push(format!("<{low}> <{high}> [{}]", list.join(" ")));
        } else {
            entries.push(format!("<{low}> <{high}> <{next}>"));
        }
        index += 3;
    }
    changed.then(|| format!("\n{}\n", entries.join("\n")))
}

/// The groups of a pattern's first match.
fn find(pattern: &str, haystack: &[u8]) -> Option<Vec<Vec<u8>>> {
    let found = Regex::new(pattern).ok()?.captures(haystack)?;
    Some(found.iter().map(|group| group.map(|group| group.as_bytes().to_vec()).unwrap_or_default()).collect())
}

fn number(bytes: &[u8]) -> Option<usize> {
    std::str::from_utf8(bytes).ok()?.trim().parse().ok()
}

/// Bytes as Latin-1 text, the way the PDF's own syntax reads; every byte survives the round trip.
fn latin(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| *byte as char).collect()
}

fn unlatin(text: &str) -> Vec<u8> {
    text.chars().map(|c| c as u32 as u8).collect()
}

fn rfind(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).rposition(|window| window == needle)
}

/// A PDF with a classic cross-reference table, as Chromium writes it, with its ToUnicode maps fixed by an incremental
/// update; the PDF itself when nothing needs fixing or the file is not one this code understands.
pub fn searchable_pdf(pdf: Vec<u8>) -> Vec<u8> {
    match update(&pdf) {
        Some(tail) => [pdf, tail].concat(),
        None => pdf,
    }
}

/// What `searchable_pdf` appends.
fn update(pdf: &[u8]) -> Option<Vec<u8>> {
    let end = &pdf[pdf.len().saturating_sub(64)..];
    let previous = number(&find(r"startxref\s+(\d+)\s+%%EOF\s*$", end)?[1])?;
    let trailer_at = rfind(pdf, b"trailer")?;
    let startxref_at = rfind(pdf, b"startxref")?;
    let trailer = pdf.get(trailer_at..startxref_at)?;
    let size = number(&find(r"/Size\s+(\d+)", trailer)?[1])?;
    let root = find(r"/Root\s+(\d+\s+\d+\s+R)", trailer)?.remove(1);
    if Regex::new(r"/Encrypt\b").ok()?.is_match(trailer) {
        return None;
    }
    let info = find(r"/Info\s+(\d+\s+\d+\s+R)", trailer).map(|mut found| found.remove(1));
    let id = find(r"/ID\s*(\[[^\]]*\])", trailer).map(|mut found| found.remove(1));
    let objects: BTreeSet<usize> = Regex::new(r"/ToUnicode\s+(\d+)\s+0\s+R").ok()?.captures_iter(pdf).filter_map(|found| number(&found[1])).collect();
    let flate = Regex::new(r"/Filter\s*/FlateDecode").ok()?;
    let parms = Regex::new(r"/DecodeParms").ok()?;
    let mut updates: Vec<(usize, Vec<u8>)> = Vec::new();
    for object in objects {
        let header = Regex::new(&format!(r"(?:^|[\r\n]){object}\s+0\s+obj\s*<<([^>]*)>>\s*stream(\r\n|\n)")).ok()?;
        let Some(found) = header.captures(pdf) else { continue };
        let dictionary = &found[1];
        // A direct length only: `/Length 12 0 R` points elsewhere.
        let Some(length) = find(r"/Length\s+(\d+)\s*(?:/|$)", dictionary).and_then(|found| number(&found[1])) else { continue };
        if !flate.is_match(dictionary) || parms.is_match(dictionary) {
            continue;
        }
        let start = found.get(0)?.end();
        let Some(compressed) = pdf.get(start..start + length) else { continue };
        let mut cmap = Vec::new();
        if flate2::read::ZlibDecoder::new(compressed).read_to_end(&mut cmap).is_err() {
            continue;
        }
        let Some(fixed) = fix_to_unicode(&latin(&cmap)) else { continue };
        let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(&unlatin(&fixed)).ok()?;
        let stream = encoder.finish().ok()?;
        let mut body = format!("{object} 0 obj\n<</Filter /FlateDecode /Length {}>>\nstream\n", stream.len()).into_bytes();
        body.extend_from_slice(&stream);
        body.extend_from_slice(b"\nendstream\nendobj\n");
        updates.push((object, body));
    }
    if updates.is_empty() {
        return None;
    }
    let mut out = Vec::new();
    let mut offset = pdf.len();
    if !pdf.ends_with(b"\n") {
        out.push(b'\n');
        offset += 1;
    }
    // The head of the free list again, as most writers repeat it: some readers take a section that does not start at
    // object 0 for a misnumbered table and renumber its objects.
    let mut table = String::from("xref\n0 1\n0000000000 65535 f \n");
    for (object, body) in &updates {
        out.extend_from_slice(body);
        // Each entry is exactly 20 bytes: offset, generation, kind, space and line feed.
        table.push_str(&format!("{object} 1\n{offset:010} 00000 n \n"));
        offset += body.len();
    }
    let mut trailer = format!("trailer\n<</Size {size} /Root {}", latin(&root));
    if let Some(info) = info {
        trailer.push_str(&format!(" /Info {}", latin(&info)));
    }
    if let Some(id) = id {
        trailer.push_str(&format!(" /ID {}", latin(&id)));
    }
    trailer.push_str(&format!(" /Prev {previous}>>\n"));
    out.extend(unlatin(&format!("{table}{trailer}startxref\n{offset}\n%%EOF\n")));
    Some(out)
}

#[cfg(test)]
mod tests {
    use ::core::prelude::v1::test;

    use super::*;

    #[test]
    fn maps_radicals_to_ideographs() {
        assert_eq!(ideograph_of(0x2F06), Some(0x4E8C));
        assert_eq!(ideograph_of(0x2E9F), Some(0x6BCD));
        assert_eq!(ideograph_of(0x4E8C), None);
        assert_eq!(fix_destination("2F064E00"), "4E8C4E00");
        assert!(RADICALS.windows(2).all(|pair| pair[0].0 < pair[1].0));
    }

    #[test]
    fn rewrites_chars_and_ranges() {
        let cmap = "beginbfchar\n<0001> <2F06>\n<0002> <0041>\nendbfchar\nbeginbfrange\n<0010> <0011> <2F05>\nendbfrange";
        let fixed = fix_to_unicode(cmap).unwrap();
        assert!(fixed.contains("<0001> <4E8C>"));
        assert!(fixed.contains("<0002> <0041>"));
        assert!(fixed.contains("<0010> <0011> [<4E85> <4E8C>]"));
        assert_eq!(fix_to_unicode("beginbfchar\n<0002> <0041>\nendbfchar"), None);
    }

    #[test]
    fn appends_an_incremental_update() {
        let cmap = "beginbfchar\n<0001> <2F06>\nendbfchar";
        let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(cmap.as_bytes()).unwrap();
        let stream = encoder.finish().unwrap();
        let mut pdf = b"%PDF-1.4\n1 0 obj\n<</Type /Catalog>>\nendobj\n2 0 obj\n<</ToUnicode 3 0 R>>\nendobj\n".to_vec();
        pdf.extend(format!("3 0 obj\n<</Filter /FlateDecode /Length {}>>\nstream\n", stream.len()).bytes());
        pdf.extend(&stream);
        pdf.extend(b"\nendstream\nendobj\n");
        let xref = pdf.len();
        pdf.extend(format!("xref\n0 4\n0000000000 65535 f \ntrailer\n<</Size 4 /Root 1 0 R>>\nstartxref\n{xref}\n%%EOF\n").bytes());
        let fixed = searchable_pdf(pdf.clone());
        assert!(fixed.len() > pdf.len());
        assert!(fixed.starts_with(&pdf));
        let tail = latin(&fixed[pdf.len()..]);
        assert!(tail.contains(&format!("/Prev {xref}")));
        assert!(tail.ends_with("%%EOF\n"));
        assert_eq!(searchable_pdf(b"not a pdf".to_vec()), b"not a pdf".to_vec());
    }
}
