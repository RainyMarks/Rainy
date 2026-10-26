//! Files shown in a page instead of, or next to, the text editor (`viewer-choice.ts`, `FileViewer.tsx`): PDF, Office
//! documents, media, shortcuts and Markdown, in WebView2 panes over the Host's own viewer page (`/?view=…`).

use gpui_kit::*;
use serde_json::{Value, json};

use crate::store::Store;
use crate::store::ide::split_key;
use crate::web::{Options, Shortcut, WebPane};

/// The viewer family of a file name (`idePreviewKind`).
pub fn preview_kind(path: &str) -> Option<&'static str> {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    let extension = name.rfind('.').filter(|dot| *dot > 0).map(|dot| name[dot + 1..].to_ascii_lowercase()).unwrap_or_default();
    let ext = extension.as_str();
    const WORD: [&str; 4] = ["docx", "docm", "dotx", "dotm"];
    const SHEET: [&str; 8] = ["xlsx", "xlsm", "xltx", "xltm", "xlsb", "xls", "xlt", "ods"];
    const OFFICE: [&str; 14] = ["pptx", "pptm", "ppsx", "ppsm", "potx", "ppt", "pps", "pot", "odp", "dps", "doc", "dot", "wps", "odt"];
    if name.starts_with("~$") && (WORD.contains(&ext) || SHEET.contains(&ext) || OFFICE.contains(&ext) || ext == "rtf") {
        return Some("office-lock");
    }
    Some(match ext {
        "svg" => "svg",
        "md" | "markdown" | "mdown" | "mkd" | "mdx" => "markdown",
        "png" | "apng" | "jpg" | "jpeg" | "jfif" | "gif" | "webp" | "bmp" | "ico" | "cur" | "avif" => "image",
        "mp3" | "wav" | "ogg" | "oga" | "opus" | "flac" | "m4a" | "aac" | "weba" => "audio",
        "mp4" | "m4v" | "webm" | "ogv" | "mov" | "mkv" => "video",
        "pdf" => "pdf",
        "lnk" => "shortcut",
        _ if WORD.contains(&ext) => "word",
        _ if SHEET.contains(&ext) => "sheet",
        _ if OFFICE.contains(&ext) => "office",
        _ => return None,
    })
}

/// The page a tab shows, and whether the text editor stays beside it (`chooseViewer`); `None` is the native editor or
/// read-only notice. `reason` is why the Host returned no editable text; `view` the tab's chosen view. Every file can be
/// shown as hex; binary files without a viewer are.
pub fn page_view(path: &str, reason: Option<&str>, view: Option<&str>) -> Option<(&'static str, bool)> {
    let kind = preview_kind(path);
    if view == Some("hex") {
        return Some(("hex", false));
    }
    match reason {
        Some("media") => match kind {
            Some("svg" | "markdown") | None => Some(("hex", false)),
            Some(kind) => Some((kind, false)),
        },
        Some("binary") => Some(("hex", false)),
        None if kind == Some("markdown") => match view {
            Some("source") => None,
            Some("preview") => Some(("markdown", false)),
            Some("split") => Some(("markdown", true)),
            _ => Some(("markdown-rich", false)),
        },
        None if kind == Some("svg") => match view {
            Some("source") => None,
            Some("split") => Some(("svg", true)),
            _ => Some(("svg", false)),
        },
        _ => None,
    }
}

/// Pages that show the editor's current text, which the workbench hands them, rather than the file's bytes.
pub fn shows_text(kind: &str) -> bool {
    kind.starts_with("markdown") || kind == "svg"
}

/// The tab views Markdown offers, with their labels and what the tab stores for them.
pub const MARKDOWN_VIEWS: [(&str, Option<&str>); 4] =
    [("viewers.viewRich", None), ("viewers.viewSource", Some("source")), ("viewers.viewPreview", Some("preview")), ("viewers.viewSplit", Some("split"))];

/// The tab views of an SVG file: the picture by default, its source, or both.
pub const SVG_VIEWS: [(&str, Option<&str>); 3] = [("viewers.viewPreview", None), ("viewers.viewSource", Some("source")), ("viewers.viewSplit", Some("split"))];

use rainy_carrier::text::percent_encode as encode;

/// The address a pane opens: the Host's launch address, which sets the session cookie and lands on the viewer page.
/// `back` tells a hex page that the tab has another view to return to.
#[allow(clippy::too_many_arguments)]
pub fn viewer_url(launch: &str, viewer: &str, key: &str, workspace_id: &str, bytes: u64, version: &str, host_path: Option<&str>, back: bool) -> String {
    let (root, path) = split_key(key);
    let mut page = format!("/?view={viewer}&workspace={}&path={}&bytes={bytes}&version={}", encode(workspace_id), encode(path), encode(version));
    if let Some(root) = root {
        page.push_str(&format!("&root={}", encode(root)));
    }
    if let Some(host_path) = host_path {
        page.push_str(&format!("&hostPath={}", encode(host_path)));
    }
    if back {
        page.push_str("&back=1");
    }
    format!("{launch}&next={}", encode(&page))
}

/// Combinations the workbench keeps while a page of `kind` has the keyboard, and the action each one runs.
pub fn shortcut_action(shortcut: Shortcut, kind: &str) -> Option<Box<dyn Action>> {
    use crate::ui::ide::*;
    const VK_OEM_COMMA: u32 = 0xBC;
    const VK_OEM_3: u32 = 0xC0;
    const VK_OEM_2: u32 = 0xBF;
    const VK_F1: u32 = 0x70;
    const VK_F8: u32 = 0x77;
    const VK_F9: u32 = 0x78;
    const VK_V: u32 = 0x56;
    // Typora's keys for the rich editor: F8 focus mode, F9 typewriter mode, Ctrl+/ source.
    match (shortcut.ctrl, shortcut.shift, shortcut.alt, shortcut.key) {
        (false, false, false, VK_F1) => return Some(Box::new(CommandPalette)),
        (false, false, false, VK_F8) => return Some(Box::new(MarkdownFocus)),
        (false, false, false, VK_F9) => return Some(Box::new(MarkdownTypewriter)),
        (true, false, false, VK_OEM_2) => return Some(Box::new(MarkdownSource)),
        // The preview, as in the source editor; other pages keep Ctrl+Shift+V for pasting plain text.
        (true, true, false, VK_V) if kind.starts_with("markdown") => return Some(Box::new(MarkdownPreview)),
        _ => {}
    }
    if !shortcut.ctrl {
        return None;
    }
    let letter = char::from_u32(shortcut.key).filter(char::is_ascii_uppercase);
    Some(match (shortcut.shift, shortcut.alt, letter, shortcut.key) {
        (false, false, Some('W'), _) => Box::new(CloseFile),
        (false, false, Some('P'), _) | (false, true, Some('P'), _) => Box::new(QuickOpen),
        (true, false, Some('P'), _) => Box::new(CommandPalette),
        (true, false, Some('F'), _) => Box::new(FindInFiles),
        (true, false, Some('H'), _) => Box::new(ReplaceInFiles),
        (true, false, Some('E'), _) => Box::new(ShowExplorer),
        (true, false, Some('G'), _) => Box::new(ShowSourceControl),
        (false, false, Some('O'), _) | (false, true, Some('O'), _) => Box::new(OpenFolder),
        (false, true, Some('N'), _) => Box::new(NewChat),
        (false, false, None, VK_OEM_3) => Box::new(ToggleTerminal),
        (false, false, None, VK_OEM_COMMA) => Box::new(OpenSettings),
        _ => return None,
    })
}

/// What the viewer page asks the workbench to do.
pub enum PageRequest {
    Ready,
    Change(String),
    Save,
    OpenUrl(String),
    OpenFile { path: String, root: Option<String>, line: Option<u64> },
    Call { id: u64, name: String, params: Value },
    /// The project map opens a file by its editor key.
    OpenKey(String),
    /// The project map asks the chat something.
    Ask(String),
    /// The heading at the top of the rich editor, in outline order; -1 above the first.
    Outline(i64),
    /// Something to tell the user, such as where an export went.
    Notice { error: bool, text: String },
    /// An image for the AI composer, as the page encoded it.
    Attach { name: String, mime: &'static str, bytes: Vec<u8> },
    /// Show the tab in another view (`hex`), or in its default one.
    View(Option<String>),
    /// The file changed on disk while the page read it; read it again.
    Reload,
}

/// The image types the composer accepts from a page.
fn attach_mime(mime: &str) -> Option<&'static str> {
    Some(match mime {
        "image/png" => "image/png",
        "image/jpeg" => "image/jpeg",
        "image/webp" => "image/webp",
        "image/gif" => "image/gif",
        _ => return None,
    })
}

pub fn parse_message(text: &str) -> Option<PageRequest> {
    let value: Value = serde_json::from_str(text).ok()?;
    Some(match value["type"].as_str()? {
        "ready" => PageRequest::Ready,
        "change" => PageRequest::Change(value["text"].as_str()?.to_owned()),
        "save" => PageRequest::Save,
        "open-url" => PageRequest::OpenUrl(value["url"].as_str()?.to_owned()),
        "open-file" => PageRequest::OpenFile {
            path: value["path"].as_str()?.to_owned(),
            root: value["rootId"].as_str().map(str::to_owned),
            line: value["line"].as_u64(),
        },
        "request" => PageRequest::Call { id: value["id"].as_u64()?, name: value["name"].as_str()?.to_owned(), params: value["params"].clone() },
        "open-key" => PageRequest::OpenKey(value["key"].as_str()?.to_owned()),
        "ask" => PageRequest::Ask(value["text"].as_str()?.to_owned()),
        "outline" => PageRequest::Outline(value["index"].as_i64()?),
        "notice" => PageRequest::Notice { error: value["tone"] == "error", text: value["text"].as_str()?.to_owned() },
        "attach" => {
            use base64::Engine as _;
            let bytes = base64::engine::general_purpose::STANDARD.decode(value["base64"].as_str()?).ok()?;
            PageRequest::Attach { name: value["name"].as_str()?.to_owned(), mime: attach_mime(value["mime"].as_str()?)?, bytes }
        }
        "view" => match &value["view"] {
            Value::Null => PageRequest::View(None),
            Value::String(view) if view == "hex" => PageRequest::View(Some(view.clone())),
            _ => return None,
        },
        "reload" => PageRequest::Reload,
        _ => return None,
    })
}

/// The script that hands the page the file's current text.
pub fn set_text_script(text: &str, version: &str) -> String {
    format!("window.__rainyViewer && window.__rainyViewer.setText({}, {})", json!(text), json!(version))
}

/// The address of the project map page.
pub fn map_url(launch: &str, workspace_id: &str) -> String {
    format!("{launch}&next={}", encode(&format!("/?view=map&workspace={}", encode(workspace_id))))
}

/// The script that tells the map page which chat's changes to mark.
pub fn set_session_script(session: Option<&str>) -> String {
    format!("window.__rainyViewer && window.__rainyViewer.setSession({})", json!(session))
}

/// The script that scrolls the rich editor to a heading.
pub fn reveal_heading_script(index: usize) -> String {
    format!("window.__rainyViewer && window.__rainyViewer.revealHeading({index})")
}

/// The script that exports the document as `pdf` or `html`.
pub fn export_script(format: &str) -> String {
    format!("window.__rainyViewer && window.__rainyViewer.exportDocument({})", json!(format))
}

/// The script that answers one of the page's requests.
pub fn resolve_script(id: u64, result: &Value) -> String {
    format!("window.__rainyViewer && window.__rainyViewer.resolve({id}, {result})")
}

/// A pane for one file view.
pub fn create_pane(url: String, kind: &'static str, window: &mut Window, cx: &mut App) -> Entity<WebPane> {
    let store = Store::global(cx);
    let folder = store.read(cx).paths.user_data.join("WebView2");
    let background = crate::web::background(gpui_kit::component::ActiveTheme::theme(cx).background);
    cx.new(|cx| {
        let options = Options {
            folder,
            private: false,
            background,
            script: None,
            on_message: None,
            // The viewer page and the files it loads come from the Host; anything else opens in the browser instead.
            allow: Some(Box::new(|url| url.starts_with("http://127.0.0.1:") || url.starts_with("about:") || url.starts_with("blob:") || url.starts_with("data:"))),
            on_new_window: None,
            on_shortcut: Some(Box::new(move |shortcut| shortcut_action(shortcut, kind).is_some())),
        };
        WebPane::new(url, options, window, cx)
    })
}

#[cfg(test)]
mod tests {
    use ::core::prelude::v1::test;

    use super::*;

    #[test]
    fn markdown_pages_keep_the_preview_key() {
        let paste_plain = Shortcut { key: 0x56, ctrl: true, shift: true, alt: false };
        let name = |kind: &str| shortcut_action(paste_plain, kind).map(|action| action.name());
        assert_eq!(name("markdown-rich"), Some("rainy::MarkdownPreview"));
        assert_eq!(name("markdown"), Some("rainy::MarkdownPreview"));
        assert_eq!(name("pdf"), None);
        assert_eq!(name("map"), None);
        // Ctrl+V itself stays with every page.
        assert!(shortcut_action(Shortcut { shift: false, ..paste_plain }, "markdown-rich").is_none());
    }

    #[test]
    fn views_of_files() {
        assert_eq!(page_view("a.png", Some("media"), None), Some(("image", false)));
        assert_eq!(page_view("a.avif", Some("media"), None), Some(("image", false)));
        assert_eq!(page_view("a.svg", None, None), Some(("svg", false)));
        assert_eq!(page_view("a.svg", None, Some("source")), None);
        assert_eq!(page_view("a.svg", None, Some("split")), Some(("svg", true)));
        assert_eq!(page_view("a.bin", Some("binary"), None), Some(("hex", false)));
        assert_eq!(page_view("a.mp4", Some("media"), Some("hex")), Some(("hex", false)));
        assert_eq!(page_view("a.log", Some("too-large"), None), None);
        assert_eq!(page_view("a.md", None, Some("rich")), Some(("markdown-rich", false)));
        assert_eq!(page_view("a.md", None, Some("hex")), Some(("hex", false)));
        assert!(shows_text("svg") && shows_text("markdown-rich") && !shows_text("hex"));
    }

    #[test]
    fn page_messages() {
        assert!(matches!(parse_message(r#"{"type":"view","view":"hex"}"#), Some(PageRequest::View(Some(view))) if view == "hex"));
        assert!(matches!(parse_message(r#"{"type":"view","view":null}"#), Some(PageRequest::View(None))));
        assert!(parse_message(r#"{"type":"view","view":"source"}"#).is_none());
        assert!(matches!(parse_message(r#"{"type":"reload"}"#), Some(PageRequest::Reload)));
        let attach = parse_message(r#"{"type":"attach","name":"a.png","mime":"image/png","base64":"AAEC"}"#);
        assert!(matches!(attach, Some(PageRequest::Attach { mime: "image/png", bytes, .. }) if bytes == [0, 1, 2]));
        assert!(parse_message(r#"{"type":"attach","name":"a.svg","mime":"image/svg+xml","base64":"AAEC"}"#).is_none());
        assert!(viewer_url("http://127.0.0.1:1/launch?t=x", "hex", "a.bin", "w", 3, "v1", None, true).contains(&encode("&back=1")));
    }
}
