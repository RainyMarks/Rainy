//! Page bridge calls handled by the desktop: `open-external`, Markdown HTML/PDF export with timeout protection,
//! and Office COM-to-PDF conversion strictly scoped to active workspace documents.

use std::path::{Path, PathBuf};

use gpui_kit::*;
use rainy_carrier::ExecutionTarget;
use rainy_carrier::text::percent_encode as encode;
use serde_json::{Value, json};

use crate::store::Store;
use crate::ui::ide::viewers;
use crate::ui::{tr, trf};

use super::EditorArea;

impl EditorArea {
    /// Requests only the desktop can carry out for a page.
    pub(super) fn answer(&mut self, key: &str, id: u64, name: &str, params: &Value, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pane) = self.viewers.get(key).map(|viewer| viewer.pane.downgrade()) else { return };
        // Hold a weak handle so long-running export/Office conversions never keep a closed WebPane alive.
        let respond = move |result: Value, cx: &mut App| {
            if let Some(pane) = pane.upgrade()
                && let Some(view) = pane.read(cx).view()
            {
                view.execute(&viewers::resolve_script(id, &result), |_| {});
            }
        };
        match name {
            "open-external" => {
                let raw = params["path"].as_str().unwrap_or_default();
                let windows = matches!(Store::global(cx).read(cx).target, None | Some(ExecutionTarget::Windows { .. }));
                let roots: Vec<String> = self
                    .ide
                    .read(cx)
                    .workspace
                    .as_ref()
                    .map(|ws| {
                        let mut paths: Vec<String> = ws.roots().iter().map(|r| r.path.clone()).collect();
                        if !paths.iter().any(|p| p == &ws.path) {
                            paths.push(ws.path.clone());
                        }
                        paths
                    })
                    .unwrap_or_default();
                let validated = if windows { validate_open_external_path(raw, &roots) } else { None };
                match validated {
                    Some(safe_path) => {
                        if params["reveal"] == true {
                            cx.reveal_path(&safe_path);
                        } else {
                            cx.open_with_system(&safe_path);
                        }
                        respond(json!({}), cx);
                    }
                    None => {
                        log::warn!("Blocked open-external request for path outside workspace or unsafe type: {raw}");
                        respond(json!({ "error": tr(cx, "native.openExternalDenied").to_string() }), cx);
                    }
                }
            }
            // An exported Markdown document: ask where, then write the page or print it to PDF.
            "export-save" => {
                let pdf = params["format"] == "pdf";
                let html = params["html"].as_str().unwrap_or_default().to_owned();
                let name = crate::web::export::file_name(params["name"].as_str().unwrap_or_default());
                let extension = if pdf { "pdf" } else { "html" };
                let title = tr(cx, if pdf { "viewers.exportPdf" } else { "viewers.exportHtml" }).trim_end_matches('…').to_owned();
                let folder = self.export_folder(key, cx);
                // Debug builds under test save without the dialog, which would come up over the tester's own windows.
                let qa_folder = if cfg!(debug_assertions) { std::env::var_os("RAINY_QA_EXPORT_DIR").map(PathBuf::from) } else { None };
                let qa_path = qa_folder.map(|folder| folder.join(format!("{name}.{extension}")));
                let chosen = qa_path.is_none().then(|| {
                    crate::ui::save_dialog::save(
                        window,
                        crate::ui::save_dialog::SaveDialog {
                            title,
                            folder,
                            name: format!("{name}.{extension}"),
                            type_name: extension.to_uppercase(),
                            extension: extension.to_owned(),
                        },
                    )
                });
                let parent = crate::web::hwnd(window);
                let profile = Store::global(cx).read(cx).paths.user_data.join("WebView2");
                cx.spawn_in(window, async move |_, cx| {
                    let answer = match qa_path {
                        Some(path) => Ok(Ok(Some(path))),
                        None => match chosen {
                            Some(chosen) => chosen.recv().await,
                            None => Ok(Ok(None)),
                        },
                    };
                    let path = match answer {
                        Ok(Ok(Some(path))) => path,
                        Ok(Ok(None)) => {
                            let _ = cx.update(|_, cx| respond(json!({}), cx));
                            return;
                        }
                        Ok(Err(error)) => {
                            let _ = cx.update(|_, cx| respond(json!({ "error": error.to_string() }), cx));
                            return;
                        }
                        // The dialog's thread ended without an answer.
                        Err(_) => {
                            let _ = cx.update(|_, cx| respond(json!({ "error": tr(cx, "viewers.exportDialogFailed").to_string() }), cx));
                            return;
                        }
                    };
                    let bytes = if pdf {
                        let (sender, receiver) = async_channel::bounded(1);
                        let handle = cx.update(|_, _| {
                            parent.map(|parent| {
                                crate::web::export::print_to_pdf(parent, profile, html, move |result| {
                                    let _ = sender.try_send(result);
                                })
                            })
                        });
                        let Ok(Some(handle)) = handle else {
                            let _ = cx.update(|_, cx| respond(json!({ "error": tr(cx, "viewers.exportNoWindow").to_string() }), cx));
                            return;
                        };
                        let (done_tx, done_rx) = async_channel::bounded(1);
                        let recv_tx = done_tx.clone();
                        cx.background_spawn(async move {
                            let _ = recv_tx.try_send(Some(receiver.recv().await));
                        })
                        .detach();
                        let timer = cx.background_executor().timer(crate::web::export::EXPORT_TIMEOUT);
                        cx.background_spawn(async move {
                            timer.await;
                            let _ = done_tx.try_send(None);
                        })
                        .detach();
                        let raced = done_rx.recv().await;
                        let Ok(made) = cx.update(|_, cx| {
                            let outcome = match raced {
                                Ok(Some(Ok(made))) => made.map_err(|failure| pdf_failure(&failure, cx)),
                                Ok(Some(Err(_))) => Err(tr(cx, "viewers.exportPdfNotMade").to_string()),
                                Ok(None) => {
                                    handle.abort();
                                    Err(pdf_failure(&crate::web::export::PdfFailure::Print("export timed out".to_owned()), cx))
                                }
                                Err(_) => Err(tr(cx, "viewers.exportPdfNotMade").to_string()),
                            };
                            crate::web::export::close_finished();
                            outcome
                        }) else {
                            return;
                        };
                        made
                    } else {
                        Ok(html.into_bytes())
                    };
                    let target = path.clone();
                    let written = match bytes {
                        Ok(bytes) => cx.background_spawn(async move { std::fs::write(&target, bytes).map_err(|error| error.to_string()) }).await,
                        Err(error) => Err(error),
                    };
                    let result = match written {
                        Ok(()) => json!({ "path": path.to_string_lossy() }),
                        Err(error) => json!({ "error": error }),
                    };
                    let _ = cx.update(|_, cx| respond(result, cx));
                })
                .detach();
            }
            // Slides and old Word files are drawn as the PDF the installed Office makes; delivered via the Host's
            // single-use `/rainy/ide/office-pdf?token=...` route. Only workspace-verified files or Host-authorized
            // `/rainy/ide/raw` locations are accepted (arbitrary unvalidated base64 payloads are rejected).
            "office-pdf" => {
                let extension = params["extension"].as_str().unwrap_or_default().trim_start_matches('.').to_ascii_lowercase();
                if crate::web::office::kind(&extension).is_none() {
                    respond(json!({ "error": "Unsupported Office document extension", "code": "unsupported", "missing": false, "encrypted": false }), cx);
                    return;
                }
                let workspace_id = params["workspaceId"].as_str().map(str::to_owned);
                let root_id = params["rootId"].as_str().map(str::to_owned);
                let rel_path = params["path"].as_str().map(str::to_owned);
                let version = params["version"].as_str().map(str::to_owned);
                let store = Store::global(cx);
                let (rpc, windows) = {
                    let s = store.read(cx);
                    (s.rpc.clone(), matches!(s.target, None | Some(ExecutionTarget::Windows { .. })))
                };
                // On Windows, locate the file directly inside the active workspace to avoid downloading it through HTTP.
                let local_source = if windows {
                    if let Some(rel) = rel_path.as_deref() {
                        self.ide.read(cx).workspace.as_ref().and_then(|ws| {
                            let ws_roots = ws.roots();
                            let base = ws_roots
                                .iter()
                                .find(|r| root_id.as_deref() == Some(r.root_id.as_str()) || (root_id.is_none() && r.primary))
                                .map_or(ws.path.as_str(), |r| r.path.as_str());
                            let candidate = Path::new(base).join(rel.replace('/', "\\"));
                            let roots: Vec<String> = ws_roots.iter().map(|r| r.path.clone()).chain(std::iter::once(ws.path.clone())).collect();
                            validate_workspace_file(&candidate.to_string_lossy(), &roots)
                        })
                    } else {
                        None
                    }
                } else {
                    None
                };
                let raw_query = match (workspace_id.as_deref(), rel_path.as_deref(), version.as_deref()) {
                    (Some(ws), Some(path), Some(ver)) => {
                        let mut q = format!("/rainy/ide/raw?w={}&p={}&v={}", encode(ws), encode(path), encode(ver));
                        if let Some(r) = root_id.as_deref() {
                            q.push_str(&format!("&r={}", encode(r)));
                        }
                        Some(q)
                    }
                    _ => None,
                };
                cx.spawn(async move |_, cx: &mut AsyncApp| {
                    let converted = cx
                        .background_spawn({
                            let rpc = rpc.clone();
                            async move {
                                if let Some(local) = local_source {
                                    return crate::web::office::convert_file_to_pdf(&local, &extension, None);
                                }
                                if let (Some(query), Some(rpc)) = (raw_query, rpc) {
                                    let folder = crate::web::office::create_staging_dir()?;
                                    let input = folder.join(format!("document.{extension}"));
                                    match rpc.get_to_file(&query, &input).await {
                                        Ok(200..=299) => return crate::web::office::convert_file_to_pdf(&input, &extension, Some(folder)),
                                        Ok(status) => {
                                            crate::web::office::ConvertedFile::cleanup(folder);
                                            return Err(crate::web::office::Failure {
                                                code: "failed",
                                                error: format!("HTTP {status}"),
                                                missing: false,
                                                encrypted: false,
                                            });
                                        }
                                        Err(error) => {
                                            crate::web::office::ConvertedFile::cleanup(folder);
                                            return Err(crate::web::office::Failure {
                                                code: "failed",
                                                error: error.to_string(),
                                                missing: false,
                                                encrypted: false,
                                            });
                                        }
                                    }
                                }
                                Err(crate::web::office::Failure {
                                    code: "failed",
                                    error: "Missing document location".to_owned(),
                                    missing: false,
                                    encrypted: false,
                                })
                            }
                        })
                        .await;
                    let result = match converted {
                        Ok(converted) => {
                            let delivered: Result<Value, String> = if let Some(rpc) = rpc {
                                if windows {
                                    let pdf_str = converted.pdf_path.to_string_lossy().to_string();
                                    let reg = rpc.call("ide", json!({ "op": "officePdf.register", "path": pdf_str, "deleteAfterRead": true })).await;
                                    match reg {
                                        Ok(value) => Ok(value),
                                        Err(error) => {
                                            let folder = converted.folder.clone();
                                            cx.background_spawn(async move { crate::web::office::ConvertedFile::cleanup(folder) }).detach();
                                            Err(error.message)
                                        }
                                    }
                                } else {
                                    let pdf_path = converted.pdf_path.clone();
                                    let folder = converted.folder.clone();
                                    cx.background_spawn(async move {
                                        let uploaded = rpc.post_file("/rainy/ide/office-pdf", "application/pdf", &pdf_path).await;
                                        crate::web::office::ConvertedFile::cleanup(folder);
                                        match uploaded {
                                            Ok((200..=299, body)) => serde_json::from_slice::<Value>(&body).map_err(|e| e.to_string()),
                                            Ok((status, body)) => Err(format!("HTTP {status} {}", String::from_utf8_lossy(&body))),
                                            Err(error) => Err(error.to_string()),
                                        }
                                    })
                                    .await
                                }
                            } else {
                                let folder = converted.folder.clone();
                                cx.background_spawn(async move { crate::web::office::ConvertedFile::cleanup(folder) }).detach();
                                Err("Host disconnected".to_owned())
                            };
                            match delivered {
                                Ok(reg) => json!({
                                    "url": reg["url"],
                                    "token": reg["token"],
                                    "application": converted.application,
                                }),
                                Err(error) => json!({
                                    "code": "failed",
                                    "error": error,
                                    "missing": false,
                                    "encrypted": false,
                                }),
                            }
                        }
                        Err(failure) => json!({ "code": failure.code, "error": failure.error, "missing": failure.missing, "encrypted": failure.encrypted }),
                    };
                    cx.update(|cx| respond(result, cx));
                })
                .detach();
            }
            _ => respond(json!({ "error": format!("Unknown request: {name}"), "missing": false }), cx),
        }
    }

    /// Where the export dialog opens: the document's own folder when this computer can reach it, else Documents.
    fn export_folder(&self, key: &str, cx: &App) -> PathBuf {
        let windows = matches!(Store::global(cx).read(cx).target, None | Some(ExecutionTarget::Windows { .. }));
        let own = windows
            .then(|| self.ide.read(cx).language_target(key))
            .flatten()
            .and_then(|target| Path::new(&target.path).parent().map(Path::to_path_buf))
            .filter(|folder| folder.is_dir());
        own.unwrap_or_else(|| {
            let home = std::env::var_os("USERPROFILE").map(PathBuf::from).unwrap_or_default();
            let documents = home.join("Documents");
            if documents.is_dir() { documents } else { home }
        })
    }
}

/// Why a Markdown export has no PDF, in the interface language; the page puts it after "导出失败：".
fn pdf_failure(failure: &crate::web::export::PdfFailure, cx: &App) -> String {
    use crate::web::export::PdfFailure;
    match failure {
        PdfFailure::Page(detail) => trf(cx, "viewers.exportPdfPage", &[("error", detail)]),
        PdfFailure::Control(detail) => trf(cx, "viewers.exportPdfControl", &[("error", detail)]),
        PdfFailure::Print(detail) => trf(cx, "viewers.exportPdfPrint", &[("error", detail)]),
        PdfFailure::Empty => tr(cx, "viewers.exportPdfEmpty"),
    }
    .to_string()
}

/// Safe document and media extensions allowed for `open-external`.
/// Strictly excludes executables, scripts, shortcuts, installers, HTML/SVG and macro-enabled Office formats.
const SAFE_EXTERNAL_EXTENSIONS: &[&str] = &[
    "pdf", "doc", "docx", "dot", "dotx", "wps", "odt", "rtf",
    "xls", "xlsx", "xlt", "xltx", "ods", "csv", "tsv",
    "ppt", "pptx", "pps", "ppsx", "pot", "potx", "odp", "dps",
    "txt", "md", "markdown", "epub",
    "png", "apng", "jpg", "jpeg", "jfif", "gif", "webp", "bmp", "tif", "tiff", "ico", "avif",
    "mp3", "wav", "ogg", "oga", "opus", "flac", "m4a", "aac", "weba",
    "mp4", "m4v", "webm", "ogv", "mov", "mkv",
];

/// Strip the Windows verbatim prefix (`\\?\`) for component comparisons; returns `None` for UNC or device paths.
fn strip_local_verbatim(path: &Path) -> Option<PathBuf> {
    let raw = path.to_str()?;
    let upper = raw.to_ascii_uppercase();
    if upper.starts_with(r"\\?\UNC\") || upper.starts_with(r"\\.\") || raw.starts_with(r"\\") && !raw.starts_with(r"\\?\") {
        return None;
    }
    let cleaned = raw.strip_prefix(r"\\?\").unwrap_or(raw);
    Some(PathBuf::from(cleaned))
}

/// Component-wise case-insensitive containment check on Windows paths (`C:\proj` never matches `C:\proj-evil`).
fn path_within_root(candidate: &Path, root: &Path) -> bool {
    let (Some(cand), Some(base)) = (strip_local_verbatim(candidate), strip_local_verbatim(root)) else {
        return false;
    };
    let mut cand_comps = cand.components();
    for base_comp in base.components() {
        let Some(cand_comp) = cand_comps.next() else {
            return false;
        };
        if !cand_comp.as_os_str().to_string_lossy().eq_ignore_ascii_case(&base_comp.as_os_str().to_string_lossy()) {
            return false;
        }
    }
    true
}

/// Reject UNC paths, device paths, control characters and NTFS Alternate Data Streams (`file.pdf:evil.exe`).
fn has_disallowed_path_syntax(raw: &str) -> bool {
    if raw.trim().is_empty() || raw.chars().any(|c| c.is_control()) {
        return true;
    }
    let trimmed = raw.trim();
    if trimmed.starts_with(r"\\") || trimmed.starts_with("//") {
        return true;
    }
    // Only a Windows drive prefix `X:` at byte index 1 may contain ':'.
    for (idx, ch) in trimmed.char_indices() {
        if ch == ':' {
            let valid_drive = idx == 1 && trimmed.as_bytes()[0].is_ascii_alphabetic();
            if !valid_drive {
                return true;
            }
        }
    }
    false
}

/// Validate that `raw` canonicalizes to an existing regular file inside one of the active workspace roots.
/// Returns a normalized local path without the Windows `\\?\` verbatim prefix so Shell APIs accept it.
fn validate_workspace_file(raw: &str, workspace_roots: &[String]) -> Option<PathBuf> {
    if has_disallowed_path_syntax(raw) || workspace_roots.is_empty() {
        return None;
    }
    let canonical = std::fs::canonicalize(raw).ok()?;
    if !canonical.is_file() {
        return None;
    }
    // Reject symlinks or reparse points that resolve to a UNC share, and strip `\\?\` for Windows Shell APIs.
    let clean = strip_local_verbatim(&canonical)?;
    let inside = workspace_roots.iter().filter_map(|root| std::fs::canonicalize(root).ok()).any(|root| path_within_root(&canonical, &root));
    inside.then_some(clean)
}

/// Validate `open-external`: path must canonicalize inside an active workspace root and carry an allowlisted document/media extension.
fn validate_open_external_path(raw: &str, workspace_roots: &[String]) -> Option<PathBuf> {
    let canonical = validate_workspace_file(raw, workspace_roots)?;
    let ext = canonical.extension()?.to_str()?.to_ascii_lowercase();
    SAFE_EXTERNAL_EXTENSIONS.contains(&ext.as_str()).then_some(canonical)
}

#[cfg(test)]
mod tests {
    use ::core::prelude::v1::test;

    use super::*;

    #[test]
    fn open_external_enforces_workspace_boundary_and_safe_extensions() {
        let base = tempfile::tempdir().expect("tempdir");
        let root = base.path().join("proj");
        let sibling = base.path().join("proj-evil");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&sibling).unwrap();

        let safe_pdf = root.join("report.pdf");
        let safe_docx = root.join("notes.docx");
        let safe_png = root.join("chart.PNG");
        std::fs::write(&safe_pdf, b"%PDF-1.4").unwrap();
        std::fs::write(&safe_docx, b"PK\x03\x04").unwrap();
        std::fs::write(&safe_png, b"\x89PNG").unwrap();

        let outside_pdf = sibling.join("leak.pdf");
        std::fs::write(&outside_pdf, b"%PDF-1.4").unwrap();

        let roots = vec![root.to_string_lossy().to_string()];

        let resolved_pdf = validate_open_external_path(&safe_pdf.to_string_lossy(), &roots).expect("safe pdf allowed");
        assert!(!resolved_pdf.to_string_lossy().starts_with(r"\\?\"), "Shell APIs reject verbatim prefix: {}", resolved_pdf.display());
        let resolved_docx = validate_open_external_path(&safe_docx.to_string_lossy(), &roots).expect("safe docx allowed");
        assert!(!resolved_docx.to_string_lossy().starts_with(r"\\?\"));
        let resolved_png = validate_open_external_path(&safe_png.to_string_lossy(), &roots).expect("safe png allowed");
        assert!(!resolved_png.to_string_lossy().starts_with(r"\\?\"));

        // Traversal & sibling prefix attacks must be rejected.
        let traversal = root.join("..").join("proj-evil").join("leak.pdf");
        assert!(validate_open_external_path(&traversal.to_string_lossy(), &roots).is_none());
        assert!(validate_open_external_path(&outside_pdf.to_string_lossy(), &roots).is_none());

        // UNC, device, and ADS paths must be rejected.
        assert!(validate_open_external_path(r"\\attacker\share\doc.pdf", &roots).is_none());
        assert!(validate_open_external_path("//attacker/share/doc.pdf", &roots).is_none());
        assert!(validate_open_external_path(&format!("{}:evil.exe", safe_pdf.display()), &roots).is_none());

        // Executable, script, shortcut, and macro-enabled extensions must all be rejected.
        for bad in ["a.exe", "a.bat", "a.cmd", "a.ps1", "a.vbs", "a.msi", "a.lnk", "a.js", "a.hta", "a.docm", "a.xlsm", "a.html"] {
            let file = root.join(bad);
            std::fs::write(&file, b"test").unwrap();
            assert!(validate_open_external_path(&file.to_string_lossy(), &roots).is_none(), "expected rejection for {bad}");
        }
    }
}
