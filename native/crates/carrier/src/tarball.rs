//! Unpacking the `.tar.gz` archives of optional modules (3.x used node-tar): ustar headers, pax `path`, `linkpath` and
//! `size` records and GNU long names. Only regular files and folders are created, each below the destination; absolute
//! paths, drive letters, `..`, links, devices and other special entries are refused.

use std::fmt;
use std::fs::{self, File};
use std::io::{self, BufReader, Read, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use flate2::read::MultiGzDecoder;

const BLOCK: usize = 512;
/// The largest pax record set or GNU long name read into memory.
const MAX_META: u64 = 1 << 20;
const CHUNK: usize = 256 * 1024;

#[derive(Debug)]
pub enum TarError {
    /// Not a tar archive, or a damaged one.
    Invalid(String),
    /// An entry Rainy does not unpack: its name and why.
    Refused(String),
    Cancelled,
    Io(io::Error),
}

impl fmt::Display for TarError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(detail) => write!(f, "The archive is damaged: {detail}"),
            Self::Refused(detail) => write!(f, "The archive contains an entry Rainy does not unpack: {detail}"),
            Self::Cancelled => f.write_str("Unpacking was cancelled."),
            Self::Io(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for TarError {}

impl From<io::Error> for TarError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// What an archive held.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Unpacked {
    pub files: u64,
    pub bytes: u64,
}

/// Unpack a gzip-compressed tar archive into `destination`, an existing folder.
pub fn unpack_gz(archive: &Path, destination: &Path, cancel: &AtomicBool) -> Result<Unpacked, TarError> {
    let file = File::open(archive)?;
    unpack(MultiGzDecoder::new(BufReader::new(file)), destination, cancel)
}

/// Names and sizes from metadata entries, for the entry that follows them.
#[derive(Default)]
struct Pending {
    path: Option<String>,
    link: Option<String>,
    size: Option<u64>,
}

impl Pending {
    /// Read pax records, `<length> <key>=<value>\n` each.
    fn read_pax(&mut self, data: &[u8]) -> Result<(), TarError> {
        let invalid = || TarError::Invalid("a pax header is malformed".into());
        let mut rest = data;
        while !rest.is_empty() {
            let space = rest.iter().position(|byte| *byte == b' ').ok_or_else(invalid)?;
            let length: usize = std::str::from_utf8(&rest[..space]).ok().and_then(|text| text.parse().ok()).ok_or_else(invalid)?;
            if length <= space + 1 || length > rest.len() || rest[length - 1] != b'\n' {
                return Err(invalid());
            }
            let record = &rest[space + 1..length - 1];
            let equals = record.iter().position(|byte| *byte == b'=').ok_or_else(invalid)?;
            let (key, value) = (&record[..equals], &record[equals + 1..]);
            let text = || String::from_utf8(value.to_vec()).map_err(|_| TarError::Invalid("a pax path is not UTF-8".into()));
            match key {
                b"path" => self.path = Some(text()?),
                b"linkpath" => self.link = Some(text()?),
                b"size" => self.size = Some(text()?.parse().map_err(|_| invalid())?),
                _ => {}
            }
            rest = &rest[length..];
        }
        Ok(())
    }
}

/// Unpack an uncompressed tar stream into `destination`, an existing folder that holds nothing but what this creates.
pub fn unpack(mut reader: impl Read, destination: &Path, cancel: &AtomicBool) -> Result<Unpacked, TarError> {
    let mut unpacked = Unpacked::default();
    let mut pending = Pending::default();
    let mut header = [0u8; BLOCK];
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(TarError::Cancelled);
        }
        if !read_block(&mut reader, &mut header)? {
            return Err(TarError::Invalid("the archive ends without its end marker".into()));
        }
        if header.iter().all(|byte| *byte == 0) {
            return Ok(unpacked);
        }
        check_sum(&header)?;
        let size = number(&header[124..136])?;
        match header[156] {
            b'x' => pending.read_pax(&read_meta(&mut reader, size)?)?,
            b'g' => skip(&mut reader, padded(size))?,
            b'L' => pending.path = Some(meta_name(&read_meta(&mut reader, size)?)?),
            b'K' => pending.link = Some(meta_name(&read_meta(&mut reader, size)?)?),
            kind => {
                let name = match pending.path.take() {
                    Some(name) => name,
                    None => header_name(&header)?,
                };
                let size = pending.size.take().unwrap_or(size);
                let link = pending.link.take();
                match kind {
                    b'0' | b'\0' | b'7' | b'5' => {}
                    b'1' | b'2' => return Err(TarError::Refused(format!("{name} is a link to {}", link.unwrap_or_else(|| link_name(&header))))),
                    b'3' | b'4' | b'6' => return Err(TarError::Refused(format!("{name} is a device or pipe"))),
                    other => return Err(TarError::Refused(format!("{name} has the entry type {}", char::from(other).escape_default()))),
                }
                let folder = kind == b'5' || (kind != b'7' && name.ends_with('/'));
                let path = relative(&name)?.map(|relative| destination.join(relative));
                match path {
                    Some(path) if folder => {
                        fs::create_dir_all(&path)?;
                        skip(&mut reader, padded(size))?;
                    }
                    Some(path) => {
                        if let Some(parent) = path.parent() {
                            fs::create_dir_all(parent)?;
                        }
                        copy(&mut reader, &path, size, cancel)?;
                        skip(&mut reader, padded(size) - size)?;
                        unpacked.files += 1;
                        unpacked.bytes += size;
                    }
                    // The archive's own root, `./`.
                    None => skip(&mut reader, padded(size))?,
                }
            }
        }
    }
}

/// One header block; `false` at the end of the stream.
fn read_block(reader: &mut impl Read, block: &mut [u8; BLOCK]) -> Result<bool, TarError> {
    let mut filled = 0;
    while filled < BLOCK {
        match reader.read(&mut block[filled..]) {
            Ok(0) if filled == 0 => return Ok(false),
            Ok(0) => return Err(TarError::Invalid("the archive ends inside a header".into())),
            Ok(read) => filled += read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(true)
}

/// `size` rounded up to whole blocks.
fn padded(size: u64) -> u64 {
    size.div_ceil(BLOCK as u64) * BLOCK as u64
}

fn skip(reader: &mut impl Read, bytes: u64) -> Result<(), TarError> {
    if io::copy(&mut reader.take(bytes), &mut io::sink())? != bytes {
        return Err(TarError::Invalid("the archive ends inside an entry".into()));
    }
    Ok(())
}

fn read_meta(reader: &mut impl Read, size: u64) -> Result<Vec<u8>, TarError> {
    if size > MAX_META {
        return Err(TarError::Invalid("a metadata entry is too large".into()));
    }
    let mut data = vec![0u8; size as usize];
    reader.read_exact(&mut data).map_err(|_| TarError::Invalid("the archive ends inside an entry".into()))?;
    skip(reader, padded(size) - size)?;
    Ok(data)
}

/// A GNU long name: the text before the first NUL.
fn meta_name(data: &[u8]) -> Result<String, TarError> {
    let end = data.iter().position(|byte| *byte == 0).unwrap_or(data.len());
    String::from_utf8(data[..end].to_vec()).map_err(|_| TarError::Invalid("a long name is not UTF-8".into()))
}

/// A NUL-terminated header field.
fn field(bytes: &[u8]) -> &[u8] {
    &bytes[..bytes.iter().position(|byte| *byte == 0).unwrap_or(bytes.len())]
}

/// The entry's name: a POSIX ustar header joins its prefix and name; GNU and old headers have the name alone.
fn header_name(header: &[u8; BLOCK]) -> Result<String, TarError> {
    let name = field(&header[..100]);
    let prefix = if &header[257..263] == b"ustar\0" { field(&header[345..500]) } else { &[] };
    let joined = if prefix.is_empty() { name.to_vec() } else { [prefix, b"/", name].concat() };
    String::from_utf8(joined).map_err(|_| TarError::Invalid("an entry name is not UTF-8".into()))
}

fn link_name(header: &[u8; BLOCK]) -> String {
    String::from_utf8_lossy(field(&header[157..257])).into_owned()
}

/// An octal number field, or a GNU base-256 one (high bit set).
fn number(bytes: &[u8]) -> Result<u64, TarError> {
    let invalid = || TarError::Invalid("a header number is malformed".into());
    if bytes[0] & 0x80 != 0 {
        if bytes[0] == 0xff {
            return Err(invalid());
        }
        return bytes[1..].iter().try_fold(u64::from(bytes[0] & 0x7f), |value, byte| value.checked_mul(256)?.checked_add(u64::from(*byte))).ok_or_else(invalid);
    }
    let digits = bytes.iter().skip_while(|byte| **byte == b' ').take_while(|byte| **byte != b' ' && **byte != 0);
    digits.into_iter().try_fold(0u64, |value, byte| match byte {
        b'0'..=b'7' => value.checked_mul(8)?.checked_add(u64::from(byte - b'0')),
        _ => None,
    })
    .ok_or_else(invalid)
}

/// The header checksum: the byte sum with the checksum field read as spaces, unsigned or (old tars) signed.
fn check_sum(header: &[u8; BLOCK]) -> Result<(), TarError> {
    let stored = number(&header[148..156])?;
    let spaces = 8 * u64::from(b' ');
    let outside = |range: std::ops::Range<usize>| header[range].iter().map(|byte| u64::from(*byte)).sum::<u64>();
    let unsigned = outside(0..148) + spaces + outside(156..BLOCK);
    let signed_outside = |range: std::ops::Range<usize>| header[range].iter().map(|byte| i64::from(*byte as i8)).sum::<i64>();
    let signed = signed_outside(0..148) + spaces as i64 + signed_outside(156..BLOCK);
    if stored == unsigned || i64::try_from(stored).is_ok_and(|stored| stored == signed) {
        Ok(())
    } else {
        Err(TarError::Invalid("a header checksum does not match".into()))
    }
}

/// The entry's path below the destination; `None` for the archive's root. Absolute paths, drive letters and `..` are
/// refused, as are names Windows cannot hold or would read differently.
fn relative(name: &str) -> Result<Option<PathBuf>, TarError> {
    let refused = |why: &str| TarError::Refused(format!("{name} {why}"));
    if name.starts_with(['/', '\\']) || name.contains(':') {
        return Err(refused("is an absolute path"));
    }
    let mut path = PathBuf::new();
    for part in name.split(['/', '\\']) {
        match part {
            "" | "." => {}
            ".." => return Err(refused("leaves the destination")),
            part if !portable(part) => return Err(refused("is not a valid Windows file name")),
            part => path.push(part),
        }
    }
    Ok((!path.as_os_str().is_empty()).then_some(path))
}

fn portable(part: &str) -> bool {
    const RESERVED: [&str; 22] = [
        "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8", "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5",
        "lpt6", "lpt7", "lpt8", "lpt9",
    ];
    let stem = part.split('.').next().unwrap_or_default().trim_end().to_ascii_lowercase();
    !part.chars().any(|c| c.is_control() || "<>\"|?*".contains(c)) && !part.ends_with(['.', ' ']) && !RESERVED.contains(&stem.as_str())
}

/// Write exactly `size` bytes of the stream to a new regular file at `path`.
fn copy(reader: &mut impl Read, path: &Path, size: u64, cancel: &AtomicBool) -> Result<(), TarError> {
    let mut output = fs::OpenOptions::new().write(true).create(true).truncate(true).open(path)?;
    let mut buffer = vec![0u8; CHUNK];
    let mut left = size;
    while left > 0 {
        if cancel.load(Ordering::Relaxed) {
            return Err(TarError::Cancelled);
        }
        let wanted = buffer.len().min(usize::try_from(left).unwrap_or(usize::MAX));
        let read = match reader.read(&mut buffer[..wanted]) {
            Ok(0) => return Err(TarError::Invalid("the archive ends inside a file".into())),
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error.into()),
        };
        output.write_all(&buffer[..read])?;
        left -= read as u64;
    }
    Ok(())
}

/// Archives for tests, written by hand: ustar entries, pax records, GNU long names.
#[cfg(test)]
pub(crate) mod build {
    use std::io::Write as _;

    use super::BLOCK;

    /// A ustar header; `prefix` fills the ustar prefix field.
    pub fn header(name: &str, prefix: &str, kind: u8, size: u64) -> [u8; BLOCK] {
        let mut header = [0u8; BLOCK];
        header[..name.len()].copy_from_slice(name.as_bytes());
        header[100..108].copy_from_slice(b"0000644\0");
        header[108..116].copy_from_slice(b"0000000\0");
        header[116..124].copy_from_slice(b"0000000\0");
        header[124..136].copy_from_slice(format!("{size:011o}\0").as_bytes());
        header[136..148].copy_from_slice(b"00000000000\0");
        header[156] = kind;
        header[257..263].copy_from_slice(b"ustar\0");
        header[263..265].copy_from_slice(b"00");
        header[345..345 + prefix.len()].copy_from_slice(prefix.as_bytes());
        seal(&mut header);
        header
    }

    /// Write the checksum of a header whose other fields are final.
    pub fn seal(header: &mut [u8; BLOCK]) {
        header[148..156].copy_from_slice(b"        ");
        let sum: u32 = header.iter().map(|byte| u32::from(*byte)).sum();
        header[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
    }

    /// A header and its data, padded to whole blocks.
    pub fn raw(out: &mut Vec<u8>, header: [u8; BLOCK], data: &[u8]) {
        out.extend_from_slice(&header);
        out.extend_from_slice(data);
        out.resize(out.len().div_ceil(BLOCK) * BLOCK, 0);
    }

    pub fn entry(out: &mut Vec<u8>, name: &str, kind: u8, data: &[u8]) {
        raw(out, header(name, "", kind, data.len() as u64), data);
    }

    pub fn file(out: &mut Vec<u8>, name: &str, data: &[u8]) {
        entry(out, name, b'0', data);
    }

    pub fn folder(out: &mut Vec<u8>, name: &str) {
        entry(out, name, b'5', b"");
    }

    /// A pax header with these records, for the entry that follows.
    pub fn pax(out: &mut Vec<u8>, records: &[(&str, &str)]) {
        let mut data = Vec::new();
        for (key, value) in records {
            let body = format!(" {key}={value}\n");
            let mut length = body.len() + 1;
            while format!("{length}").len() + body.len() != length {
                length += 1;
            }
            data.extend_from_slice(format!("{length}{body}").as_bytes());
        }
        entry(out, "PaxHeaders/x", b'x', &data);
    }

    /// A GNU long-name entry for the entry that follows.
    pub fn long_name(out: &mut Vec<u8>, name: &str) {
        let mut data = name.as_bytes().to_vec();
        data.push(0);
        let mut header = header("././@LongLink", "", b'L', data.len() as u64);
        header[257..265].copy_from_slice(b"ustar  \0");
        seal(&mut header);
        raw(out, header, &data);
    }

    /// The end marker: two zero blocks.
    pub fn finish(mut out: Vec<u8>) -> Vec<u8> {
        out.extend_from_slice(&[0u8; BLOCK * 2]);
        out
    }

    pub fn gzip(bytes: &[u8]) -> Vec<u8> {
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder.write_all(bytes).unwrap();
        encoder.finish().unwrap()
    }
}

#[cfg(test)]
mod tests {
    use ::core::prelude::v1::test;

    use super::build::*;
    use super::*;

    fn unpacked(archive: &[u8]) -> (tempfile::TempDir, Result<Unpacked, TarError>) {
        let dir = tempfile::tempdir().unwrap();
        let result = unpack(archive, dir.path(), &AtomicBool::new(false));
        (dir, result)
    }

    #[test]
    fn unpacks_files_folders_and_long_names() {
        let long = format!("pkg/{}/deep.txt", "d".repeat(150));
        let gnu = format!("pkg/{}.txt", "g".repeat(120));
        let mut tar = Vec::new();
        folder(&mut tar, "./");
        folder(&mut tar, "pkg/");
        file(&mut tar, "pkg/a.txt", b"alpha");
        file(&mut tar, "./pkg/ext/b.dll", &[7u8; 1300]);
        pax(&mut tar, &[("mtime", "1.5"), ("path", &long)]);
        file(&mut tar, "PaxHeaders/ignored", b"from pax");
        long_name(&mut tar, &gnu);
        file(&mut tar, "././@LongLink", b"from gnu");
        raw(&mut tar, header("c.txt", "pkg/prefixed", b'0', 3), b"pre");
        entry(&mut tar, "pkg/contiguous", b'7', b"seven");
        entry(&mut tar, "pkg/global", b'g', b"10 a=bcde\n");
        file(&mut tar, "pkg/empty", b"");
        let archive = gzip(&finish(tar));
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("module.tar.gz");
        fs::write(&path, &archive).unwrap();
        let destination = dir.path().join("out");
        fs::create_dir(&destination).unwrap();
        let unpacked = unpack_gz(&path, &destination, &AtomicBool::new(false)).unwrap();
        assert_eq!(unpacked, Unpacked { files: 7, bytes: 5 + 1300 + 8 + 8 + 3 + 5 });
        let read = |relative: &str| fs::read(destination.join(relative)).unwrap();
        assert_eq!(read("pkg/a.txt"), b"alpha");
        assert_eq!(read("pkg/ext/b.dll"), vec![7u8; 1300]);
        assert_eq!(read(&long), b"from pax");
        assert_eq!(read(&gnu), b"from gnu");
        assert_eq!(read("pkg/prefixed/c.txt"), b"pre");
        assert_eq!(read("pkg/contiguous"), b"seven");
        assert_eq!(read("pkg/empty"), b"");
        assert!(!destination.join("PaxHeaders").exists());
        assert!(!destination.join("././@LongLink").exists());
    }

    #[test]
    fn reads_large_and_pax_sizes() {
        assert_eq!(number(b"00000001750\0").unwrap(), 1000);
        assert_eq!(number(b"  1750 \0\0\0\0\0").unwrap(), 1000);
        assert_eq!(number(&[0x80, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x03, 0xe8]).unwrap(), 1000);
        assert!(number(&[0xff; 12]).is_err());
        assert!(number(b"0000000008\0\0").is_err());
        // A pax size overrides the header's.
        let mut tar = Vec::new();
        pax(&mut tar, &[("size", "4")]);
        raw(&mut tar, header("sized", "", b'0', 0), b"four");
        let (dir, result) = unpacked(&finish(tar));
        assert_eq!(result.unwrap().bytes, 4);
        assert_eq!(fs::read(dir.path().join("sized")).unwrap(), b"four");
    }

    #[test]
    fn refuses_entries_outside_the_destination() {
        for name in ["/etc/passwd", "\\windows\\x", "C:/x", "C:x", "a/../../b", "..", "a\\..\\b", "a/b:stream", "nul", "dir/COM1.txt", "bad?.txt", "dot."] {
            let mut tar = Vec::new();
            file(&mut tar, name, b"x");
            let (dir, result) = unpacked(&finish(tar));
            assert!(matches!(result, Err(TarError::Refused(_))), "{name}: {result:?}");
            assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0, "{name}");
        }
        let mut tar = Vec::new();
        pax(&mut tar, &[("path", "../escape")]);
        file(&mut tar, "fine", b"x");
        assert!(matches!(unpacked(&finish(tar)).1, Err(TarError::Refused(_))));
        let mut tar = Vec::new();
        long_name(&mut tar, "/abs/long");
        file(&mut tar, "fine", b"x");
        assert!(matches!(unpacked(&finish(tar)).1, Err(TarError::Refused(_))));
    }

    #[test]
    fn refuses_links_and_devices() {
        for kind in *b"12346SV" {
            let mut tar = Vec::new();
            file(&mut tar, "ok.txt", b"ok");
            entry(&mut tar, "special", kind, b"");
            let (_, result) = unpacked(&finish(tar));
            assert!(matches!(result, Err(TarError::Refused(_))), "{}: {result:?}", char::from(kind));
        }
        let mut tar = Vec::new();
        pax(&mut tar, &[("linkpath", "/etc/passwd")]);
        entry(&mut tar, "link", b'2', b"");
        let message = unpacked(&finish(tar)).1.unwrap_err().to_string();
        assert!(message.contains("link to /etc/passwd"), "{message}");
    }

    #[test]
    fn refuses_damaged_archives() {
        let mut tar = Vec::new();
        file(&mut tar, "a.txt", b"alpha");
        let complete = finish(tar.clone());
        let mut broken = complete.clone();
        broken[0] = b'b';
        assert!(matches!(unpacked(&broken).1, Err(TarError::Invalid(_))));
        // No end marker, or cut inside a file.
        assert!(matches!(unpacked(&tar).1, Err(TarError::Invalid(_))));
        assert!(matches!(unpacked(&complete[..BLOCK + 3]).1, Err(TarError::Invalid(_))));
        assert!(matches!(unpacked(&complete[..100]).1, Err(TarError::Invalid(_))));
        let mut pax_broken = Vec::new();
        entry(&mut pax_broken, "PaxHeaders/x", b'x', b"99 path=x\n");
        assert!(matches!(unpacked(&finish(pax_broken)).1, Err(TarError::Invalid(_))));
    }

    #[test]
    fn stops_when_cancelled() {
        let mut tar = Vec::new();
        file(&mut tar, "a.txt", b"alpha");
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(unpack(&finish(tar)[..], dir.path(), &AtomicBool::new(true)), Err(TarError::Cancelled)));
    }
}
