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
/// image view. `reason` is why the Host returned no editable text; `view` the tab's chosen view.
pub fn page_view(path: &str, reason: Option<&str>, view: Option<&str>) -> Option<(&'static str, bool)> {
    let kind = preview_kind(path);
    match reason {
        Some("media") => match kind {
            Some("image" | "svg" | "markdown") | None => None,
            Some(kind) => Some((kind, false)),
        },
        None if kind == Some("markdown") => match view {
            Some("source" | "hex") => None,
            Some("preview") => Some(("markdown", false)),
            Some("split") => Some(("markdown", true)),
            _ => Some(("markdown-rich", false)),
        },
        _ => None,
    }
}

/// The tab views Markdown offers, with their labels and what the tab stores for them.
pub const MARKDOWN_VIEWS: [(&str, Option<&str>); 4] =
    [("viewers.viewRich", None), ("viewers.viewSource", Some("source")), ("viewers.viewPreview", Some("preview")), ("viewers.viewSplit", Some("split"))];

fn encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// The address a pane opens: the Host's launch address, which sets the session cookie and lands on the viewer page.
pub fn viewer_url(launch: &str, viewer: &str, key: &str, workspace_id: &str, bytes: u64, version: &str, host_path: Option<&str>) -> String {
    let (root, path) = split_key(key);
    let mut page = format!("/?view={viewer}&workspace={}&path={}&bytes={bytes}&version={}", encode(workspace_id), encode(path), encode(version));
    if let Some(root) = root {
        page.push_str(&format!("&root={}", encode(root)));
    }
    if let Some(host_path) = host_path {
        page.push_str(&format!("&hostPath={}", encode(host_path)));
    }
    format!("{launch}&next={}", encode(&page))
}

/// Combinations the workbench keeps while a page has the keyboard, and the action each one runs.
pub fn shortcut_action(shortcut: Shortcut) -> Option<Box<dyn Action>> {
    use crate::ui::ide::*;
    const VK_OEM_COMMA: u32 = 0xBC;
    const VK_OEM_3: u32 = 0xC0;
    const VK_OEM_2: u32 = 0xBF;
    const VK_F1: u32 = 0x70;
    const VK_F8: u32 = 0x77;
    const VK_F9: u32 = 0x78;
    // Typora's keys for the rich editor: F8 focus mode, F9 typewriter mode, Ctrl+/ source.
    match (shortcut.ctrl, shortcut.shift, shortcut.alt, shortcut.key) {
        (false, false, false, VK_F1) => return Some(Box::new(CommandPalette)),
        (false, false, false, VK_F8) => return Some(Box::new(MarkdownFocus)),
        (false, false, false, VK_F9) => return Some(Box::new(MarkdownTypewriter)),
        (true, false, false, VK_OEM_2) => return Some(Box::new(MarkdownSource)),
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
pub fn create_pane(url: String, window: &mut Window, cx: &mut App) -> Entity<WebPane> {
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
            on_shortcut: Some(Box::new(|shortcut| shortcut_action(shortcut).is_some())),
        };
        WebPane::new(url, options, window, cx)
    })
}
