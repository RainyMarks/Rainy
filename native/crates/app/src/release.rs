//! The public keys Rainy's release files are signed with, compiled in by `build.rs` from
//! `runtime/release-public-keys.json` or `RAINY_RELEASE_PUBLIC_KEYS`.

use std::sync::OnceLock;

use rainy_carrier::keyring::Keyring;

include!(concat!(env!("OUT_DIR"), "/release_keys.rs"));

/// The keys installed resources must be signed with; `None` for a build made without them (development without
/// `runtime/release-public-keys.json`, or `RAINY_ALLOW_UNSIGNED=1`), which checks no resources.
pub fn keyring() -> Option<&'static Keyring> {
    static KEYRING: OnceLock<Option<Keyring>> = OnceLock::new();
    KEYRING.get_or_init(|| RELEASE_KEYS.map(Keyring::from_keys)).as_ref()
}
