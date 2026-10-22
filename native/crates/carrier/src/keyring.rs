//! The public keys that sign Rainy's release files: `release-public-keys.json` (`release-trust.ts` in 3.x).
//! `app/build.rs` compiles this file as well to check the keyring it embeds, so it uses only std, anyhow, base64,
//! serde_json and sha2.

use std::collections::BTreeMap;

use anyhow::{Context as _, Result, bail, ensure};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde_json::Value;
use sha2::{Digest as _, Sha256};

/// The DER encoding of an Ed25519 public key (SubjectPublicKeyInfo) starts with these bytes; the 32-byte key follows.
const ED25519_SPKI: [u8; 12] = [0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00];
const PEM_BEGIN: &str = "-----BEGIN PUBLIC KEY-----";
const PEM_END: &str = "-----END PUBLIC KEY-----";

/// Ed25519 release keys by key id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Keyring(BTreeMap<String, [u8; 32]>);

impl Keyring {
    /// Read `{"version": 1, "keys": {"<id>": "<PEM public key>"}}` as strictly as `parseReleaseKeyring`: no other
    /// fields, at least one key, Ed25519 public keys only, each under its own key id.
    pub fn parse(text: &str) -> Result<Self> {
        let value: Value = serde_json::from_str(text).context("The release keyring is not valid JSON")?;
        let object = value.as_object().context("The release keyring is not a JSON object")?;
        if let Some(field) = object.keys().find(|field| !matches!(field.as_str(), "version" | "keys")) {
            bail!("The release keyring has an unknown field {field:?}");
        }
        ensure!(object.get("version").and_then(Value::as_u64) == Some(1), "The release keyring is not version 1");
        let entries = object.get("keys").and_then(Value::as_object).context("The release keyring has no keys object")?;
        ensure!(!entries.is_empty(), "The installation has no key to verify its files.");
        let mut keys = BTreeMap::new();
        for (id, pem) in entries {
            ensure!(id.len() == 32 && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')), "A release key has an invalid ID.");
            let pem = pem.as_str().filter(|pem| pem.len() <= 4096).context("A release key is not a PEM text")?;
            ensure!(pem.starts_with(PEM_BEGIN), "The installation may contain only public release keys.");
            let key = public_key(pem)?;
            ensure!(key_id(&key) == *id, "A release key has an invalid ID.");
            keys.insert(id.clone(), key);
        }
        Ok(Self(keys))
    }

    /// Keys already checked, as `app/build.rs` embeds them.
    pub fn from_keys(keys: &[(&str, [u8; 32])]) -> Self {
        Self(keys.iter().map(|(id, key)| ((*id).to_owned(), *key)).collect())
    }

    /// The key a release file names by its id.
    pub fn get(&self, id: &str) -> Option<&[u8; 32]> {
        self.0.get(id)
    }

    /// Every key with its id, ordered by id.
    pub fn keys(&self) -> impl Iterator<Item = (&str, &[u8; 32])> {
        self.0.iter().map(|(id, key)| (id.as_str(), key))
    }
}

/// The id release files use for a key: the first 32 hex digits of the SHA-256 of its DER encoding.
pub fn key_id(key: &[u8; 32]) -> String {
    let mut der = ED25519_SPKI.to_vec();
    der.extend_from_slice(key);
    Sha256::digest(&der).iter().take(16).map(|byte| format!("{byte:02x}")).collect()
}

/// The raw key of a PEM-encoded Ed25519 public key.
fn public_key(pem: &str) -> Result<[u8; 32]> {
    let (body, _) = pem
        .strip_prefix(PEM_BEGIN)
        .and_then(|rest| rest.split_once(PEM_END))
        .context("A release key is not a complete PEM text")?;
    let der = STANDARD
        .decode(body.split_ascii_whitespace().collect::<String>())
        .context("A release key is not valid base64")?;
    match der.strip_prefix(&ED25519_SPKI[..]) {
        Some(key) if key.len() == 32 => Ok(key.try_into().expect("32 bytes")),
        _ => bail!("Release keys must use Ed25519."),
    }
}

#[cfg(test)]
mod tests {
    use ::core::prelude::v1::test;

    use super::*;

    /// The key that signs Rainy 3.x and 4.0 releases.
    const KEYRING: &str = "{\n  \"version\": 1,\n  \"keys\": {\n    \"357ac484b238cb4646a884d148204905\": \"-----BEGIN PUBLIC KEY-----\\nMCowBQYDK2VwAyEADn2d4sKUejaUJ87nCxFTMsw8B+f24zPeIpilP9IOZyo=\\n-----END PUBLIC KEY-----\\n\"\n  }\n}\n";
    const RAW: [u8; 32] = [
        0x0e, 0x7d, 0x9d, 0xe2, 0xc2, 0x94, 0x7a, 0x36, 0x94, 0x27, 0xce, 0xe7, 0x0b, 0x11, 0x53, 0x32, 0xcc, 0x3c, 0x07, 0xe7,
        0xf6, 0xe3, 0x33, 0xde, 0x22, 0x98, 0xa5, 0x3f, 0xd2, 0x0e, 0x67, 0x2a,
    ];

    #[test]
    fn reads_the_release_key() {
        let keyring = Keyring::parse(KEYRING).unwrap();
        assert_eq!(keyring.get("357ac484b238cb4646a884d148204905"), Some(&RAW));
        assert_eq!(keyring.keys().count(), 1);
        assert_eq!(key_id(&RAW), "357ac484b238cb4646a884d148204905");
        assert_eq!(Keyring::from_keys(&[("357ac484b238cb4646a884d148204905", RAW)]), keyring);
    }

    #[test]
    fn rejects_what_3x_rejects() {
        let pem = "-----BEGIN PUBLIC KEY-----\nMCowBQYDK2VwAyEADn2d4sKUejaUJ87nCxFTMsw8B+f24zPeIpilP9IOZyo=\n-----END PUBLIC KEY-----\n";
        let keyring = |version: Value, keys: Value| serde_json::json!({ "version": version, "keys": keys }).to_string();
        let error = |text: &str| Keyring::parse(text).unwrap_err().to_string();
        assert_eq!(error(&keyring(1.into(), serde_json::json!({}))), "The installation has no key to verify its files.");
        assert!(error(&keyring(2.into(), serde_json::json!({ "357ac484b238cb4646a884d148204905": pem }))).contains("version 1"));
        assert!(error(&serde_json::json!({ "version": 1, "keys": { "357ac484b238cb4646a884d148204905": pem }, "extra": 1 }).to_string()).contains("extra"));
        // An id that is not the key's own, or not 32 lowercase hex digits.
        assert_eq!(error(&keyring(1.into(), serde_json::json!({ "457ac484b238cb4646a884d148204905": pem }))), "A release key has an invalid ID.");
        assert_eq!(error(&keyring(1.into(), serde_json::json!({ "357AC484B238CB4646A884D148204905": pem }))), "A release key has an invalid ID.");
        let private = "-----BEGIN PRIVATE KEY-----\nMC4CAQAwBQYDK2VwBCIEIA==\n-----END PRIVATE KEY-----\n";
        assert_eq!(error(&keyring(1.into(), serde_json::json!({ "357ac484b238cb4646a884d148204905": private }))), "The installation may contain only public release keys.");
        // A P-256 key is a public key, but not an Ed25519 one.
        let p256 = "-----BEGIN PUBLIC KEY-----\nMFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAEAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA==\n-----END PUBLIC KEY-----\n";
        assert_eq!(error(&keyring(1.into(), serde_json::json!({ "357ac484b238cb4646a884d148204905": p256 }))), "Release keys must use Ed25519.");
        assert!(Keyring::parse("not json").is_err());
    }
}
