//! The key that encrypts API keys in the Host's credential file (`src/host/credentials.ts`).
//!
//! Rainy 3.x kept it in `<userData>/credential-key.bin`, encrypted with Electron's `safeStorage`, which is Chromium's
//! OSCrypt: `v10` + a 12-byte nonce + AES-256-GCM ciphertext and tag, under a key stored in `<userData>/Local State` as
//! `os_crypt.encrypted_key` = base64(`DPAPI` + a Windows DPAPI blob). This module reads and writes exactly that format,
//! so keys saved by 3.x open in 4.0 and the other way round.

use std::fmt;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use anyhow::{Context as _, Result, anyhow, bail};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Map, Value};

use crate::locale::localized;

const KEY_FILE: &str = "credential-key.bin";
const LOCAL_STATE: &str = "Local State";
const VERSION: &[u8] = b"v10";
const DPAPI_PREFIX: &[u8] = b"DPAPI";
const NONCE_LEN: usize = 12;

/// Security recovery notice surfaced to the interface when credential protection files had to be rebuilt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialWarning {
    /// `Local State` JSON or its DPAPI `os_crypt.encrypted_key` was damaged; the damaged file was backed up and a
    /// fresh DPAPI-protected key was created so API keys never downgrade to plaintext.
    LocalStateRecovered { backup: PathBuf, reason: String },
    /// `credential-key.bin` could not be opened; it was moved aside and a new protected key was created.
    KeyFileRecovered { backup: PathBuf },
}

impl CredentialWarning {
    /// User-facing warning text in the current interface language.
    pub fn localized_message(&self) -> String {
        match self {
            Self::LocalStateRecovered { backup, reason } => localized(
                &format!(
                    "凭据保护文件 Local State 已损坏（{reason}），已备份至 {} 并重新生成受 Windows 数据保护（DPAPI）保护的密钥。之前保存的 API 密钥需要重新输入，新密钥仍以加密方式存储。",
                    backup.display()
                ),
                &format!(
                    "The credential protection file Local State was damaged ({reason}). It was backed up to {} and a new Windows DPAPI-protected key was created. Previously stored API keys must be entered again; new keys remain encrypted.",
                    backup.display()
                ),
            ),
            Self::KeyFileRecovered { backup } => localized(
                &format!(
                    "凭据密钥文件无法解密，已备份至 {} 并重新生成受保护的新密钥。之前保存的 API 密钥需要重新输入。",
                    backup.display()
                ),
                &format!(
                    "The credential key file could not be opened and was moved to {}. A new protected key was created; stored API keys must be entered again.",
                    backup.display()
                ),
            ),
        }
    }
}

impl fmt::Display for CredentialWarning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.localized_message())
    }
}

/// Result of loading or safely recovering the DPAPI-protected credential key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialKey {
    /// Base64 32-byte key used to seal API keys in `.credentials.sealed.json`.
    pub key: String,
    /// Recovery warnings that should be surfaced to the user.
    pub warnings: Vec<CredentialWarning>,
}

/// Structured failure when DPAPI protection cannot be established for this run. Existing files are left untouched; the
/// Host then keeps API keys entered meanwhile in its private `.credentials.json` and seals them on the next run that has
/// the key, which the interface says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialError {
    /// Reading or writing credential files failed due to an I/O error.
    Io { path: PathBuf, detail: String },
    /// Windows DPAPI could not protect a new key (encryption unavailable).
    ProtectionUnavailable { detail: String },
    /// Encryption or random-byte generation failed internally.
    Crypto { detail: String },
}

impl CredentialError {
    /// User-facing error text in the current interface language.
    pub fn localized_message(&self) -> String {
        match self {
            Self::Io { path, detail } => localized(
                &format!("无法读写凭据保护文件 {}（{detail}）。", path.display()),
                &format!("Could not access the credential protection file {} ({detail}).", path.display()),
            ),
            Self::ProtectionUnavailable { detail } => localized(
                &format!("Windows 数据保护（DPAPI）暂时不可用（{detail}）。"),
                &format!("Windows data protection (DPAPI) is unavailable for now ({detail})."),
            ),
            Self::Crypto { detail } => localized(
                &format!("凭据加密初始化失败（{detail}）。"),
                &format!("Credential encryption failed ({detail})."),
            ),
        }
    }
}

impl fmt::Display for CredentialError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.localized_message())
    }
}

impl std::error::Error for CredentialError {}

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
        return Err(anyhow::Error::new(std::io::Error::last_os_error()).context("Windows data protection failed"));
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

/// The Chromium OSCrypt key from `Local State`, created when the file has none, or backed up and recreated under DPAPI
/// when the existing key can never be opened again: the file is damaged, or Windows says the protected key is invalid
/// for this account (an administrator reset the password, a profile copied from another account). Any other DPAPI
/// failure may pass, so it is an error and nothing is touched; the next run tries again.
fn os_crypt_key(
    user_data: &Path,
    protector: &dyn Protector,
    log: &dyn Fn(&str),
) -> std::result::Result<([u8; 32], Option<CredentialWarning>), CredentialError> {
    let path = user_data.join(LOCAL_STATE);
    let existing = match fs::read(&path) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(CredentialError::Io { path, detail: error.to_string() }),
    };
    let mut state = Map::new();
    let mut json_valid = false;
    let mut damaged: Option<String> = None;
    if let Some(bytes) = &existing {
        match serde_json::from_slice::<Map<String, Value>>(bytes) {
            Ok(parsed) => {
                json_valid = true;
                match parsed.get("os_crypt") {
                    None => {
                        state = parsed;
                    }
                    Some(Value::Object(os_crypt)) => match os_crypt.get("encrypted_key") {
                        None => {
                            state = parsed;
                        }
                        Some(Value::String(encoded)) => match decode_os_crypt_key(encoded, protector) {
                            Ok(key) => return Ok((key, None)),
                            Err(KeyProblem::Lost(reason)) => {
                                state = parsed;
                                damaged = Some(reason);
                            }
                            Err(KeyProblem::Unavailable(detail)) => return Err(CredentialError::ProtectionUnavailable { detail }),
                        },
                        Some(_) => {
                            state = parsed;
                            damaged = Some("Local State encrypted_key is not a string".to_owned());
                        }
                    },
                    Some(_) => {
                        state = parsed;
                        damaged = Some("Local State has an unexpected os_crypt entry".to_owned());
                    }
                }
            }
            Err(error) => {
                damaged = Some(format!("Local State is not valid JSON: {error}"));
            }
        }
    }
    // Verify DPAPI protection works before touching or backing up any existing file.
    let mut key = [0u8; 32];
    getrandom::fill(&mut key).map_err(|error| CredentialError::Crypto { detail: error.to_string() })?;
    let protected = protector.protect(&key).map_err(|error| CredentialError::ProtectionUnavailable { detail: error.to_string() })?;
    let mut blob = DPAPI_PREFIX.to_vec();
    blob.extend(protected);

    let warning = if let Some(reason) = damaged {
        let backup = unique_backup_path(user_data, "Local State.unreadable", "");
        if let Some(raw) = &existing {
            if json_valid {
                write_atomic(&backup, raw).map_err(|error| CredentialError::Io { path: backup.clone(), detail: error.to_string() })?;
            } else {
                fs::rename(&path, &backup).map_err(|error| CredentialError::Io { path: path.clone(), detail: error.to_string() })?;
            }
        }
        let warning = CredentialWarning::LocalStateRecovered { backup: backup.clone(), reason: reason.clone() };
        log(&format!(
            "Local State could not be opened ({reason}) and was backed up to {}; created a new DPAPI-protected key so API keys stay encrypted.",
            backup.display()
        ));
        Some(warning)
    } else {
        None
    };

    let mut os_crypt = match state.remove("os_crypt") {
        Some(Value::Object(existing_map)) => existing_map,
        _ => Map::new(),
    };
    os_crypt.insert("encrypted_key".into(), Value::String(STANDARD.encode(blob)));
    state.insert("os_crypt".into(), Value::Object(os_crypt));
    let encoded_state = serde_json::to_vec(&state).map_err(|error| CredentialError::Crypto { detail: error.to_string() })?;
    write_atomic(&path, &encoded_state).map_err(|error| CredentialError::Io { path, detail: error.to_string() })?;
    Ok((key, warning))
}

/// Why the `Local State` key could not be opened.
enum KeyProblem {
    /// It can never be opened again; a new key has to replace it.
    Lost(String),
    /// Windows could not open it this time; it may work on the next run.
    Unavailable(String),
}

/// DPAPI errors that mean the protected data can never be opened by this account: NTE_BAD_KEY_STATE (the account's
/// master key changed, e.g. after an administrator password reset or for a profile copied from another account),
/// NTE_BAD_DATA and ERROR_INVALID_DATA (the blob is damaged).
fn dpapi_key_lost(error: &anyhow::Error) -> bool {
    const NTE_BAD_KEY_STATE: u32 = 0x8009_000B;
    const NTE_BAD_DATA: u32 = 0x8009_0005;
    const ERROR_INVALID_DATA: u32 = 13;
    const HRESULT_INVALID_DATA: u32 = 0x8007_000D;
    error.chain().filter_map(|cause| cause.downcast_ref::<std::io::Error>()).filter_map(std::io::Error::raw_os_error).any(|code| {
        matches!(code as u32, NTE_BAD_KEY_STATE | NTE_BAD_DATA | ERROR_INVALID_DATA | HRESULT_INVALID_DATA)
    })
}

fn decode_os_crypt_key(encoded: &str, protector: &dyn Protector) -> std::result::Result<[u8; 32], KeyProblem> {
    let blob = STANDARD.decode(encoded).map_err(|error| KeyProblem::Lost(format!("Local State key is not base64: {error}")))?;
    let blob = blob.strip_prefix(DPAPI_PREFIX).ok_or_else(|| KeyProblem::Lost("Local State key is not DPAPI-protected".to_owned()))?;
    let key = protector.unprotect(blob).map_err(|error| {
        let detail = format!("{error:#}");
        if dpapi_key_lost(&error) { KeyProblem::Lost(detail) } else { KeyProblem::Unavailable(detail) }
    })?;
    key.try_into().map_err(|_| KeyProblem::Lost("Local State key has the wrong length".to_owned()))
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

/// Load or safely recover the DPAPI-protected credential key.
///
/// - When `Local State` is damaged or its key can never be opened again (see [`os_crypt_key`]) but Windows DPAPI works,
///   the old file is backed up and a fresh DPAPI-protected key is created with an explicit [`CredentialWarning`].
/// - When DPAPI fails otherwise or I/O fails, returns `Err(CredentialError)` without modifying existing files; the
///   interface says that API keys entered in this run stay unencrypted until a later run seals them.
pub fn load_credential_key(
    user_data: &Path,
    protector: &dyn Protector,
    log: &dyn Fn(&str),
) -> std::result::Result<CredentialKey, CredentialError> {
    let (os_key, mut warnings) = match os_crypt_key(user_data, protector, log) {
        Ok((key, warning)) => (key, warning.into_iter().collect::<Vec<_>>()),
        Err(error) => {
            log(&format!("DPAPI credential protection failed; nothing was changed: {error}"));
            return Err(error);
        }
    };
    let path = user_data.join(KEY_FILE);
    let mut needs_backup = false;
    match fs::read(&path) {
        Ok(stored) => {
            if let Ok(key) = open(&os_key, &stored)
                && valid_key(&key)
            {
                return Ok(CredentialKey { key, warnings });
            }
            needs_backup = true;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(CredentialError::Io { path, detail: error.to_string() }),
    }
    let mut raw = [0u8; 32];
    getrandom::fill(&mut raw).map_err(|error| CredentialError::Crypto { detail: error.to_string() })?;
    let key = STANDARD.encode(raw);
    let sealed = seal(&os_key, &key).map_err(|error| CredentialError::Crypto { detail: error.to_string() })?;
    if needs_backup {
        // Windows can no longer open it, for example after an administrator reset the account password or Local State
        // had to be rebuilt. The file is kept for diagnosis; API keys stored with it have to be entered again.
        let aside = unique_backup_path(user_data, "credential-key.unreadable", ".bin");
        fs::rename(&path, &aside).map_err(|error| CredentialError::Io { path: path.clone(), detail: error.to_string() })?;
        log(&format!("The credential key could not be read and was moved to {}; stored API keys must be entered again.", aside.display()));
        warnings.push(CredentialWarning::KeyFileRecovered { backup: aside });
    }
    write_atomic(&path, &sealed).map_err(|error| CredentialError::Io { path, detail: error.to_string() })?;
    Ok(CredentialKey { key, warnings })
}

/// Read the credential key, creating it on first use (`src/main/credential-key.ts`).
/// Never returns `Ok(None)` on `Local State` corruption or DPAPI failure; callers should prefer [`load_credential_key`]
/// to surface structured warnings and errors to the interface.
pub fn credential_key(user_data: &Path, protector: &dyn Protector, log: &dyn Fn(&str)) -> Result<Option<String>> {
    load_credential_key(user_data, protector, log).map(|loaded| Some(loaded.key)).map_err(anyhow::Error::new)
}

fn unique_backup_path(user_data: &Path, stem: &str, suffix: &str) -> PathBuf {
    let millis = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    let candidate = user_data.join(format!("{stem}-{millis}{suffix}"));
    if !candidate.exists() {
        return candidate;
    }
    for n in 1u32.. {
        let next = user_data.join(format!("{stem}-{millis}-{n}{suffix}"));
        if !next.exists() {
            return next;
        }
    }
    candidate
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

    struct FailingProtect;
    impl Protector for FailingProtect {
        fn protect(&self, _data: &[u8]) -> Result<Vec<u8>> {
            bail!("DPAPI unavailable")
        }
        fn unprotect(&self, _data: &[u8]) -> Result<Vec<u8>> {
            bail!("DPAPI unavailable")
        }
    }

    struct FailingUnprotect;
    impl Protector for FailingUnprotect {
        fn protect(&self, data: &[u8]) -> Result<Vec<u8>> {
            Ok(data.iter().map(|b| b ^ 0x5a).collect())
        }
        fn unprotect(&self, _data: &[u8]) -> Result<Vec<u8>> {
            // NTE_BAD_KEY_STATE, what CryptUnprotectData reports after an administrator reset the account password.
            Err(anyhow::Error::new(std::io::Error::from_raw_os_error(0x8009_000Bu32 as i32)).context("Windows data protection failed"))
        }
    }

    struct BusyUnprotect;
    impl Protector for BusyUnprotect {
        fn protect(&self, data: &[u8]) -> Result<Vec<u8>> {
            Ok(data.iter().map(|b| b ^ 0x5a).collect())
        }
        fn unprotect(&self, _data: &[u8]) -> Result<Vec<u8>> {
            // RPC_S_SERVER_UNAVAILABLE: a failure that may pass.
            Err(anyhow::Error::new(std::io::Error::from_raw_os_error(1722)).context("Windows data protection failed"))
        }
    }

    #[test]
    fn creates_then_reads_the_same_key() {
        let dir = tempfile::tempdir().unwrap();
        let first = load_credential_key(dir.path(), &Fake, &|_| {}).unwrap();
        assert!(valid_key(&first.key));
        assert!(first.warnings.is_empty());
        let state: Value = serde_json::from_slice(&fs::read(dir.path().join(LOCAL_STATE)).unwrap()).unwrap();
        assert!(state["os_crypt"]["encrypted_key"].as_str().unwrap().starts_with("RFBBUE"));
        let again = load_credential_key(dir.path(), &Fake, &|_| {}).unwrap();
        assert_eq!(again.key, first.key);
        assert!(again.warnings.is_empty());
        assert_eq!(credential_key(dir.path(), &Fake, &|_| {}).unwrap().as_deref(), Some(first.key.as_str()));
        assert!(fs::read(dir.path().join(KEY_FILE)).unwrap().starts_with(b"v10"));
    }

    #[test]
    fn keeps_other_local_state_fields() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(LOCAL_STATE), br#"{"uninstall_metrics":{"a":1}}"#).unwrap();
        let loaded = load_credential_key(dir.path(), &Fake, &|_| {}).unwrap();
        assert!(loaded.warnings.is_empty());
        let state: Value = serde_json::from_slice(&fs::read(dir.path().join(LOCAL_STATE)).unwrap()).unwrap();
        assert_eq!(state["uninstall_metrics"]["a"], 1);
    }

    #[test]
    fn moves_an_unreadable_key_aside() {
        let dir = tempfile::tempdir().unwrap();
        let first = credential_key(dir.path(), &Fake, &|_| {}).unwrap().unwrap();
        fs::write(dir.path().join(KEY_FILE), b"v10garbage-garbage-garbage-garbage").unwrap();
        let second = load_credential_key(dir.path(), &Fake, &|_| {}).unwrap();
        assert_ne!(first, second.key);
        assert!(matches!(second.warnings.as_slice(), [CredentialWarning::KeyFileRecovered { .. }]));
        assert!(fs::read_dir(dir.path()).unwrap().any(|e| e.unwrap().file_name().to_string_lossy().starts_with("credential-key.unreadable-")));
    }

    #[test]
    fn recovers_corrupted_local_state_under_dpapi_with_warning_never_plaintext() {
        let dir = tempfile::tempdir().unwrap();
        let first = credential_key(dir.path(), &Fake, &|_| {}).unwrap().unwrap();
        // Corrupt Local State JSON (e.g. power loss truncation).
        fs::write(dir.path().join(LOCAL_STATE), b"{not valid json").unwrap();
        let recovered = load_credential_key(dir.path(), &Fake, &|_| {}).unwrap();
        assert!(valid_key(&recovered.key));
        assert_ne!(recovered.key, first);
        assert!(recovered.warnings.iter().any(|w| matches!(w, CredentialWarning::LocalStateRecovered { .. })));
        assert!(recovered.warnings.iter().any(|w| matches!(w, CredentialWarning::KeyFileRecovered { .. })));
        assert!(fs::read_dir(dir.path()).unwrap().any(|e| e.unwrap().file_name().to_string_lossy().starts_with("Local State.unreadable-")));
        // Next read succeeds cleanly with zero warnings using the rebuilt DPAPI Local State.
        let steady = load_credential_key(dir.path(), &Fake, &|_| {}).unwrap();
        assert_eq!(steady.key, recovered.key);
        assert!(steady.warnings.is_empty());
    }

    #[test]
    fn recovers_damaged_os_crypt_key_while_preserving_other_local_state_fields() {
        let dir = tempfile::tempdir().unwrap();
        let first = credential_key(dir.path(), &Fake, &|_| {}).unwrap().unwrap();
        fs::write(dir.path().join(LOCAL_STATE), br#"{"uninstall_metrics":{"kept":true},"os_crypt":{"encrypted_key":"not-base64!!!"}}"#).unwrap();
        let recovered = load_credential_key(dir.path(), &Fake, &|_| {}).unwrap();
        assert_ne!(recovered.key, first);
        assert!(recovered.warnings.iter().any(|w| matches!(w, CredentialWarning::LocalStateRecovered { .. })));
        let state: Value = serde_json::from_slice(&fs::read(dir.path().join(LOCAL_STATE)).unwrap()).unwrap();
        assert_eq!(state["uninstall_metrics"]["kept"], true);
        // Unprotect failure (e.g. Windows account password reset) also recovers cleanly under DPAPI.
        let reset = load_credential_key(dir.path(), &FailingUnprotect, &|_| {}).unwrap();
        assert_ne!(reset.key, recovered.key);
        assert!(reset.warnings.iter().any(|w| matches!(w, CredentialWarning::LocalStateRecovered { .. })));
    }

    #[test]
    fn keeps_the_key_when_dpapi_fails_for_a_while() {
        let dir = tempfile::tempdir().unwrap();
        let first = credential_key(dir.path(), &Fake, &|_| {}).unwrap().unwrap();
        let state = fs::read(dir.path().join(LOCAL_STATE)).unwrap();
        let err = load_credential_key(dir.path(), &BusyUnprotect, &|_| {}).unwrap_err();
        assert!(matches!(err, CredentialError::ProtectionUnavailable { .. }));
        // Nothing was rotated or set aside, so the next run opens the same key.
        assert_eq!(fs::read(dir.path().join(LOCAL_STATE)).unwrap(), state);
        assert!(!fs::read_dir(dir.path()).unwrap().any(|e| e.unwrap().file_name().to_string_lossy().contains(".unreadable")));
        assert_eq!(credential_key(dir.path(), &Fake, &|_| {}).unwrap().as_deref(), Some(first.as_str()));
    }

    #[test]
    fn refuses_plaintext_fallback_when_dpapi_is_unavailable() {
        let dir = tempfile::tempdir().unwrap();
        let original = b"{broken local state";
        fs::write(dir.path().join(LOCAL_STATE), original).unwrap();
        let err = load_credential_key(dir.path(), &FailingProtect, &|_| {}).unwrap_err();
        assert!(matches!(err, CredentialError::ProtectionUnavailable { .. }));
        // Compatibility wrapper must also return Err, never Ok(None).
        assert!(credential_key(dir.path(), &FailingProtect, &|_| {}).is_err());
        // Existing damaged file is left untouched when DPAPI cannot protect a replacement.
        assert_eq!(fs::read(dir.path().join(LOCAL_STATE)).unwrap(), original);
    }

    #[test]
    fn seals_and_opens() {
        let key = [7u8; 32];
        assert_eq!(open(&key, &seal(&key, "密钥").unwrap()).unwrap(), "密钥");
        assert!(open(&[8u8; 32], &seal(&key, "x").unwrap()).is_err());
    }
}
