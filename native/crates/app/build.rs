//! What Rainy.exe carries besides its code: its icon and version resource, `WebView2Loader.dll` next to it, and the
//! public keys its release files are signed with (`release_keys.rs`, read by `src/release.rs`).

use std::env;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

#[allow(dead_code)]
#[path = "../carrier/src/keyring.rs"]
mod keyring;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR is set for build scripts"));
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is set for build scripts"));
    copy_webview_loader(&out);
    embed_resources(&out, &manifest);
    embed_keyring(&out, &manifest);
}

/// Puts `WebView2Loader.dll`, which Rainy.exe loads at start for its WebView2 panes, next to the built executable.
fn copy_webview_loader(out: &Path) {
    // OUT_DIR is target/<profile>/build/<crate>-<hash>/out.
    let Some(profile) = out.ancestors().nth(3) else { return };
    let home = env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("USERPROFILE").map(|home| PathBuf::from(home).join(".cargo")));
    let Some(registry) = home.map(|home| home.join("registry").join("src")) else { return };
    let Ok(indexes) = std::fs::read_dir(registry) else { return };
    let mut found: Vec<PathBuf> = indexes
        .flatten()
        .flat_map(|index| std::fs::read_dir(index.path()).into_iter().flatten().flatten())
        .filter(|entry| entry.file_name().to_string_lossy().starts_with("webview2-com-sys-0.39"))
        .map(|entry| entry.path().join("x64").join("WebView2Loader.dll"))
        .filter(|path| path.is_file())
        .collect();
    found.sort();
    if let Some(loader) = found.last() {
        let _ = std::fs::copy(loader, profile.join("WebView2Loader.dll"));
    } else {
        println!("cargo:warning=WebView2Loader.dll was not found; copy it next to Rainy.exe before running.");
    }
}

/// Icon 1, which GPUI loads for the window and Explorer shows for the file, and the version block of the file's
/// properties. The application manifest comes from gpui-pre.
fn embed_resources(out: &Path, manifest: &Path) {
    let brand = manifest.join("assets").join("brand");
    println!("cargo:rerun-if-changed={}", brand.join("rainy.ico").display());
    let version = env::var("CARGO_PKG_VERSION").expect("CARGO_PKG_VERSION is set for build scripts");
    let [major, minor, patch] = ["MAJOR", "MINOR", "PATCH"].map(|part| env::var(format!("CARGO_PKG_VERSION_{part}")).unwrap_or_default());
    // VS_FF_DEBUG for anything but a release build, VS_FF_PRERELEASE for a version like 4.1.0-beta.1.
    let debug = u8::from(env::var("PROFILE").as_deref() != Ok("release"));
    let prerelease = u8::from(env::var("CARGO_PKG_VERSION_PRE").is_ok_and(|pre| !pre.is_empty()));
    let flags = debug | prerelease << 1;
    let script = format!(
        r#"1 ICON "rainy.ico"

1 VERSIONINFO
FILEVERSION {major},{minor},{patch},0
PRODUCTVERSION {major},{minor},{patch},0
FILEFLAGSMASK 0x3F
FILEFLAGS {flags:#x}
FILEOS 0x40004
FILETYPE 0x1
FILESUBTYPE 0x0
BEGIN
    BLOCK "StringFileInfo"
    BEGIN
        BLOCK "040904B0"
        BEGIN
            VALUE "CompanyName", "RainyMarks"
            VALUE "FileDescription", "Rainy"
            VALUE "FileVersion", "{version}"
            VALUE "InternalName", "Rainy"
            VALUE "LegalCopyright", "Copyright (c) 2026 RainyMarks"
            VALUE "OriginalFilename", "RainyAgent.exe"
            VALUE "ProductName", "Rainy"
            VALUE "ProductVersion", "{version}"
        END
    END
    BLOCK "VarFileInfo"
    BEGIN
        VALUE "Translation", 0x409, 1200
    END
END
"#
    );
    let script_path = out.join("rainy.rc");
    std::fs::write(&script_path, script).expect("writing rainy.rc");
    embed_resource::compile(&script_path, embed_resource::ParamsIncludeDirs([&brand]))
        .manifest_required()
        .expect("compiling rainy.rc with windres (MSYS2 UCRT64 must be on PATH)");
}

/// `release_keys.rs`: the keyring at `RAINY_RELEASE_PUBLIC_KEYS` or `<repo>/runtime/release-public-keys.json`, checked
/// like 3.x checks it. A release build fails without one unless `RAINY_ALLOW_UNSIGNED=1`, which builds an executable
/// without keys, so it checks no resources.
fn embed_keyring(out: &Path, manifest: &Path) {
    println!("cargo:rerun-if-env-changed=RAINY_RELEASE_PUBLIC_KEYS");
    println!("cargo:rerun-if-env-changed=RAINY_ALLOW_UNSIGNED");
    let named = env::var_os("RAINY_RELEASE_PUBLIC_KEYS").map(PathBuf::from);
    let path = named.clone().unwrap_or_else(|| {
        let repo = manifest.ancestors().nth(3).expect("native/crates/app lies in the repository");
        repo.join("runtime").join("release-public-keys.json")
    });
    let release = env::var("PROFILE").as_deref() == Ok("release");
    let keyring = if env::var("RAINY_ALLOW_UNSIGNED").as_deref() == Ok("1") {
        if release {
            println!("cargo:warning=RAINY_ALLOW_UNSIGNED=1: this Rainy.exe has no release keys and does not check its resources.");
        }
        None
    } else if path.is_file() {
        // A missing file is not watched, since Cargo would rerun this script on every build; a release build without
        // keys fails, and a failed script always runs again.
        println!("cargo:rerun-if-changed={}", path.display());
        let text = std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("Reading {}: {error}", path.display()));
        Some(keyring::Keyring::parse(&text).unwrap_or_else(|error| panic!("{}: {error:#}", path.display())))
    } else if named.is_some() {
        panic!("RAINY_RELEASE_PUBLIC_KEYS names {}, which is not a file.", path.display());
    } else if release {
        panic!(
            "A release build needs the public release keys at {}. Set RAINY_RELEASE_PUBLIC_KEYS to the keyring, or \
             RAINY_ALLOW_UNSIGNED=1 for a local build that does not check its resources.",
            path.display()
        );
    } else {
        None
    };
    let mut code = String::from("pub const RELEASE_KEYS: Option<&[(&str, [u8; 32])]> = ");
    match keyring {
        None => code.push_str("None;\n"),
        Some(keyring) => {
            code.push_str("Some(&[\n");
            for (id, key) in keyring.keys() {
                writeln!(code, "    ({id:?}, {key:?}),").expect("writing to a String");
            }
            code.push_str("]);\n");
        }
    }
    std::fs::write(out.join("release_keys.rs"), code).expect("writing release_keys.rs");
}
