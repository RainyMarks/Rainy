//! ZIP archives as release pages publish them (the Strata Terminal, ruff, ty): stored and deflated entries, ZIP64 sizes
//! and offsets, unpacked into a folder. A name that would leave the folder, an encrypted entry or a wrong CRC fails the
//! unpack; what was already written stays for the caller to remove with the folder.

use std::fs::{self, File};
use std::io::{self, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context as _, Result, anyhow, bail};
use flate2::Crc;
use flate2::read::DeflateDecoder;

const END: u32 = 0x0605_4b50;
const END64: u32 = 0x0606_4b50;
const LOCATOR64: u32 = 0x0706_4b50;
const CENTRAL: u32 = 0x0201_4b50;
const LOCAL: u32 = 0x0403_4b50;
/// The end record, its largest comment, and the ZIP64 locator before it.
const TAIL: u64 = 22 + 65_535 + 20;
const MAX_DIRECTORY: u64 = 256 * 1024 * 1024;
/// Upper bounds guarding against zip bombs and integer overflow.
const MAX_UNPACKED_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const MAX_ENTRIES: u64 = 200_000;

/// One file of the archive, as its central directory describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    method: u16,
    flags: u16,
    crc: u32,
    pub compressed: u64,
    pub size: u64,
    offset: u64,
    version_made_by_os: u8,
    external_attr: u32,
}

impl Entry {
    pub fn is_dir(&self) -> bool {
        self.name.ends_with('/') || self.name.ends_with('\\')
    }

    fn is_symlink(&self) -> bool {
        self.version_made_by_os == 3 && ((self.external_attr >> 16) & 0o170000) == 0o120000
    }
}

fn u16_at(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

fn u64_at(bytes: &[u8], at: usize) -> u64 {
    u64::from(u32_at(bytes, at)) | (u64::from(u32_at(bytes, at + 4)) << 32)
}

fn damaged() -> anyhow::Error {
    anyhow!("The ZIP archive is damaged.")
}

/// The archive's entries, in the order of its central directory.
pub fn entries(file: &mut File) -> Result<Vec<Entry>> {
    let length = file.seek(SeekFrom::End(0))?;
    let tail = length.min(TAIL);
    file.seek(SeekFrom::Start(length - tail))?;
    let mut buffer = vec![0u8; tail as usize];
    file.read_exact(&mut buffer)?;
    let end = (0..=buffer.len().saturating_sub(22))
        .rev()
        .find(|&at| u32_at(&buffer, at) == END && at + 22 + usize::from(u16_at(&buffer, at + 20)) == buffer.len())
        .context("The file is not a ZIP archive.")?;
    let mut count = u64::from(u16_at(&buffer, end + 10));
    let mut size = u64::from(u32_at(&buffer, end + 12));
    let mut start = u64::from(u32_at(&buffer, end + 16));
    if count == 0xFFFF || size == 0xFFFF_FFFF || start == 0xFFFF_FFFF {
        if end < 20 || u32_at(&buffer, end - 20) != LOCATOR64 {
            return Err(damaged());
        }
        let record = u64_at(&buffer, end - 20 + 8);
        file.seek(SeekFrom::Start(record))?;
        let mut head = [0u8; 56];
        file.read_exact(&mut head)?;
        if u32_at(&head, 0) != END64 {
            return Err(damaged());
        }
        count = u64_at(&head, 32);
        size = u64_at(&head, 40);
        start = u64_at(&head, 48);
    }
    if count > MAX_ENTRIES {
        bail!("The ZIP archive contains too many entries.");
    }
    if size > MAX_DIRECTORY || start.checked_add(size).is_none_or(|stop| stop > length) {
        return Err(damaged());
    }
    file.seek(SeekFrom::Start(start))?;
    let mut directory = vec![0u8; size as usize];
    file.read_exact(&mut directory)?;
    let mut list = Vec::new();
    let mut at = 0usize;
    for _ in 0..count {
        if at.checked_add(46).is_none_or(|end| end > directory.len()) || u32_at(&directory, at) != CENTRAL {
            return Err(damaged());
        }
        let version_made_by_os = directory[at + 5];
        let flags = u16_at(&directory, at + 8);
        let method = u16_at(&directory, at + 10);
        let crc = u32_at(&directory, at + 16);
        let mut compressed = u64::from(u32_at(&directory, at + 20));
        let mut unpacked = u64::from(u32_at(&directory, at + 24));
        let name_length = usize::from(u16_at(&directory, at + 28));
        let extra_length = usize::from(u16_at(&directory, at + 30));
        let comment_length = usize::from(u16_at(&directory, at + 32));
        let external_attr = u32_at(&directory, at + 38);
        let mut offset = u64::from(u32_at(&directory, at + 42));
        let name_end = at.checked_add(46).and_then(|n| n.checked_add(name_length)).ok_or_else(damaged)?;
        let extra_end = name_end.checked_add(extra_length).ok_or_else(damaged)?;
        let next = extra_end.checked_add(comment_length).ok_or_else(damaged)?;
        if next > directory.len() {
            return Err(damaged());
        }
        let name = String::from_utf8(directory[at + 46..name_end].to_vec()).map_err(|_| anyhow!("The ZIP archive names a file in an unknown encoding."))?;
        // ZIP64: the fields that did not fit, in this order.
        let mut extra = &directory[name_end..extra_end];
        while extra.len() >= 4 {
            let (id, length) = (u16_at(extra, 0), usize::from(u16_at(extra, 2)));
            if 4 + length > extra.len() {
                break;
            }
            if id == 0x0001 {
                let mut field = &extra[4..4 + length];
                for value in [&mut unpacked, &mut compressed, &mut offset] {
                    if *value == 0xFFFF_FFFF && field.len() >= 8 {
                        *value = u64_at(field, 0);
                        field = &field[8..];
                    }
                }
            }
            extra = &extra[4 + length..];
        }
        list.push(Entry { name, method, flags, crc, compressed, size: unpacked, offset, version_made_by_os, external_attr });
        at = next;
    }
    Ok(list)
}

/// Where `name` goes inside `into`, or an error when it would leave it or is not portable on Windows.
fn place(into: &Path, name: &str) -> Result<PathBuf> {
    match crate::fs_util::safe_archive_relative(name) {
        Ok(Some(relative)) => Ok(into.join(relative)),
        Ok(None) => bail!("The ZIP archive contains a file without a name."),
        Err(why) => bail!("The ZIP archive contains an unsafe path ({why}): {name}"),
    }
}

/// A reader that counts what passes through it into a CRC.
struct Checked<R> {
    inner: R,
    crc: Crc,
}

impl<R: Read> Read for Checked<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let read = self.inner.read(buffer)?;
        self.crc.update(&buffer[..read]);
        Ok(read)
    }
}

/// Unpack every entry of `archive` into `into`, which is created; `progress` gets the bytes unpacked and their total.
/// Returns the number of files written.
pub fn unpack(archive: &Path, into: &Path, cancel: &AtomicBool, progress: &dyn Fn(u64, u64)) -> Result<usize> {
    let mut file = File::open(archive).with_context(|| format!("Opening {}", archive.display()))?;
    let list = entries(&mut file)?;
    let mut total = 0u64;
    for entry in &list {
        total = total.checked_add(entry.size).ok_or_else(damaged)?;
        if total > MAX_UNPACKED_BYTES {
            bail!("The ZIP archive exceeds the maximum unpacked size.");
        }
    }
    fs::create_dir_all(into).with_context(|| format!("Creating {}", into.display()))?;
    let (mut done, mut files) = (0u64, 0usize);
    let mut buffer = vec![0u8; 256 * 1024];
    for entry in &list {
        if cancel.load(Ordering::Relaxed) {
            bail!("cancelled");
        }
        if entry.is_symlink() {
            bail!("The ZIP archive contains a symbolic link: {}", entry.name);
        }
        let target = place(into, &entry.name)?;
        if entry.is_dir() {
            fs::create_dir_all(&target)?;
            continue;
        }
        if entry.flags & 1 != 0 {
            bail!("The ZIP archive contains an encrypted file: {}", entry.name);
        }
        file.seek(SeekFrom::Start(entry.offset))?;
        let mut head = [0u8; 30];
        file.read_exact(&mut head)?;
        if u32_at(&head, 0) != LOCAL {
            return Err(damaged());
        }
        let skip = u64::from(u16_at(&head, 26)) + u64::from(u16_at(&head, 28));
        file.seek(SeekFrom::Current(skip as i64))?;
        let raw = (&mut file).take(entry.compressed);
        let mut reader: Checked<Box<dyn Read + '_>> = match entry.method {
            0 => Checked { inner: Box::new(raw), crc: Crc::new() },
            8 => Checked { inner: Box::new(DeflateDecoder::new(raw)), crc: Crc::new() },
            method => bail!("The ZIP archive uses a compression Rainy cannot read ({method}): {}", entry.name),
        };
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut output = BufWriter::new(File::create(&target).with_context(|| format!("Creating {}", target.display()))?);
        let mut written = 0u64;
        loop {
            if cancel.load(Ordering::Relaxed) {
                bail!("cancelled");
            }
            let read = reader.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            written += read as u64;
            if written > entry.size {
                return Err(damaged());
            }
            output.write_all(&buffer[..read])?;
            done += read as u64;
            progress(done, total);
        }
        output.flush()?;
        if written != entry.size || reader.crc.sum() != entry.crc {
            bail!("A file in the ZIP archive is damaged: {}", entry.name);
        }
        files += 1;
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use ::core::prelude::v1::test;

    use super::*;

    /// A ZIP archive with the given entries, deflated or stored, as release tools write them.
    fn archive(entries: &[(&str, &[u8], bool)]) -> Vec<u8> {
        use flate2::Compression;
        use flate2::write::DeflateEncoder;
        let (mut body, mut directory) = (Vec::new(), Vec::new());
        for (name, data, deflate) in entries {
            let mut crc = Crc::new();
            crc.update(data);
            let packed = if *deflate {
                let mut encoder = DeflateEncoder::new(Vec::new(), Compression::default());
                encoder.write_all(data).unwrap();
                encoder.finish().unwrap()
            } else {
                data.to_vec()
            };
            let method: u16 = if *deflate { 8 } else { 0 };
            let offset = body.len() as u32;
            let mut local = Vec::new();
            local.extend_from_slice(&LOCAL.to_le_bytes());
            local.extend_from_slice(&[20, 0, 0, 0]);
            local.extend_from_slice(&method.to_le_bytes());
            local.extend_from_slice(&[0; 4]);
            local.extend_from_slice(&crc.sum().to_le_bytes());
            local.extend_from_slice(&(packed.len() as u32).to_le_bytes());
            local.extend_from_slice(&(data.len() as u32).to_le_bytes());
            local.extend_from_slice(&(name.len() as u16).to_le_bytes());
            local.extend_from_slice(&[0, 0]);
            local.extend_from_slice(name.as_bytes());
            body.extend_from_slice(&local);
            body.extend_from_slice(&packed);
            directory.extend_from_slice(&CENTRAL.to_le_bytes());
            directory.extend_from_slice(&[20, 0, 20, 0, 0, 0]);
            directory.extend_from_slice(&method.to_le_bytes());
            directory.extend_from_slice(&[0; 4]);
            directory.extend_from_slice(&crc.sum().to_le_bytes());
            directory.extend_from_slice(&(packed.len() as u32).to_le_bytes());
            directory.extend_from_slice(&(data.len() as u32).to_le_bytes());
            directory.extend_from_slice(&(name.len() as u16).to_le_bytes());
            directory.extend_from_slice(&[0; 12]);
            directory.extend_from_slice(&offset.to_le_bytes());
            directory.extend_from_slice(name.as_bytes());
        }
        let start = body.len() as u32;
        body.extend_from_slice(&directory);
        body.extend_from_slice(&END.to_le_bytes());
        body.extend_from_slice(&[0; 4]);
        body.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        body.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        body.extend_from_slice(&(directory.len() as u32).to_le_bytes());
        body.extend_from_slice(&start.to_le_bytes());
        body.extend_from_slice(&[0, 0]);
        body
    }

    #[test]
    fn unpacks_stored_and_deflated_files() {
        let dir = tempfile::tempdir().unwrap();
        let zip = dir.path().join("a.zip");
        let text = b"hello hello hello hello".repeat(50);
        fs::write(&zip, archive(&[("bin/strata.cmd", b"@echo off", false), ("python/lib/x.py", &text, true), ("docs/", b"", false)])).unwrap();
        let out = dir.path().join("out");
        let files = unpack(&zip, &out, &AtomicBool::new(false), &|_, _| {}).unwrap();
        assert_eq!(files, 2);
        assert_eq!(fs::read(out.join("bin").join("strata.cmd")).unwrap(), b"@echo off");
        assert_eq!(fs::read(out.join("python").join("lib").join("x.py")).unwrap(), text);
        assert!(out.join("docs").is_dir());
    }

    #[test]
    fn refuses_names_that_leave_the_folder_and_damaged_files() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["../evil.txt", "a/../../evil.txt", "C:/evil.txt", "a/b:stream", "./"] {
            let zip = dir.path().join("bad.zip");
            fs::write(&zip, archive(&[(name, b"x", false)])).unwrap();
            assert!(unpack(&zip, &dir.path().join("out"), &AtomicBool::new(false), &|_, _| {}).is_err(), "{name}");
        }
        assert!(!dir.path().join("evil.txt").exists());
        // A changed byte fails the CRC.
        let zip = dir.path().join("crc.zip");
        let mut bytes = archive(&[("a.txt", b"abcdef", false)]);
        let at = bytes.windows(6).position(|window| window == b"abcdef").unwrap();
        bytes[at] = b'X';
        fs::write(&zip, bytes).unwrap();
        assert!(unpack(&zip, &dir.path().join("out2"), &AtomicBool::new(false), &|_, _| {}).is_err());
        fs::write(&zip, b"not a zip").unwrap();
        assert!(unpack(&zip, &dir.path().join("out3"), &AtomicBool::new(false), &|_, _| {}).is_err());
    }

    #[test]
    fn refuses_windows_reserved_names_leading_slashes_symlinks_and_handles_eocd_in_comment() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["/etc/passwd", "\\abs\\x", "CON.txt", "dir/nul", "bad?.txt", "dot."] {
            let zip = dir.path().join("unsafe.zip");
            fs::write(&zip, archive(&[(name, b"x", false)])).unwrap();
            assert!(unpack(&zip, &dir.path().join("out-unsafe"), &AtomicBool::new(false), &|_, _| {}).is_err(), "{name}");
        }

        // Backslash directory entry is treated as a directory.
        let bs_zip = dir.path().join("backslash-dir.zip");
        fs::write(&bs_zip, archive(&[("sub\\", b"", false), ("sub/ok.txt", b"ok", false)])).unwrap();
        let bs_out = dir.path().join("bs-out");
        assert_eq!(unpack(&bs_zip, &bs_out, &AtomicBool::new(false), &|_, _| {}).unwrap(), 1);
        assert!(bs_out.join("sub").is_dir());

        // Unix symlink entry (version_made_by OS = 3, mode = 0o120777) must be refused.
        let mut sym_bytes = archive(&[("link", b"/etc/passwd", false)]);
        let central = sym_bytes.windows(4).position(|w| w == CENTRAL.to_le_bytes()).unwrap();
        sym_bytes[central + 5] = 3; // Unix
        let attr = (0o120777u32 << 16).to_le_bytes();
        sym_bytes[central + 38..central + 42].copy_from_slice(&attr);
        let sym_zip = dir.path().join("symlink.zip");
        fs::write(&sym_zip, sym_bytes).unwrap();
        assert!(unpack(&sym_zip, &dir.path().join("sym-out"), &AtomicBool::new(false), &|_, _| {}).is_err());

        // A fake EOCD signature inside the archive comment must not fool EOCD scanning.
        let mut commented = archive(&[("ok.txt", b"hello", false)]);
        let eocd = commented.len() - 22;
        let fake_comment = [END.to_le_bytes().as_slice(), &[0u8; 20]].concat();
        commented[eocd + 20..eocd + 22].copy_from_slice(&(fake_comment.len() as u16).to_le_bytes());
        commented.extend_from_slice(&fake_comment);
        let c_zip = dir.path().join("comment.zip");
        fs::write(&c_zip, commented).unwrap();
        let c_out = dir.path().join("c-out");
        assert_eq!(unpack(&c_zip, &c_out, &AtomicBool::new(false), &|_, _| {}).unwrap(), 1);
        assert_eq!(fs::read(c_out.join("ok.txt")).unwrap(), b"hello");
    }
}
