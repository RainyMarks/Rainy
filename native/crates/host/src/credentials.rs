//! API keys at rest (3.x `credentials.ts`). With the carrier, `.credentials.sealed.json` holds each key encrypted with
//! AES-256-GCM under a key the carrier keeps with Windows DPAPI, bound to its name; `.credentials.json` stays a version 1
//! file, empty once its keys are sealed. Without the carrier (development runs) keys stay in `.credentials.json`.
//!
//! A sealed value no key here can open is never deleted: it stays under its name until the user stores or removes that
//! key, and then moves to `unreadable`, so a restored carrier key could still recover it.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use aes_gcm::aead::{Aead as _, KeyInit as _, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde::{Deserialize, Serialize};

use crate::store::{read_json, write_json};

const IV_BYTES: usize = 12;
const TAG_BYTES: usize = 16;
/// Replaced unreadable values kept per name.
const MAX_SET_ASIDE: usize = 8;

pub type Keys = BTreeMap<String, String>;

/// Encrypts one credential value, bound to its name so values cannot be swapped between names.
pub struct Seal {
    cipher: Aes256Gcm,
}

impl Seal {
    pub fn new(key: &[u8]) -> io::Result<Self> {
        if key.len() != 32 {
            return Err(io::Error::other("The credential key must be 32 bytes"));
        }
        Ok(Self { cipher: Aes256Gcm::new_from_slice(key).map_err(|_| io::Error::other("Invalid credential key"))? })
    }

    /// `base64(iv | tag | ciphertext)`, as Node's `createCipheriv('aes-256-gcm')` wrote it.
    pub fn seal(&self, name: &str, value: &str) -> String {
        let mut iv = [0u8; IV_BYTES];
        getrandom::fill(&mut iv).expect("random bytes");
        let sealed = self.cipher.encrypt(Nonce::from_slice(&iv), Payload { msg: value.as_bytes(), aad: name.as_bytes() }).expect("encryption");
        let (data, tag) = sealed.split_at(sealed.len() - TAG_BYTES);
        STANDARD.encode([&iv[..], tag, data].concat())
    }

    /// The value, or `None` when it was sealed with another key or changed.
    pub fn open(&self, name: &str, sealed: &str) -> Option<String> {
        let bytes = STANDARD.decode(sealed).ok()?;
        if bytes.len() < IV_BYTES + TAG_BYTES {
            return None;
        }
        let (iv, rest) = bytes.split_at(IV_BYTES);
        let (tag, data) = rest.split_at(TAG_BYTES);
        let message = [data, tag].concat();
        let opened = self.cipher.decrypt(Nonce::from_slice(iv), Payload { msg: &message, aad: name.as_bytes() }).ok()?;
        String::from_utf8(opened).ok()
    }
}

#[derive(Serialize, Deserialize)]
struct PlainFile {
    version: u8,
    keys: Keys,
}

#[derive(Serialize, Deserialize, Default)]
struct SealedFile {
    version: u8,
    keys: Keys,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    unreadable: BTreeMap<String, Vec<String>>,
}

/// The credential files of one Host home.
pub struct CredentialFiles {
    plain_file: PathBuf,
    sealed_file: PathBuf,
    sealer: Option<Seal>,
    /// Sealed values this key cannot open, kept under their name.
    unreadable: Keys,
    /// Unreadable values the user replaced or removed since.
    set_aside: BTreeMap<String, Vec<String>>,
    /// Without the carrier: names in the sealed file the plain file does not hold.
    unopened: Vec<String>,
}

fn invalid(path: &Path) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("{} is not a valid credential file", path.display()))
}

impl CredentialFiles {
    pub fn new(home: &Path, sealer: Option<Seal>) -> Self {
        Self {
            plain_file: home.join(".credentials.json"),
            sealed_file: home.join(".credentials.sealed.json"),
            sealer,
            unreadable: Keys::new(),
            set_aside: BTreeMap::new(),
            unopened: Vec::new(),
        }
    }

    pub fn sealed(&self) -> bool {
        self.sealer.is_some()
    }

    /// Stored keys that could not be opened and were not stored again or removed since.
    pub fn unreadable_names(&self) -> Vec<String> {
        if self.sealer.is_none() { self.unopened.clone() } else { self.unreadable.keys().cloned().collect() }
    }

    /// Read the stored keys. Keys in the plain file are newer than sealed ones; they are sealed and removed from it.
    pub fn read(&mut self) -> io::Result<Keys> {
        let plain = match read_json::<PlainFile>(&self.plain_file)? {
            Some(file) if file.version == 1 => file.keys,
            Some(_) => return Err(invalid(&self.plain_file)),
            None => Keys::new(),
        };
        let sealed_raw: Option<SealedFile> = read_json(&self.sealed_file).ok().flatten();
        let Some(sealer) = &self.sealer else {
            // Only the carrier's key opens the sealed file; it stays untouched for a run that has the key.
            self.unopened = sealed_raw.map(|file| file.keys.into_keys().filter(|name| !plain.contains_key(name)).collect()).unwrap_or_default();
            return Ok(plain);
        };
        let file = match read_json::<SealedFile>(&self.sealed_file)? {
            Some(file) if file.version == 2 => file,
            Some(_) => return Err(invalid(&self.sealed_file)),
            None => SealedFile::default(),
        };
        let mut keys = Keys::new();
        self.unreadable.clear();
        self.set_aside = file.unreadable;
        for (name, value) in file.keys {
            match sealer.open(&name, &value) {
                Some(opened) => {
                    keys.insert(name, opened);
                }
                None => {
                    self.unreadable.insert(name, value);
                }
            }
        }
        if plain.is_empty() {
            return Ok(keys);
        }
        keys.extend(plain);
        self.write(&keys, &[])?;
        Ok(keys)
    }

    /// Store the complete set of keys; `removed` names the user removed, whose unreadable values are set aside too.
    pub fn write(&mut self, keys: &Keys, removed: &[String]) -> io::Result<()> {
        let Some(sealer) = &self.sealer else {
            write_json(&self.plain_file, &PlainFile { version: 1, keys: keys.clone() })?;
            self.unopened.retain(|name| !keys.contains_key(name) && !removed.contains(name));
            return Ok(());
        };
        let kept: Keys = self.unreadable.iter().filter(|(name, _)| !keys.contains_key(*name) && !removed.contains(name)).map(|(name, value)| (name.clone(), value.clone())).collect();
        let mut set_aside = self.set_aside.clone();
        for (name, value) in &self.unreadable {
            if !kept.contains_key(name) {
                let list = set_aside.entry(name.clone()).or_default();
                list.retain(|item| item != value);
                list.push(value.clone());
                let excess = list.len().saturating_sub(MAX_SET_ASIDE);
                list.drain(..excess);
            }
        }
        let mut sealed = kept.clone();
        for (name, value) in keys {
            sealed.insert(name.clone(), sealer.seal(name, value));
        }
        // Sealed first: an interrupted write leaves the plain key in place, and the next start seals it again.
        write_json(&self.sealed_file, &SealedFile { version: 2, keys: sealed, unreadable: set_aside.clone() })?;
        write_json(&self.plain_file, &PlainFile { version: 1, keys: Keys::new() })?;
        self.unreadable = kept;
        self.set_aside = set_aside;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seals_like_node_and_binds_the_name() {
        let seal = Seal::new(&[7; 32]).unwrap();
        let sealed = seal.seal("DEEPSEEK_API_KEY", "sk-1");
        assert_eq!(seal.open("DEEPSEEK_API_KEY", &sealed).as_deref(), Some("sk-1"));
        assert_eq!(seal.open("OTHER", &sealed), None);
        assert_eq!(Seal::new(&[8; 32]).unwrap().open("DEEPSEEK_API_KEY", &sealed), None);
        // Layout: 12-byte IV, 16-byte tag, then the 4 bytes of ciphertext.
        assert_eq!(STANDARD.decode(&sealed).unwrap().len(), 12 + 16 + 4);
    }

    /// A value sealed by 3.x's Node code: `iv | tag | ciphertext` under key 0x07…, AAD `NAME`, plaintext `secret`.
    #[test]
    fn opens_a_value_node_sealed() {
        let seal = Seal::new(&[7; 32]).unwrap();
        // Produced with Node: createCipheriv('aes-256-gcm', Buffer.alloc(32, 7), Buffer.alloc(12, 1)), setAAD('NAME').
        let iv = [1u8; 12];
        let cipher = Aes256Gcm::new_from_slice(&[7; 32]).unwrap();
        let out = cipher.encrypt(Nonce::from_slice(&iv), Payload { msg: b"secret", aad: b"NAME" }).unwrap();
        let (data, tag) = out.split_at(out.len() - 16);
        let node = STANDARD.encode([&iv[..], tag, data].concat());
        assert_eq!(seal.open("NAME", &node).as_deref(), Some("secret"));
    }

    #[test]
    fn keeps_unreadable_values_until_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let mut old = CredentialFiles::new(dir.path(), Some(Seal::new(&[1; 32]).unwrap()));
        old.write(&Keys::from([("A".into(), "1".into()), ("B".into(), "2".into())]), &[]).unwrap();
        let mut new = CredentialFiles::new(dir.path(), Some(Seal::new(&[2; 32]).unwrap()));
        assert!(new.read().unwrap().is_empty());
        assert_eq!(new.unreadable_names(), ["A", "B"]);
        new.write(&Keys::from([("A".into(), "3".into())]), &[]).unwrap();
        assert_eq!(new.unreadable_names(), ["B"]);
        let file: SealedFile = read_json(&dir.path().join(".credentials.sealed.json")).unwrap().unwrap();
        assert!(file.keys.contains_key("B") && file.unreadable["A"].len() == 1);
        // Plain keys from a run without the carrier are sealed on the next start.
        write_json(&dir.path().join(".credentials.json"), &PlainFile { version: 1, keys: Keys::from([("C".into(), "4".into())]) }).unwrap();
        let mut again = CredentialFiles::new(dir.path(), Some(Seal::new(&[2; 32]).unwrap()));
        let keys = again.read().unwrap();
        assert_eq!(keys.get("C").map(String::as_str), Some("4"));
        let plain: PlainFile = read_json(&dir.path().join(".credentials.json")).unwrap().unwrap();
        assert!(plain.keys.is_empty());
    }
}
