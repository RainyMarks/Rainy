use ::core::prelude::v1::test;

use ring::rand::SystemRandom;
use ring::signature::{Ed25519KeyPair, KeyPair as _};

use super::*;
use crate::keyring::key_id;

/// `release/latest.yml` of 3.9.1, byte for byte, as electron-builder 26.15.3 wrote it.
const LATEST_3_9_1: &str = "version: 3.9.1\nfiles:\n  - url: Rainy-3.9.1-windows-x64-setup.exe\n    sha512: L8s/JYn7p3o+1bvmDfpMqsqwNIDRrlOa7mCBrhM2bBGEFQgoeXXue3C2DyO2iQU7d/g01824G/H7kagnYvfwKg==\n    size: 180169836\npath: Rainy-3.9.1-windows-x64-setup.exe\nsha512: L8s/JYn7p3o+1bvmDfpMqsqwNIDRrlOa7mCBrhM2bBGEFQgoeXXue3C2DyO2iQU7d/g01824G/H7kagnYvfwKg==\nreleaseDate: '2026-10-21T23:56:21.008Z'\n";
const SHA512_3_9_1: &str = "L8s/JYn7p3o+1bvmDfpMqsqwNIDRrlOa7mCBrhM2bBGEFQgoeXXue3C2DyO2iQU7d/g01824G/H7kagnYvfwKg==";

fn version(text: &str) -> Version {
    Version::parse(text).unwrap()
}

/// A release key made for one test.
struct Signer(Ed25519KeyPair);

impl Signer {
    fn new() -> Self {
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).unwrap();
        Self(Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap())
    }

    fn keyring(&self) -> Keyring {
        let public: [u8; 32] = self.0.public_key().as_ref().try_into().unwrap();
        Keyring::from_keys(&[(key_id(&public).as_str(), public)])
    }

    /// `latest.yml.sig` for `feed`, as the release script writes it: base64url without padding and a newline.
    fn sign(&self, domain: &[u8], feed: &[u8]) -> Vec<u8> {
        format!("{}\n", URL_SAFE_NO_PAD.encode(self.0.sign(&[domain, feed].concat()).as_ref())).into_bytes()
    }
}

#[test]
fn reads_the_latest_yml_of_3_9_1() {
    let feed = parse_feed(LATEST_3_9_1).unwrap();
    assert_eq!(feed.version, version("3.9.1"));
    assert_eq!(
        feed.files,
        [FeedFile { url: "Rainy-3.9.1-windows-x64-setup.exe".into(), sha512: SHA512_3_9_1.into(), size: Some(180_169_836), block_map_size: None }]
    );
    assert_eq!(feed.path.as_deref(), Some("Rainy-3.9.1-windows-x64-setup.exe"));
    assert_eq!(feed.sha512.as_deref(), Some(SHA512_3_9_1));
    assert_eq!(feed.release_date.as_deref(), Some("2026-10-21T23:56:21.008Z"));
    assert_eq!(feed.minimum_system_version, None);
    let installer = feed.installer(&Source::github()).unwrap();
    assert_eq!(installer.url, "https://github.com/RainyMarks/Rainy/releases/download/v3.9.1/Rainy-3.9.1-windows-x64-setup.exe");
    assert_eq!(installer.file, "Rainy-3.9.1-windows-x64-setup.exe");
    assert_eq!(installer.expected, Expected { bytes: 180_169_836, digest: Digest::sha512_base64(SHA512_3_9_1).unwrap() });
}

/// The one electron-builder wrote last in the main checkout, when it is there.
#[test]
fn reads_the_latest_yml_of_the_main_checkout() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../../RainyAgent/release/latest.yml");
    let Ok(text) = fs::read_to_string(&path) else { return };
    let feed = parse_feed(&text).unwrap();
    let installer = feed.installer(&Source::github()).unwrap();
    assert!(installer.file.starts_with(&format!("Rainy-{}-", feed.version)), "{}", installer.file);
    assert_eq!(feed.path.as_deref(), Some(installer.file.as_str()));
}

#[test]
fn tolerates_quoting_comments_and_unknown_keys() {
    let text = "\u{feff}---\r\n# written by hand\r\nversion: \"4.0.1\"   # the release\r\nreleaseName: 'Rainy 4.0.1'\r\nreleaseNotes: |\r\n  - fixed: things\r\n  version: 9.9.9\r\nstagingPercentage: 50\r\nfiles:\r\n- url: 'Rainy-4.0.1-windows-arm64-setup.exe'\r\n  sha512: \"AAAA\"\r\n  size: 1\r\n-\r\n  url: \"Rainy 4.0.1 windows x64 setup.exe\"\r\n  sha512: 'L8s/JYn7p3o+1bvmDfpMqsqwNIDRrlOa7mCBrhM2bBGEFQgoeXXue3C2DyO2iQU7d/g01824G/H7kagnYvfwKg=='\r\n  size: '42'\r\n  blockMapSize: 7\r\n  isAdminRightsRequired: false\r\n  extra:\r\n    nested: [1, 2]\r\n'path': Rainy-4.0.1-windows-x64-setup.exe\r\nminimumSystemVersion: 10.0.19041\r\nreleaseDate: \"2026-11-01T00:00:00.000Z\"\r\n...\r\nignored: after the end\r\n";
    let feed = parse_feed(text).unwrap();
    assert_eq!(feed.version, version("4.0.1"));
    assert_eq!(feed.files.len(), 2);
    assert_eq!(feed.files[1], FeedFile { url: "Rainy 4.0.1 windows x64 setup.exe".into(), sha512: SHA512_3_9_1.into(), size: Some(42), block_map_size: Some(7) });
    assert_eq!(feed.path.as_deref(), Some("Rainy-4.0.1-windows-x64-setup.exe"));
    assert_eq!(feed.minimum_system_version.as_deref(), Some("10.0.19041"));
    assert_eq!(feed.release_date.as_deref(), Some("2026-11-01T00:00:00.000Z"));
    // The x64 installer, named as electron-updater requests it.
    let installer = feed.installer(&Source::at("http://127.0.0.1:9/feed/latest.yml?x=1")).unwrap();
    assert_eq!(installer.file, "Rainy-4.0.1-windows-x64-setup.exe");
    assert_eq!(installer.url, "http://127.0.0.1:9/feed/Rainy-4.0.1-windows-x64-setup.exe");
    assert_eq!(installer.expected.bytes, 42);
    // Escapes in double quotes, and a `v` as in a tag.
    let feed = parse_feed("version: v4.2.0\nfiles:\n  - url: \"R\\x61iny-\\u0034.exe\"\n    sha512: x\n").unwrap();
    assert_eq!((feed.version, feed.files[0].url.as_str()), (version("4.2.0"), "Rainy-4.exe"));
    assert!(parse_feed("version: '4.0.0'\nnote: 'it''s'\n").unwrap().files.is_empty());
}

#[test]
fn refuses_web_installers() {
    let text = format!("{LATEST_3_9_1}packages:\n  x64:\n    path: Rainy-3.9.1-x64.nsis.7z\n");
    assert_eq!(parse_feed(&text), Err(UpdateError::WebInstaller));
    assert_eq!(parse_feed("packages: {}\n"), Err(UpdateError::WebInstaller));
}

#[test]
fn refuses_what_electron_builder_does_not_write() {
    let refused = |text: &str| assert!(matches!(parse_feed(text), Err(UpdateError::Invalid(_))), "{text:?} → {:?}", parse_feed(text));
    refused("");
    refused("files:\n  - url: a.exe\n    sha512: x\n");
    refused("version: 4.0\n");
    refused("version: 4.0.0\nversion: 4.0.1\n");
    refused("version: '4.0.0\n");
    refused("version: \"4.0.0\" trailing\n");
    refused("version: |\n  4.0.0\n");
    refused("version: [4, 0, 0]\n");
    refused("version: 4.0.0\nfiles: []\n");
    refused("version: 4.0.0\nfiles:\n  url: a.exe\n");
    refused("version: 4.0.0\nfiles:\n  - url: a.exe\n");
    refused("version: 4.0.0\nfiles:\n  - url: a.exe\n    sha512: x\n    size: -1\n");
    refused("version: 4.0.0\nfiles:\n  - url: a.exe\n    sha512: x\n    url: b.exe\n");
    refused("version: 4.0.0\nfiles:\n  - - url: a.exe\n");
    refused("version: 4.0.0\n\tfiles:\n");
    refused("version: 4.0.0\njust text\n");
    refused("  version: 4.0.0\nfiles:\n");
    refused("version: 4.0.0\n---\nversion: 4.0.1\n");
    let installer = |files: &str| parse_feed(&format!("version: 4.0.1\nfiles:\n{files}")).unwrap().installer(&Source::github());
    let good = format!("    sha512: {SHA512_3_9_1}\n    size: 5\n");
    assert!(installer(&format!("  - url: Rainy-4.0.1-windows-x64-setup.exe\n{good}")).is_ok());
    for bad in [
        format!("  - url: Rainy.zip\n{good}"),
        format!("  - url: ../Rainy.exe\n{good}"),
        format!("  - url: dir/Rainy.exe\n{good}"),
        format!("  - url: https://example.com/Rainy.exe\n{good}"),
        format!("  - url: CON.exe\n{good}"),
        "  - url: Rainy.exe\n    sha512: AAAA\n    size: 5\n".into(),
        format!("  - url: Rainy.exe\n    sha512: {SHA512_3_9_1}\n"),
        format!("  - url: Rainy.exe\n    sha512: {SHA512_3_9_1}\n    size: 0\n"),
        format!("  - url: Rainy.exe\n    sha512: {SHA512_3_9_1}\n    size: 3000000000\n"),
    ] {
        assert!(matches!(installer(&bad), Err(UpdateError::Invalid(_))), "{bad}");
    }
}

#[test]
fn offers_only_newer_stable_versions() {
    let current = version("4.0.0");
    assert!(offered(&version("4.0.1"), &current));
    assert!(offered(&version("5.0.0"), &current));
    assert!(!offered(&version("4.0.0"), &current));
    assert!(!offered(&version("3.9.1"), &current));
    assert!(!offered(&version("4.1.0-beta.1"), &current));
    // Build metadata does not make a version newer.
    assert!(!offered(&version("4.0.0+20261101"), &current));
    assert!(offered(&version("4.1.0"), &version("4.1.0-beta.2")));
    assert_eq!(parse_version("v4.0.1"), Some(version("4.0.1")));
    assert_eq!(parse_version("4.0"), None);
    assert!(supported(None, Some(&version("10.0.19045"))));
    assert!(supported(Some("10.0.19041"), Some(&version("10.0.19045"))));
    assert!(supported(Some("10.0.19045"), Some(&version("10.0.19045"))));
    assert!(!supported(Some("10.0.22000"), Some(&version("10.0.19045"))));
    assert!(supported(Some("not a version"), Some(&version("10.0.19045"))));
    assert!(supported(Some("10.0.22000"), None));
    let windows = win32::os_version();
    assert!(cfg!(not(windows)) || windows.is_some_and(|windows| windows.major >= 10));
}

#[test]
fn checks_signatures_of_latest_yml() {
    let signer = Signer::new();
    let keyring = signer.keyring();
    let feed = LATEST_3_9_1.as_bytes();
    let signature = signer.sign(SIGNATURE_DOMAIN, feed);
    assert!(signature_valid(&keyring, feed, &signature));
    // Padding and surrounding spaces are tolerated.
    let padded = format!("  {}==\r\n", String::from_utf8_lossy(&signature).trim());
    assert!(signature_valid(&keyring, feed, padded.as_bytes()));
    // Another file, another domain, another key, or no signature at all.
    let mut changed = feed.to_vec();
    changed[9] = b'2';
    assert!(!signature_valid(&keyring, &changed, &signature));
    assert!(!signature_valid(&keyring, feed, &signer.sign(trust::DOMAIN, feed)));
    assert!(!signature_valid(&Signer::new().keyring(), feed, &signature));
    assert!(!signature_valid(&keyring, feed, b""));
    assert!(!signature_valid(&keyring, feed, b"not base64!"));
    assert!(!signature_valid(&keyring, feed, URL_SAFE_NO_PAD.encode([0u8; 64]).as_bytes()));
    assert!(!signature_valid(&keyring, feed, STANDARD.encode(&signature[..signature.len() - 1]).as_bytes()));
}

#[test]
fn finds_the_files_of_a_release() {
    let github = Source::github();
    assert!(!github.overridden() && !github.allow_loopback_http);
    assert_eq!(github.file_url(&version("4.0.1"), SIGNATURE_FILE), "https://github.com/RainyMarks/Rainy/releases/download/v4.0.1/latest.yml.sig");
    let test = Source::at("http://127.0.0.1:8080/qa/feed/latest.yml#top");
    assert!(test.overridden() && test.allow_loopback_http);
    assert_eq!(test.file_url(&version("4.0.1"), SIGNATURE_FILE), "http://127.0.0.1:8080/qa/feed/latest.yml.sig");
    assert!(test.options(Arc::new(AtomicBool::new(false))).allow_loopback_http);
    assert!(!github.options(Arc::new(AtomicBool::new(false))).allow_loopback_http);
}

#[test]
fn uses_the_updater_cache_of_3x_only_for_github() {
    assert_eq!(cache_dir_from(None, None, Some("C:/Users/me/AppData/Local".into())), Some(PathBuf::from("C:/Users/me/AppData/Local/rainy-agent-updater/pending")));
    assert_eq!(cache_dir_from(Some("".into()), None, Some("C:/L".into())), Some(PathBuf::from("C:/L/rainy-agent-updater/pending")));
    assert_eq!(cache_dir_from(Some("C:/qa/cache".into()), None, Some("C:/L".into())), Some(PathBuf::from("C:/qa/cache")));
    assert_eq!(cache_dir_from(None, None, None), None);
    // A test feed keeps its installers in the user data folder unless a cache is named.
    let test = Some(PathBuf::from("C:/ud/update-cache"));
    assert_eq!(cache_dir_from(None, test.clone(), Some("C:/L".into())), test);
    assert_eq!(cache_dir_from(Some("C:/qa/cache".into()), test, Some("C:/L".into())), Some(PathBuf::from("C:/qa/cache")));
}

#[test]
fn starts_the_installer_as_3x_did() {
    let line = command_line(Path::new("C:\\Users\\me\\AppData\\Local\\rainy-agent-updater\\pending\\Rainy-4.0.1-windows-x64-setup.exe"));
    assert_eq!(line, "\"C:\\Users\\me\\AppData\\Local\\rainy-agent-updater\\pending\\Rainy-4.0.1-windows-x64-setup.exe\" --updated /S --force-run");
    // As Rainy quits the update installs silently and Rainy stays closed.
    assert_eq!(command_line_with(Path::new("C:\\u\\setup.exe"), &QUIT_ARGUMENTS), "\"C:\\u\\setup.exe\" --updated /S");
}

#[test]
fn detects_per_machine_install_scope_for_quit_uac_avoidance() {
    // Only HKLM present -> NSIS selects $installMode == "all" and elevates via UAC.
    assert_eq!(install_scope_from(None, Some(r"C:\Program Files\RainyAgent"), None), InstallScope::PerMachine);
    assert_eq!(install_scope_from(Some(""), Some(r"C:\Program Files\RainyAgent"), None), InstallScope::PerMachine);
    // Only HKCU present -> per-user, silent quit update does not prompt for UAC.
    assert_eq!(install_scope_from(Some(r"C:\Users\u\AppData\Local\Programs\RainyAgent"), None, None), InstallScope::PerUser);
    // Both present: running from the HKLM installation dir counts as PerMachine; running from HKCU counts as PerUser.
    assert_eq!(
        install_scope_from(
            Some(r"C:\Users\u\AppData\Local\Programs\RainyAgent"),
            Some(r"C:\Program Files\RainyAgent\"),
            Some(Path::new(r"c:/program files/rainyagent")),
        ),
        InstallScope::PerMachine
    );
    // Verbatim `\\?\` paths (from current_exe or canonicalized paths) must still match HKLM.
    assert_eq!(
        install_scope_from(
            Some(r"C:\Users\u\AppData\Local\Programs\RainyAgent"),
            Some(r"C:\Program Files\RainyAgent"),
            Some(Path::new(r"\\?\C:\Program Files\RainyAgent")),
        ),
        InstallScope::PerMachine
    );
    assert_eq!(
        install_scope_from(
            Some(r"C:\Users\u\AppData\Local\Programs\RainyAgent"),
            Some(r"C:\Program Files\RainyAgent"),
            Some(Path::new(r"C:\Users\u\AppData\Local\Programs\RainyAgent")),
        ),
        InstallScope::PerUser
    );
    // UninstallString quoted folder extraction mirrors rainy.nsi `rainyQuotedFolder`.
    assert_eq!(
        quoted_folder(r#""C:\Program Files\RainyAgent\Uninstall RainyAgent.exe" /allusers"#).as_deref(),
        Some(r"C:\Program Files\RainyAgent")
    );
    assert_eq!(quoted_folder("unquoted.exe"), None);
}

#[test]
fn validates_installer_before_launch() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("Rainy-setup.exe");
    assert!(validate_installer_path(&missing).is_err());
    let empty = dir.path().join("empty.exe");
    fs::write(&empty, b"").unwrap();
    assert!(validate_installer_path(&empty).is_err());
    let non_exe = dir.path().join("script.bat");
    fs::write(&non_exe, b"@echo hi").unwrap();
    assert!(validate_installer_path(&non_exe).is_err());
    let ok = dir.path().join("Rainy-4.12.0-windows-x64-setup.exe");
    fs::write(&ok, b"MZ").unwrap();
    assert!(validate_installer_path(&ok).is_ok());
}

/// Record `file` of `release` as this updater does, with a placeholder digest.
fn record(cache: &Path, release: &str, file: &str) {
    let record = Record { version: 1, release: release.into(), file: file.into(), sha512: SHA512_3_9_1.into(), size: 3 };
    fs::write(cache.join(RECORD_FILE), serde_json::to_string(&record).unwrap()).unwrap();
}

#[test]
fn removes_only_what_this_updater_left() {
    let keep = "Rainy-4.0.2-windows-x64-setup.exe";
    for name in [
        "Rainy-4.0.1-windows-x64-setup.exe.partial",
        "RAINY-4.0.0-windows-x64-setup.exe.partial",
        "Rainy-4.0.1-windows-x64-setup.exe.partial.0",
        "RAINY-4.0.0-windows-x64-setup.exe.partial.5",
    ] {
        assert!(unfinished(name, keep), "{name}");
    }
    for name in [
        "rainy-4.0.2-windows-x64-setup.exe.partial",
        "rainy-4.0.2-windows-x64-setup.exe.partial.0",
        "Rainy-4.0.1-windows-x64-setup.exe.partial.bak",
        "Rainy-4.0.1-windows-x64-setup.exe",
        "temp-Rainy-4.0.1-windows-x64-setup.exe",
        "temp-Rainy-4.0.1-windows-x64-setup.exe.partial",
        "Other-setup.exe.partial",
        "Rainy-notes.txt.partial",
        RECORD_FILE,
    ] {
        assert!(!unfinished(name, keep), "{name}");
    }

    let cache = tempfile::tempdir().unwrap();
    let file = |name: &str| cache.path().join(name);
    // electron-updater's: its installers, the one being downloaded, its record and blockmap.
    let electron_updater = ["RainyAgent-2.1.1-windows-x64-setup.exe", "Rainy-3.8.0-windows-x64-setup.exe", "temp-Rainy-4.0.2-windows-x64-setup.exe", "current.blockmap"];
    for name in electron_updater.into_iter().chain([
        "Rainy-4.0.1-windows-x64-setup.exe",
        "Rainy-4.0.0-windows-x64-setup.exe.partial",
        "Rainy-4.0.0-windows-x64-setup.exe.partial.0",
        "Rainy-4.0.0-windows-x64-setup.exe.partial.3",
        keep,
    ]) {
        fs::write(file(name), b"x").unwrap();
    }
    fs::write(file(ELECTRON_UPDATER_RECORD), r#"{"fileName":"RainyAgent-2.1.1-windows-x64-setup.exe","sha512":"x","isAdminRightsRequired":false}"#).unwrap();
    record(cache.path(), "4.0.1", "Rainy-4.0.1-windows-x64-setup.exe");
    assert_eq!(recorded_file(cache.path()).as_deref(), Some("Rainy-4.0.1-windows-x64-setup.exe"));
    remove_earlier(cache.path(), recorded_file(cache.path()).as_deref(), keep);
    assert!(!file("Rainy-4.0.1-windows-x64-setup.exe").exists());
    assert!(!file("Rainy-4.0.0-windows-x64-setup.exe.partial").exists());
    assert!(!file("Rainy-4.0.0-windows-x64-setup.exe.partial.0").exists());
    assert!(!file("Rainy-4.0.0-windows-x64-setup.exe.partial.3").exists());
    for name in electron_updater.into_iter().chain([keep, ELECTRON_UPDATER_RECORD]) {
        assert!(file(name).exists(), "{name}");
    }
    // The file electron-updater prepared is kept even when this updater recorded the same name.
    record(cache.path(), "2.1.1", "RainyAgent-2.1.1-windows-x64-setup.exe");
    remove_earlier(cache.path(), recorded_file(cache.path()).as_deref(), keep);
    assert!(file("RainyAgent-2.1.1-windows-x64-setup.exe").exists());
    assert_eq!(pending(cache.path(), &version("4.0.0"), &AtomicBool::new(false)), None);
    assert!(file("RainyAgent-2.1.1-windows-x64-setup.exe").exists());
    assert!(!file(RECORD_FILE).exists());
    // Without a valid record nothing is recorded to remove.
    fs::write(file(RECORD_FILE), "{}").unwrap();
    assert_eq!(recorded_file(cache.path()), None);
}

#[cfg(windows)]
mod network {
    use ::core::prelude::v1::test;
    use std::sync::Mutex;

    use ring::digest::{SHA512, digest};

    use super::super::*;
    use super::{LATEST_3_9_1, Signer, version};
    use crate::test_server::{Reply, Server};

    const SETUP: &str = "Rainy-4.0.1-windows-x64-setup.exe";

    fn installer_bytes() -> Vec<u8> {
        (0..300_000).map(|index| (index * 13 % 251) as u8).collect()
    }

    fn latest_yml(release: &str, setup: &[u8]) -> String {
        let sha512 = STANDARD.encode(digest(&SHA512, setup).as_ref());
        format!(
            "version: {release}\nfiles:\n  - url: {SETUP}\n    sha512: {sha512}\n    size: {}\npath: {SETUP}\nsha512: {sha512}\nreleaseDate: '2026-11-01T00:00:00.000Z'\n",
            setup.len()
        )
    }

    /// A release folder: `latest.yml`, its signature when given, and the installer.
    fn serve(feed: String, signature: Option<Vec<u8>>, setup: Vec<u8>) -> Server {
        Server::start(move |seen| match seen.path.as_str() {
            "/feed/latest.yml" => Reply::ok(feed.clone().into_bytes()),
            "/feed/latest.yml.sig" => signature.clone().map_or_else(|| Reply::status(404), Reply::ok),
            path if path == format!("/feed/{SETUP}") => Reply::ok(setup.clone()),
            _ => Reply::status(404),
        })
    }

    fn options() -> Options {
        Source::at("http://127.0.0.1/").options(Arc::new(AtomicBool::new(false)))
    }

    fn paths(server: &Server) -> Vec<String> {
        server.seen().into_iter().map(|seen| seen.path).collect()
    }

    /// The whole flow as Rainy runs it, with the feed and the cache named by the environment as QA names them.
    #[test]
    fn checks_downloads_and_offers_a_signed_release() {
        let setup = installer_bytes();
        let feed = latest_yml("4.0.1", &setup);
        let signer = Signer::new();
        let server = serve(feed.clone(), Some(signer.sign(SIGNATURE_DOMAIN, feed.as_bytes())), setup.clone());
        let (cache, user_data) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        fs::write(cache.path().join("Rainy-3.9.1-windows-x64-setup.exe"), b"old").unwrap();
        fs::write(cache.path().join("update-info.json"), b"{}").unwrap();
        fs::write(cache.path().join("Rainy-4.0.0-windows-x64-setup.exe"), b"recorded").unwrap();
        super::record(cache.path(), "4.0.0", "Rainy-4.0.0-windows-x64-setup.exe");
        // SAFETY: Windows serializes access to the environment, and no other test reads these two variables.
        unsafe {
            std::env::set_var(FEED_VARIABLE, server.url("/feed/latest.yml"));
            std::env::set_var(CACHE_VARIABLE, cache.path());
        }
        let (source, folder) = (Source::from_env(), cache_dir(&Source::from_env(), user_data.path()).unwrap());
        // SAFETY: as above.
        unsafe { std::env::remove_var(CACHE_VARIABLE) };
        let unnamed = cache_dir(&source, user_data.path());
        // SAFETY: as above.
        unsafe { std::env::remove_var(FEED_VARIABLE) };
        assert_eq!(folder, cache.path());
        assert!(source.overridden());
        // Without a named cache, a test feed never uses 3.x's folder.
        assert_eq!(unnamed, Some(user_data.path().join("update-cache")));
        assert_eq!(cache_dir(&Source::from_env(), user_data.path()), cache_dir_from(None, None, std::env::var_os("LOCALAPPDATA")));
        let current = version("4.0.0");
        let options = source.options(Arc::new(AtomicBool::new(false)));
        let installer = check(&source, &current, Some(&signer.keyring()), &options).unwrap().unwrap();
        assert_eq!(installer.version, version("4.0.1"));
        assert_eq!(installer.url, server.url(&format!("/feed/{SETUP}")));
        let seen = Mutex::new(Vec::new());
        let prepared = download(&installer, &folder, &options, &|done, total| seen.lock().unwrap().push((done, total))).unwrap();
        assert_eq!(prepared.path, folder.join(SETUP));
        assert_eq!(fs::read(&prepared.path).unwrap(), setup);
        assert_eq!(seen.lock().unwrap().last(), Some(&(300_000, 300_000)));
        assert_eq!(paths(&server), ["/feed/latest.yml", "/feed/latest.yml.sig", format!("/feed/{SETUP}").as_str()]);
        // The installer recorded before is gone; what electron-updater keeps beside it stays.
        assert!(!folder.join("Rainy-4.0.0-windows-x64-setup.exe").exists());
        assert!(folder.join("Rainy-3.9.1-windows-x64-setup.exe").exists());
        assert!(folder.join("update-info.json").exists());
        assert!(prepared.verify(&AtomicBool::new(false)).unwrap());

        // A later start offers it without the network, and a second download reuses it.
        assert_eq!(pending(&folder, &current, &AtomicBool::new(false)), Some(prepared.clone()));
        assert_eq!(download(&installer, &folder, &options, &|_, _| {}).unwrap(), prepared);
        assert_eq!(server.seen().len(), 3);
        // A changed file is no longer offered, nor accepted before installing.
        fs::write(&prepared.path, b"changed").unwrap();
        assert_eq!(pending(&folder, &current, &AtomicBool::new(false)), None);
        assert!(!prepared.verify(&AtomicBool::new(false)).unwrap());
        download(&installer, &folder, &options, &|_, _| {}).unwrap();
        assert_eq!(fs::read(&prepared.path).unwrap(), setup);
        // Once 4.0.1 runs, its installer and the record are deleted.
        assert_eq!(pending(&folder, &version("4.0.1"), &AtomicBool::new(false)), None);
        assert!(!prepared.path.exists());
        assert!(!folder.join(RECORD_FILE).exists());
    }

    #[test]
    fn refuses_a_release_with_an_invalid_signature() {
        let setup = installer_bytes();
        let feed = latest_yml("4.0.1", &setup);
        let signer = Signer::new();
        // Signed by another key, and the signature of another file.
        for signature in [Signer::new().sign(SIGNATURE_DOMAIN, feed.as_bytes()), signer.sign(SIGNATURE_DOMAIN, LATEST_3_9_1.as_bytes()), b"garbage".to_vec()] {
            let server = serve(feed.clone(), Some(signature), setup.clone());
            let source = Source::at(&server.url("/feed/latest.yml"));
            assert_eq!(check(&source, &version("4.0.0"), Some(&signer.keyring()), &options()), Err(UpdateError::Signature));
            assert_eq!(paths(&server), ["/feed/latest.yml", "/feed/latest.yml.sig"]);
        }
    }

    #[test]
    fn refuses_an_unsigned_release() {
        // Leaving the signature out must not be a way around it.
        let setup = installer_bytes();
        let server = serve(latest_yml("4.0.1", &setup), None, setup.clone());
        let source = Source::at(&server.url("/feed/latest.yml"));
        assert_eq!(check(&source, &version("4.0.0"), Some(&Signer::new().keyring()), &options()), Err(UpdateError::Signature));
        assert_eq!(paths(&server), ["/feed/latest.yml", "/feed/latest.yml.sig"]);
        // A build without keys does not ask for the signature.
        let server = serve(latest_yml("4.0.1", &setup), Some(b"garbage".to_vec()), setup.clone());
        assert!(check(&Source::at(&server.url("/feed/latest.yml")), &version("4.0.0"), None, &options()).unwrap().is_some());
        assert_eq!(paths(&server), ["/feed/latest.yml"]);
    }

    #[test]
    fn offers_nothing_to_a_current_version() {
        let setup = installer_bytes();
        let server = serve(latest_yml("4.0.1", &setup), None, setup.clone());
        let source = Source::at(&server.url("/feed/latest.yml"));
        for current in ["4.0.1", "4.1.0", "4.0.1+local"] {
            assert_eq!(check(&source, &version(current), None, &options()), Ok(None));
        }
        let server = serve(latest_yml("4.1.0-beta.1", &setup), None, setup);
        assert_eq!(check(&Source::at(&server.url("/feed/latest.yml")), &version("4.0.0"), None, &options()), Ok(None));
    }

    #[test]
    fn reports_failures() {
        let setup = installer_bytes();
        let mut wrong = setup.clone();
        wrong[0] ^= 1;
        let server = serve(latest_yml("4.0.1", &setup), None, wrong);
        let source = Source::at(&server.url("/feed/latest.yml"));
        let installer = check(&source, &version("4.0.0"), None, &options()).unwrap().unwrap();
        let cache = tempfile::tempdir().unwrap();
        assert_eq!(download(&installer, cache.path(), &options(), &|_, _| {}), Err(UpdateError::Download(DownloadError::Mismatch)));
        assert!(!cache.path().join(SETUP).exists());
        assert_eq!(pending(cache.path(), &version("4.0.0"), &AtomicBool::new(false)), None);
        let server = serve(format!("{LATEST_3_9_1}packages:\n  x64: {{}}\n"), None, Vec::new());
        assert_eq!(check(&Source::at(&server.url("/feed/latest.yml")), &version("3.0.0"), None, &options()), Err(UpdateError::WebInstaller));
        assert_eq!(check(&Source::at(&server.url("/missing/latest.yml")), &version("3.0.0"), None, &options()), Err(UpdateError::Download(DownloadError::Status(404))));
        // Plain HTTP only to this computer and only for a named feed.
        let github_like = Source { feed: server.url("/feed/latest.yml"), ..Source::github() };
        assert!(matches!(check(&github_like, &version("3.0.0"), None, &github_like.options(Arc::new(AtomicBool::new(false)))), Err(UpdateError::Download(DownloadError::Insecure(_)))));
    }
}
