//! Exported Markdown documents (`src/main/document-export.ts`): the page builds a self-contained HTML file, and this prints
//! it to an A4 PDF in a hidden WebView2 that opens nothing else, with its headings as bookmarks and text that copies and
//! searches as written.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use base64::Engine as _;
use serde_json::Value;
use windows::Win32::Foundation::HWND;

use super::webview::{Options, WebView};

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

/// Print `html` to a PDF; `done` runs on the UI thread with the file's bytes.
pub fn print_to_pdf(parent: HWND, profile: PathBuf, html: String, done: impl FnOnce(Result<Vec<u8>, String>) + 'static) {
    let folder = std::env::temp_dir().join(format!("rainy-export-{}-{}", std::process::id(), chrono::Utc::now().timestamp_millis()));
    let page = folder.join("document.html");
    if let Err(error) = std::fs::create_dir_all(&folder).and_then(|()| std::fs::write(&page, html)) {
        done(Err(error.to_string()));
        return;
    }
    let url = format!("file:///{}", page.to_string_lossy().replace('\\', "/"));
    let allowed = url.clone();
    let done: Rc<RefCell<Option<Box<dyn FnOnce(Result<Vec<u8>, String>)>>>> = Rc::new(RefCell::new(Some(Box::new(done))));
    // The control lives until the PDF is made; then it and the page go.
    let holder: Rc<RefCell<Option<WebView>>> = Rc::default();
    let finish = {
        let (done, holder, folder) = (done.clone(), holder.clone(), folder.clone());
        Rc::new(move |result: Result<Vec<u8>, String>| {
            log::info!("Markdown export printed: {}", result.as_ref().map_or_else(|error| error.clone(), |pdf| format!("{} bytes", pdf.len())));
            let _ = std::fs::remove_dir_all(&folder);
            if let Some(done) = done.borrow_mut().take() {
                done(result);
            }
            // Closed once WebView2 is out of the callback that got here.
            if let Some(view) = holder.borrow_mut().take() {
                CLOSING.with(|closing| closing.borrow_mut().push(view));
            }
        })
    };
    let options = Options {
        folder: profile,
        private: true,
        background: (255, 255, 255),
        script: None,
        on_message: None,
        allow: Some(Box::new(move |address| address == allowed || address.starts_with("about:") || address.starts_with("data:"))),
        on_new_window: Some(Box::new(|_| {})),
        on_shortcut: None,
    };
    let failed = finish.clone();
    WebView::create(parent, options, move |created| {
        let view = match created {
            Ok(view) => view,
            Err(error) => {
                failed(Err(error));
                return;
            }
        };
        view.set_visible(false);
        view.set_bounds(0, 0, 1000, 1400);
        *holder.borrow_mut() = Some(view.clone());
        let printing = Rc::new(RefCell::new(false));
        let (printer, finish) = (view.clone(), finish.clone());
        view.on_devtools_event("Page.loadEventFired", move |_| {
            if std::mem::replace(&mut *printing.borrow_mut(), true) {
                return;
            }
            // Fonts load after the page; the PDF waits for them.
            let (printer, finish) = (printer.clone(), finish.clone());
            let wait = r#"{"expression":"document.fonts.ready.then(() => true)","awaitPromise":true}"#;
            printer.clone().devtools("Runtime.evaluate", wait, move |_| {
                let params = r#"{"printBackground":true,"preferCSSPageSize":true,"paperWidth":8.27,"paperHeight":11.69,"generateDocumentOutline":true,"generateTaggedPDF":true}"#;
                printer.devtools("Page.printToPDF", params, move |result| {
                    let pdf = result.and_then(|json| {
                        let value: Value = serde_json::from_str(&json).map_err(|error| error.to_string())?;
                        let data = value["data"].as_str().ok_or_else(|| "WebView2 returned no PDF.".to_owned())?;
                        base64::engine::general_purpose::STANDARD.decode(data).map_err(|error| error.to_string())
                    });
                    finish(pdf.map(super::pdf_text::searchable_pdf));
                });
            });
        });
        let navigator = view.clone();
        view.devtools("Page.enable", "{}", move |_| navigator.navigate(&url));
    });
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
}
