//! Exported Markdown documents (`src/main/document-export.ts`): the page builds a self-contained HTML file, and this prints
//! it to an A4 PDF in a hidden WebView2 that opens nothing else, with its headings as bookmarks and text that copies and
//! searches as written.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use base64::Engine as _;
use serde_json::Value;
use windows::Win32::Foundation::HWND;

use super::webview::{Options, WebView};

type PdfDone = Box<dyn FnOnce(Result<Vec<u8>, PdfFailure>) + Send>;

static EXPORT_SEQ: AtomicU64 = AtomicU64::new(1);

/// Maximum time allowed for hidden WebView2 PDF export before aborting and cleaning up.
pub const EXPORT_TIMEOUT: Duration = Duration::from_secs(60);

/// Handle returned by [`print_to_pdf`] so the caller can abort a stalled export on the UI thread.
pub struct ExportHandle {
    cancel: Option<Rc<dyn Fn()>>,
}

impl ExportHandle {
    /// Abort an in-flight export if it has not yet completed; must be called on the UI thread.
    pub fn abort(&self) {
        if let Some(cancel) = &self.cancel {
            cancel();
        }
    }
}

/// Build a canonical percent-encoded `file:///` URL for a local Windows file path.
fn file_url(path: &Path) -> String {
    let raw = path.to_string_lossy().replace('\\', "/");
    let raw = raw.strip_prefix("//?/").unwrap_or(&raw);
    let mut encoded = String::from("file:///");
    for byte in raw.as_bytes() {
        let ch = *byte as char;
        if ch.is_ascii_alphanumeric() || matches!(ch, '/' | '-' | '_' | '.' | '~' | ':') {
            encoded.push(ch);
        } else {
            encoded.push_str(&format!("%{:02X}", byte));
        }
    }
    encoded
}

/// Whether `address` is `about:blank` or the exact exported `document.html` file URL.
fn is_allowed_export_url(address: &str, allowed: &str) -> bool {
    if address == "about:blank" {
        return true;
    }
    address.eq_ignore_ascii_case(allowed)
}

/// A file name the page suggested, without characters Windows does not allow; `document` when nothing is left.
pub fn file_name(name: &str) -> String {
    let cleaned: String = name.chars().map(|c| if (c as u32) < 32 || "<>:\"/\\|?*".contains(c) { '-' } else { c }).collect();
    let cleaned: String = cleaned.trim_end_matches(['.', ' ']).trim().chars().take(120).collect();
    if cleaned.is_empty() { "document".to_owned() } else { cleaned }
}

thread_local! {
    /// Export controls done with, closed by `close_finished` from outside WebView2's callbacks.
    static CLOSING: RefCell<Vec<WebView>> = RefCell::default();
}

/// Close the controls of finished exports; call it outside WebView2's callbacks.
pub fn close_finished() {
    for view in CLOSING.with(|closing| std::mem::take(&mut *closing.borrow_mut())) {
        view.close();
    }
}

/// Why no PDF was made. The caller words it for the user; the details are the system's or WebView2's own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PdfFailure {
    /// The page to print could not be written to the temporary folder.
    Page(String),
    /// The hidden WebView2 did not start.
    Control(String),
    /// WebView2 did not print the page.
    Print(String),
    /// WebView2 printed, but its answer held no PDF.
    Empty,
}

/// The bytes of the PDF in WebView2's answer to `Page.printToPDF`.
fn pdf_bytes(answer: &str) -> Result<Vec<u8>, PdfFailure> {
    let value: Value = serde_json::from_str(answer).map_err(|_| PdfFailure::Empty)?;
    let data = value["data"].as_str().ok_or(PdfFailure::Empty)?;
    base64::engine::general_purpose::STANDARD.decode(data).map_err(|_| PdfFailure::Empty)
}

/// Print `html` to a PDF; `done` runs once the PDF is built (with ToUnicode radical post-processing off the UI thread).
pub fn print_to_pdf(parent: HWND, profile: PathBuf, html: String, done: impl FnOnce(Result<Vec<u8>, PdfFailure>) + Send + 'static) -> ExportHandle {
    let seq = EXPORT_SEQ.fetch_add(1, Ordering::Relaxed);
    let folder = std::env::temp_dir().join(format!("rainy-export-{}-{}-{}", std::process::id(), chrono::Utc::now().timestamp_millis(), seq));
    let page = folder.join("document.html");
    if let Err(error) = std::fs::create_dir_all(&folder).and_then(|()| std::fs::write(&page, html)) {
        done(Err(PdfFailure::Page(error.to_string())));
        return ExportHandle { cancel: None };
    }
    let url = file_url(&page);
    let allowed = url.clone();
    let expected_source = url.clone();
    let done: Rc<RefCell<Option<PdfDone>>> = Rc::new(RefCell::new(Some(Box::new(done))));
    // The control lives until the PDF is made; then it and the page go.
    let holder: Rc<RefCell<Option<WebView>>> = Rc::default();
    let finish = {
        let (done, holder, folder) = (done.clone(), holder.clone(), folder.clone());
        Rc::new(move |result: Result<Vec<u8>, PdfFailure>| {
            let _ = std::fs::remove_dir_all(&folder);
            // Closed once WebView2 is out of the callback that got here.
            if let Some(view) = holder.borrow_mut().take() {
                CLOSING.with(|closing| closing.borrow_mut().push(view));
            }
            let Some(done) = done.borrow_mut().take() else { return };
            match result {
                Ok(raw_pdf) => {
                    // Run zlib decompression + ToUnicode CMap rewrite off the UI thread so large PDFs never freeze GPUI.
                    let spawned = std::thread::Builder::new().name("pdf-text".into()).spawn(move || {
                        let searchable = super::pdf_text::searchable_pdf(raw_pdf);
                        log::info!("Markdown export printed: {} bytes", searchable.len());
                        done(Ok(searchable));
                    });
                    if let Err(error) = spawned {
                        log::warn!("Failed to spawn pdf-text worker: {error}");
                    }
                }
                Err(failure) => {
                    log::info!("Markdown export printed: {failure:?}");
                    done(Err(failure));
                }
            }
        })
    };
    let options = Options {
        folder: profile,
        private: true,
        background: (255, 255, 255),
        scripts_enabled: false,
        ipc_origin: None,
        script: None,
        on_message: None,
        allow: Some(Box::new(move |address| is_allowed_export_url(address, &allowed))),
        allow_resource: None,
        on_new_window: Some(Box::new(|_| {})),
        on_shortcut: None,
    };
    let failed = finish.clone();
    let abort_finish = finish.clone();
    let cancelled = Rc::new(RefCell::new(false));
    let cancelled_create = cancelled.clone();
    WebView::create(parent, options, move |created| {
        let view = match created {
            Ok(view) => view,
            Err(error) => {
                failed(Err(PdfFailure::Control(error)));
                return;
            }
        };
        if *cancelled_create.borrow() {
            CLOSING.with(|closing| closing.borrow_mut().push(view));
            return;
        }
        view.set_visible(false);
        view.set_bounds(0, 0, 1000, 1400);
        *holder.borrow_mut() = Some(view.clone());
        let printing = Rc::new(RefCell::new(false));
        let (printer, finish) = (view.clone(), finish.clone());
        // Scripts are disabled for untrusted exported HTML, so wait for WebView2's own
        // NavigationCompleted event (after subresources and CSS stylesheets load) instead of
        // evaluating JS via `Runtime.evaluate`.
        view.on_navigation_completed(move |source, success| {
            if source == "about:blank" || (!source.is_empty() && !source.eq_ignore_ascii_case(&expected_source)) {
                return;
            }
            if std::mem::replace(&mut *printing.borrow_mut(), true) {
                return;
            }
            if !success {
                finish(Err(PdfFailure::Print("navigation failed".to_owned())));
                return;
            }
            let finish = finish.clone();
            let params = r#"{"printBackground":true,"preferCSSPageSize":true,"paperWidth":8.27,"paperHeight":11.69,"generateDocumentOutline":true,"generateTaggedPDF":true}"#;
            printer.devtools("Page.printToPDF", params, move |result| {
                let pdf = result.map_err(PdfFailure::Print).and_then(|answer| pdf_bytes(&answer));
                finish(pdf);
            });
        });
        view.navigate(&url);
    });
    ExportHandle {
        cancel: Some(Rc::new(move || {
            *cancelled.borrow_mut() = true;
            abort_finish(Err(PdfFailure::Print("export timed out".to_owned())));
        })),
    }
}

#[cfg(test)]
mod tests {
    use ::core::prelude::v1::test;

    use super::*;

    #[test]
    fn cleans_file_names_as_3x() {
        assert_eq!(file_name("报告: 第一版?"), "报告- 第一版-");
        assert_eq!(file_name("a/b\\c"), "a-b-c");
        assert_eq!(file_name("  ..  "), "document");
        assert_eq!(file_name("notes. "), "notes");
    }

    #[test]
    fn reads_the_pdf_from_the_print_answer() {
        assert_eq!(pdf_bytes(r#"{"data":"JVBERg=="}"#), Ok(b"%PDF".to_vec()));
        assert_eq!(pdf_bytes(r#"{"stream":"1"}"#), Err(PdfFailure::Empty));
        assert_eq!(pdf_bytes(r#"{"data":"not base64!"}"#), Err(PdfFailure::Empty));
        assert_eq!(pdf_bytes("no json"), Err(PdfFailure::Empty));
    }

    #[test]
    fn export_file_url_encodes_spaces_and_unicode_and_blocks_other_paths() {
        let url = file_url(Path::new(r"C:\Users\Alice Smith\测试\document.html"));
        assert_eq!(url, "file:///C:/Users/Alice%20Smith/%E6%B5%8B%E8%AF%95/document.html");
        assert!(is_allowed_export_url("about:blank", &url));
        assert!(is_allowed_export_url(&url, &url));
        assert!(is_allowed_export_url("file:///c:/Users/Alice%20Smith/%e6%b5%8b%e8%af%95/document.html", &url));
        assert!(!is_allowed_export_url("file:///C:/Windows/win.ini", &url));
        assert!(!is_allowed_export_url("https://example.com/", &url));
        assert!(!is_allowed_export_url("data:text/html,<script>alert(1)</script>", &url));
    }
}
