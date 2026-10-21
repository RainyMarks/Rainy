//! The key that encrypts API keys in the Host's credential file (`src/host/credentials.ts`).
//!
//! Rainy 3.x kept it in `<userData>/credential-key.bin`, encrypted with Electron's `safeStorage`, which is Chromium's
//! OSCrypt: `v10` + a 12-byte nonce + AES-256-GCM ciphertext and tag, under a key stored in `<userData>/Local State` as
//! `os_crypt.encrypted_key` = base64(`DPAPI` + a Windows DPAPI blob). This module reads and writes exactly that format,
//! so keys saved by 3.x open in 4.0 and the other way round.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use anyhow::{Context as _, Result, anyhow, bail};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Map, Value};

const KEY_FILE: &str = "credential-key.bin";
const LOCAL_STATE: &str = "Local State";
const VERSION: &[u8] = b"v10";
const DPAPI_PREFIX: &[u8] = b"DPAPI";
const NONCE_LEN: usize = 12;

/// Protects and opens bytes for the current Windows account.
pub trait Protector {
    fn protect(&self, data: &[u8]) -> Result<Vec<u8>>;
    fn unprotect(&self, data: &[u8]) -> Result<Vec<u8>>;
}

/// Windows DPAPI (`CryptProtectData`), bound to the signed-in account.
pub struct Dpapi;

#[cfg(windows)]
impl Protector for Dpapi {
    fn protect(&self, data: &[u8]) -> Result<Vec<u8>> {
        dpapi(data, true)
    }
    fn unprotect(&self, data: &[u8]) -> Result<Vec<u8>> {
        dpapi(data, false)
    }
}

#[cfg(windows)]
fn dpapi(data: &[u8], protect: bool) -> Result<Vec<u8>> {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
    };
    let input = CRYPT_INTEGER_BLOB { cbData: data.len() as u32, pbData: data.as_ptr().cast_mut() };
    let mut output = CRYPT_INTEGER_BLOB { cbData: 0, pbData: std::ptr::null_mut() };
    // SAFETY: the input blob points at `data`, which outlives the call; the output is allocated by Windows and freed below.
    let ok = unsafe {
        if protect {
            CryptProtectData(&input, std::ptr::null(), std::ptr::null(), std::ptr::null(), std::ptr::null(), CRYPTPROTECT_UI_FORBIDDEN, &mut output)
        } else {
            CryptUnprotectData(&input, std::ptr::null_mut(), std::ptr::null(), std::ptr::null(), std::ptr::null(), CRYPTPROTECT_UI_FORBIDDEN, &mut output)
        }
    };
    if ok == 0 {
        return Err(anyhow!("Windows data protection failed: {}", std::io::Error::last_os_error()));
    }
    // SAFETY: Windows returned `cbData` bytes at `pbData`, which stay valid until LocalFree.
    let bytes = unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize) }.to_vec();
    unsafe { LocalFree(output.pbData.cast()) };
    Ok(bytes)
}

#[cfg(not(windows))]
impl Protector for Dpapi {
    fn protect(&self, _data: &[u8]) -> Result<Vec<u8>> {
        bail!("Windows data protection is unavailable.")
    }
    fn unprotect(&self, _data: &[u8]) -> Result<Vec<u8>> {
        bail!("Windows data protection is unavailable.")
    }
}

/// The Chromium OSCrypt key from `Local State`, created when the file has none.
fn os_crypt_key(user_data: &Path, protector: &dyn Protector) -> Result<[u8; 32]> {
    let path = user_data.join(LOCAL_STATE);
    let mut state: Map<String, Value> = match fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes).context("Local State is not valid JSON")?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Map::new(),
        Err(error) => return Err(error).context("Reading Local State"),
    };
    if let Some(encoded) = state.get("os_crypt").and_then(|value| value.get("encrypted_key")).and_then(Value::as_str) {
        let blob = STANDARD.decode(encoded).context("Local State key is not base64")?;
        let blob = blob.strip_prefix(DPAPI_PREFIX).ok_or_else(|| anyhow!("Local State key is not DPAPI-protected"))?;
        let key = protector.unprotect(blob)?;
        return key.try_into().map_err(|_| anyhow!("Local State key has the wrong length"));
    }
    let mut key = [0u8; 32];
    getrandom::fill(&mut key).map_err(|error| anyhow!("{error}"))?;
    let mut blob = DPAPI_PREFIX.to_vec();
    blob.extend(protector.protect(&key)?);
    let os_crypt = state.entry("os_crypt").or_insert_with(|| Value::Object(Map::new()));
    let Value::Object(os_crypt) = os_crypt else { bail!("Local State has an unexpected os_crypt entry") };
    os_crypt.insert("encrypted_key".into(), Value::String(STANDARD.encode(blob)));
    write_atomic(&path, &serde_json::to_vec(&state)?)?;
    Ok(key)
}

/// OSCrypt `EncryptString`.
pub fn seal(key: &[u8; 32], text: &str) -> Result<Vec<u8>> {
    let mut nonce = [0u8; NONCE_LEN];
    getrandom::fill(&mut nonce).map_err(|error| anyhow!("{error}"))?;
    let cipher = Aes256Gcm::new(key.into());
    let sealed = cipher.encrypt(Nonce::from_slice(&nonce), text.as_bytes()).map_err(|_| anyhow!("Encryption failed"))?;
    Ok([VERSION, &nonce, &sealed].concat())
}

/// OSCrypt `DecryptString`.
pub fn open(key: &[u8; 32], data: &[u8]) -> Result<String> {
    let body = data.strip_prefix(VERSION).ok_or_else(|| anyhow!("Unknown encryption version"))?;
    if body.len() < NONCE_LEN + 16 {
        bail!("Encrypted data is too short");
    }
    let (nonce, sealed) = body.split_at(NONCE_LEN);
    let cipher = Aes256Gcm::new(key.into());
    let plain = cipher.decrypt(Nonce::from_slice(nonce), sealed).map_err(|_| anyhow!("Decryption failed"))?;
    String::from_utf8(plain).context("Decrypted text is not UTF-8")
}

fn valid_key(key: &str) -> bool {
    key.len() == 44 && key.ends_with('=') && key[..43].bytes().all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/')
}

/// Read the credential key, creating it on first use (`src/main/credential-key.ts`).
/// Returns `Ok(None)` when the system cannot protect it; the Host then keeps API keys unencrypted for this run.
pub fn credential_key(user_data: &Path, protector: &dyn Protector, log: &dyn Fn(&str)) -> Result<Option<String>> {
    let os_key = match os_crypt_key(user_data, protector) {
        Ok(key) => key,
        Err(error) => {
            log(&format!("API keys are stored unencrypted: {error}"));
            return Ok(None);
        }
    };
    let path = user_data.join(KEY_FILE);
    match fs::read(&path) {
        Ok(stored) => {
            if let Ok(key) = open(&os_key, &stored)
                && valid_key(&key)
            {
                return Ok(Some(key));
            }
            // Windows can no longer open it, for example after an administrator reset the account password. The file
            // is kept for diagnosis; API keys stored with it have to be entered again.
            let aside = unreadable_path(user_data);
            fs::rename(&path, &aside)?;
            log(&format!("The credential key could not be read and was moved to {}; stored API keys must be entered again.", aside.display()));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("Reading the credential key"),
    }
    let mut raw = [0u8; 32];
    getrandom::fill(&mut raw).map_err(|error| anyhow!("{error}"))?;
    let key = STANDARD.encode(raw);
    write_atomic(&path, &seal(&os_key, &key)?)?;
    Ok(Some(key))
}

fn unreadable_path(user_data: &Path) -> PathBuf {
    let millis = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    user_data.join(format!("credential-key.unreadable-{millis}.bin"))
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let directory = path.parent().ok_or_else(|| anyhow!("No parent directory"))?;
    fs::create_dir_all(directory)?;
    let mut suffix = [0u8; 8];
    getrandom::fill(&mut suffix).map_err(|error| anyhow!("{error}"))?;
    let name = path.file_name().and_then(|name| name.to_str()).unwrap_or("record");
    let temporary = directory.join(format!(".{name}.{}.tmp", suffix.iter().map(|b| format!("{b:02x}")).collect::<String>()));
    let mut file = fs::OpenOptions::new().write(true).create_new(true).open(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&temporary, path).inspect_err(|_| {
        let _ = fs::remove_file(&temporary);
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Stands in for DPAPI: XOR with a fixed byte, so tests run anywhere.
    struct Fake;
    impl Protector for Fake {
        fn protect(&self, data: &[u8]) -> Result<Vec<u8>> {
            Ok(data.iter().map(|b| b ^ 0x5a).collect())
        }
        fn unprotect(&self, data: &[u8]) -> Result<Vec<u8>> {
            self.protect(data)
        }
    }

    #[test]
    fn creates_then_reads_the_same_key() {
        let dir = tempfile::tempdir().unwrap();
        let first = credential_key(dir.path(), &Fake, &|_| {}).unwrap().unwrap();
        assert!(valid_key(&first));
        let state: Value = serde_json::from_slice(&fs::read(dir.path().join(LOCAL_STATE)).unwrap()).unwrap();
        assert!(state["os_crypt"]["encrypted_key"].as_str().unwrap().starts_with("RFBBUE"));
        assert_eq!(credential_key(dir.path(), &Fake, &|_| {}).unwrap().unwrap(), first);
        assert!(fs::read(dir.path().join(KEY_FILE)).unwrap().starts_with(b"v10"));
    }

    #[test]
    fn keeps_other_local_state_fields() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(LOCAL_STATE), br#"{"uninstall_metrics":{"a":1}}"#).unwrap();
        credential_key(dir.path(), &Fake, &|_| {}).unwrap();
        let state: Value = serde_json::from_slice(&fs::read(dir.path().join(LOCAL_STATE)).unwrap()).unwrap();
        assert_eq!(state["uninstall_metrics"]["a"], 1);
    }

    #[test]
    fn moves_an_unreadable_key_aside() {
        let dir = tempfile::tempdir().unwrap();
        let first = credential_key(dir.path(), &Fake, &|_| {}).unwrap().unwrap();
        fs::write(dir.path().join(KEY_FILE), b"v10garbage-garbage-garbage-garbage").unwrap();
        let second = credential_key(dir.path(), &Fake, &|_| {}).unwrap().unwrap();
        assert_ne!(first, second);
        assert!(fs::read_dir(dir.path()).unwrap().any(|e| e.unwrap().file_name().to_string_lossy().starts_with("credential-key.unreadable-")));
    }

    #[test]
    fn seals_and_opens() {
        let key = [7u8; 32];
        assert_eq!(open(&key, &seal(&key, "密钥").unwrap()).unwrap(), "密钥");
        assert!(open(&[8u8; 32], &seal(&key, "x").unwrap()).is_err());
    }
}
