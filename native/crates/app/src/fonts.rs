//! Fonts for the interface and for code beyond Windows' own (Settings → Appearance). Popular open-source fonts are
//! downloaded when chosen, from Rainy's `fonts-v1` release with their size and SHA-256 checked, kept in
//! `<user data>\fonts` and loaded only while a preference names them, so the installer carries none of them.

use std::borrow::Cow;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use gpui_kit::*;
use rainy_carrier::download::{self, Digest, Expected, Options};
use rainy_protocol::prefs::UiPreferences;
use serde_json::json;

const RELEASE: &str = "https://github.com/RainyMarks/Rainy/releases/download/fonts-v1/";

/// Which preference a font is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Use {
    Ui,
    Code,
}

impl Use {
    pub fn pref(self) -> &'static str {
        match self {
            Use::Ui => "uiFont",
            Use::Code => "codeFont",
        }
    }
}

/// A font file Rainy downloads.
pub struct FontFile {
    pub name: &'static str,
    pub bytes: u64,
    pub sha256: &'static str,
}

/// A font the settings offer: its family (empty for Rainy's default), its names, and the file to download unless
/// Windows has it.
pub struct Choice {
    pub family: &'static str,
    pub zh: &'static str,
    pub en: &'static str,
    pub used: Use,
    pub file: Option<FontFile>,
}

const fn system(family: &'static str, zh: &'static str, en: &'static str, used: Use) -> Choice {
    Choice { family, zh, en, used, file: None }
}

const fn download(family: &'static str, zh: &'static str, en: &'static str, used: Use, name: &'static str, bytes: u64, sha256: &'static str) -> Choice {
    Choice { family, zh, en, used, file: Some(FontFile { name, bytes, sha256 }) }
}

/// The fonts offered, in menu order: Windows' own first. All downloaded ones are under the SIL Open Font License.
pub const CHOICES: [Choice; 14] = [
    system("", "默认（Segoe UI，中文用微软雅黑）", "Default (Segoe UI)", Use::Ui),
    system("Microsoft YaHei UI", "微软雅黑", "Microsoft YaHei", Use::Ui),
    system("DengXian", "等线", "DengXian", Use::Ui),
    download("Inter", "Inter", "Inter", Use::Ui, "Inter.ttf", 876_576, "29160a80ff49ddcab2c97711247e08b1fab27a484a329ce8b813d820dc559031"),
    download("Geist", "Geist", "Geist", Use::Ui, "Geist.ttf", 169_056, "73894e0448cae90a92b6c2f8732b7bb9acb7b94c418bff559dad4a18e1de9659"),
    download("Noto Sans SC", "思源黑体", "Noto Sans SC", Use::Ui, "NotoSansSC.ttf", 17_772_300, "a3041811a78c361b1de50f953c805e0244951c21c5bd412f7232ef0d899af0da"),
    download("LXGW WenKai", "霞鹜文楷", "LXGW WenKai", Use::Ui, "LXGWWenKai.ttf", 25_928_940, "2d73eea94ad808999d90c0000c239a84a33b09d4bc6ab6c5b8bbf1eda8a7fc8b"),
    system("", "默认（Cascadia Mono）", "Default (Cascadia Mono)", Use::Code),
    system("Consolas", "Consolas", "Consolas", Use::Code),
    system("Cascadia Code", "Cascadia Code（连字）", "Cascadia Code (ligatures)", Use::Code),
    download("JetBrains Mono", "JetBrains Mono", "JetBrains Mono", Use::Code, "JetBrainsMono.ttf", 187_208, "48715a42ec242c21e9f02692891e147d022299a52e48d5e413e1a942193ffeda"),
    download("Fira Code", "Fira Code", "Fira Code", Use::Code, "FiraCode.ttf", 260_364, "9335b082b3c7850d98a64b584f3417f65355f3471278bb5eeb8c6c0e8657aeeb"),
    download("Geist Mono", "Geist Mono", "Geist Mono", Use::Code, "GeistMono.ttf", 171_948, "d00e590b8eb3a59acc329b2d044fd143ae935090b7da33199ebee27cc7de8196"),
    download("LXGW WenKai Mono", "霞鹜文楷等宽", "LXGW WenKai Mono", Use::Code, "LXGWWenKaiMono.ttf", 25_957_300, "895ce1d2fa00d5ca3bec0329d38e3190454f4b5fa7b4733f491c419faf9753a1"),
];

/// The offered font of a family used one way.
pub fn choice(family: &str, used: Use) -> Option<&'static Choice> {
    CHOICES.iter().find(|choice| choice.family == family && choice.used == used)
}

/// The first family of a comma-separated font preference.
pub fn first_family(preference: &str) -> &str {
    preference.split(',').map(|name| name.trim().trim_matches(['"', '\''])).find(|name| !name.is_empty()).unwrap_or("")
}

/// A size as the menu shows it: "0.9 MB".
pub fn megabytes(bytes: u64) -> String {
    format!("{:.1} MB", (bytes as f64 / 1_000_000.0).max(0.1))
}

/// A font being downloaded.
pub struct Download {
    pub family: &'static str,
    pub used: Use,
    pub done: Arc<AtomicU64>,
    pub total: u64,
}

impl Download {
    pub fn percent(&self) -> u64 {
        (self.done.load(Ordering::Relaxed) * 100).checked_div(self.total).unwrap_or(0).min(100)
    }
}

/// Downloaded fonts and the one downloading.
pub struct FontLibrary {
    dir: PathBuf,
    /// Families Windows has, read once.
    system: HashSet<String>,
    loaded: HashSet<&'static str>,
    pub downloading: Option<Download>,
    /// The last download that failed: what it was for and why.
    pub failed: Option<(Use, String)>,
}

impl Global for FontLibrary {}

/// Read which fonts Windows has; downloaded ones are kept under `user_data`.
pub fn init(user_data: &Path, cx: &mut App) {
    let system = cx.text_system().all_font_names().into_iter().collect();
    cx.set_global(FontLibrary { dir: user_data.join("fonts"), system, loaded: HashSet::new(), downloading: None, failed: None });
}

impl FontLibrary {
    fn path(&self, file: &FontFile) -> PathBuf {
        self.dir.join(file.name)
    }

    /// Whether a font can be used now: it is the default, Windows has it, or its file was downloaded.
    pub fn available(&self, choice: &Choice) -> bool {
        match &choice.file {
            None => choice.family.is_empty() || self.system.contains(choice.family),
            Some(file) => std::fs::metadata(self.path(file)).is_ok_and(|metadata| metadata.len() == file.bytes),
        }
    }
}

/// Load the downloaded fonts the preferences name, once each, before the theme uses them.
pub fn load_chosen(prefs: &UiPreferences, cx: &mut App) {
    let Some(library) = cx.try_global::<FontLibrary>() else { return };
    let wanted: Vec<(&'static str, PathBuf)> = [(prefs.ui_font.as_str(), Use::Ui), (prefs.code_font.as_str(), Use::Code)]
        .into_iter()
        .filter_map(|(preference, used)| {
            let choice = choice(first_family(preference), used)?;
            let file = choice.file.as_ref()?;
            (!library.loaded.contains(choice.family)).then(|| (choice.family, library.path(file)))
        })
        .collect();
    for (family, path) in wanted {
        match std::fs::read(&path) {
            Ok(bytes) => match cx.text_system().add_fonts(vec![Cow::Owned(bytes)]) {
                Ok(()) => {
                    cx.global_mut::<FontLibrary>().loaded.insert(family);
                }
                Err(error) => log::warn!("The font {family} was not loaded: {error:#}"),
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => log::warn!("The font {} was not read: {error}", path.display()),
        }
    }
}

/// Download a font's file, then make it the preference it is for. One download runs at a time.
pub fn download_font(choice: &'static Choice, window: &mut Window, cx: &mut App) {
    let Some(file) = &choice.file else { return };
    let Some(digest) = Digest::sha256_hex(file.sha256) else { return };
    let library = cx.global_mut::<FontLibrary>();
    if library.downloading.is_some() {
        return;
    }
    let done = Arc::new(AtomicU64::new(0));
    library.downloading = Some(Download { family: choice.family, used: choice.used, done: done.clone(), total: file.bytes });
    library.failed = None;
    let target = library.path(file);
    let url = format!("{RELEASE}{}", file.name);
    let expected = Expected { bytes: file.bytes, digest };
    let handle = window.window_handle();
    // Redraw while it runs, so the settings show its progress.
    cx.spawn(async move |cx: &mut AsyncApp| {
        loop {
            cx.background_executor().timer(Duration::from_millis(200)).await;
            let running = cx.update(|cx| {
                cx.refresh_windows();
                cx.global::<FontLibrary>().downloading.is_some()
            });
            if !running {
                break;
            }
        }
    })
    .detach();
    cx.spawn(async move |cx: &mut AsyncApp| {
        let result = cx
            .background_spawn(async move {
                if let Some(folder) = target.parent() {
                    std::fs::create_dir_all(folder).map_err(|error| error.to_string())?;
                }
                let options = Options::new(Arc::new(AtomicBool::new(false)));
                let progress = |present, _| done.store(present, Ordering::Relaxed);
                // A connection that drops once is tried again, from where it stopped.
                download::download(&url, &target, &expected, &options, &progress)
                    .or_else(|_| {
                        std::thread::sleep(Duration::from_secs(1));
                        download::download(&url, &target, &expected, &options, &progress)
                    })
                    .map_err(|error| error.to_string())
            })
            .await;
        cx.update(|cx| {
            let library = cx.global_mut::<FontLibrary>();
            library.downloading = None;
            match result {
                Ok(()) => {
                    let _ = handle.update(cx, |_, window, cx| crate::ui::settings::set_prefs(json!({ choice.used.pref(): choice.family }), window, cx));
                }
                Err(error) => {
                    log::warn!("The font {} was not downloaded: {error}", choice.family);
                    library.failed = Some((choice.used, error));
                }
            }
            cx.refresh_windows();
        });
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::*;
    // `gpui_kit::*` brings GPUI's own `test` macro; use Rust's.
    use ::core::prelude::v1::test;

    #[test]
    fn offers_each_font_once_with_a_valid_digest() {
        let mut seen = HashSet::new();
        for choice in &CHOICES {
            assert!(seen.insert((choice.family, choice.used)), "{}", choice.family);
            if let Some(file) = &choice.file {
                assert!(Digest::sha256_hex(file.sha256).is_some(), "{}", file.name);
                assert!(file.bytes > 0 && file.name.ends_with(".ttf"));
            }
        }
        assert!(choice("", Use::Ui).is_some() && choice("", Use::Code).is_some());
    }

    #[test]
    fn reads_the_first_family() {
        assert_eq!(first_family(""), "");
        assert_eq!(first_family("'LXGW WenKai', serif"), "LXGW WenKai");
        assert_eq!(megabytes(876_576), "0.9 MB");
        assert_eq!(megabytes(25_928_940), "25.9 MB");
    }
}
