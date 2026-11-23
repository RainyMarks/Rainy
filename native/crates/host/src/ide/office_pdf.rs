//! One-shot, cookie-authenticated `/rainy/ide/office-pdf` delivery for converted Office PDFs.
//!
//! Eliminates 200 MiB Base64 round-trips between GPUI and WebView2:
//! - Local Windows Host: GPUI registers a temporary `.pdf` via `ide.registerOfficePdf` / `ide { op: "officePdf.register" }`.
//! - Remote WSL/SSH Host: GPUI streams raw PDF bytes via `POST /rainy/ide/office-pdf`.
//! - Viewer page: fetches `GET /rainy/ide/office-pdf?token=<token>` once; the token is consumed immediately and the
//!   temporary file is deleted once streaming finishes or when the TTL expires.

use std::collections::HashMap;
use std::io::Read as _;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::Request;
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::response::Response;
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use futures::StreamExt as _;
use rainy_protocol::ide::OfficePdfRegistration;
use tokio::io::AsyncWriteExt as _;
use url::Url;

use crate::error::{HostError, HostResult};
use crate::locale::l;
use crate::paths::{contains, realpath};

const MAX_PDF_BYTES: u64 = 256 * 1024 * 1024;
const TOKEN_TTL: Duration = Duration::from_secs(120);
const MAX_ACTIVE_TOKENS: usize = 32;
const STREAM_CHUNK_BYTES: usize = 256 * 1024;
const PDF_MAGIC: &[u8; 5] = b"%PDF-";

struct Entry {
    path: PathBuf,
    delete_after_read: bool,
    created_at: Instant,
    expires_at: Instant,
}

pub struct OfficePdfStore {
    upload_dir: PathBuf,
    entries: Mutex<HashMap<String, Entry>>,
}

fn random_token() -> String {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("random bytes");
    URL_SAFE_NO_PAD.encode(bytes)
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn status(code: StatusCode) -> Response {
    let mut response = Response::new(Body::empty());
    *response.status_mut() = code;
    response
}

fn secure_headers(response: &mut Response) {
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static("sandbox; default-src 'none'"));
    headers.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    headers.insert("cross-origin-resource-policy", HeaderValue::from_static("same-origin"));
}

fn remove_temp_pdf(path: &Path) {
    let _ = std::fs::remove_file(path);
    // Clean up an empty `rainy-office-*` parent folder created by desktop Office conversion.
    if let Some(parent) = path.parent() {
        let is_office_dir = parent
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|name| name.starts_with("rainy-office-"));
        if is_office_dir {
            let _ = std::fs::remove_dir(parent);
        }
    }
}

struct DeleteOnDrop {
    path: Option<PathBuf>,
}

impl Drop for DeleteOnDrop {
    fn drop(&mut self) {
        if let Some(path) = self.path.take() {
            remove_temp_pdf(&path);
        }
    }
}

impl OfficePdfStore {
    pub fn new(host_tmp: PathBuf) -> Arc<Self> {
        let upload_dir = host_tmp.join("office-pdf");
        Arc::new(Self { upload_dir, entries: Mutex::default() })
    }

    fn prune_locked(entries: &mut HashMap<String, Entry>) {
        let now = Instant::now();
        let expired: Vec<String> = entries.iter().filter(|(_, e)| now >= e.expires_at).map(|(k, _)| k.clone()).collect();
        for key in expired {
            if let Some(entry) = entries.remove(&key) {
                if entry.delete_after_read {
                    remove_temp_pdf(&entry.path);
                }
            }
        }
    }

    fn allowed_temp_pdf(&self, raw_path: &Path) -> HostResult<PathBuf> {
        let meta = std::fs::symlink_metadata(raw_path).map_err(|_| HostError::new("not-found", l("临时 PDF 文件不存在。", "The temporary PDF file does not exist.")))?;
        if meta.file_type().is_symlink() || !meta.is_file() {
            return Err(HostError::invalid(l("只允许注册受限临时目录中的普通 PDF 文件。", "Only regular PDF files inside the restricted temp directory may be registered.")));
        }
        let canonical = realpath(raw_path)?;
        let is_pdf = canonical.extension().and_then(|e| e.to_str()).is_some_and(|ext| ext.eq_ignore_ascii_case("pdf"));
        if !is_pdf {
            return Err(HostError::invalid(l("仅允许注册 .pdf 文件。", "Only .pdf files may be registered.")));
        }
        let in_upload_dir = realpath(&self.upload_dir).ok().is_some_and(|base| contains(&base, &canonical));
        let in_office_temp = realpath(&std::env::temp_dir()).ok().is_some_and(|sys_tmp| {
            if !contains(&sys_tmp, &canonical) {
                return false;
            }
            let Ok(rel) = canonical.strip_prefix(&sys_tmp) else { return false };
            matches!(rel.components().next(), Some(Component::Normal(first)) if first.to_string_lossy().starts_with("rainy-office-"))
        });
        if !in_upload_dir && !in_office_temp {
            return Err(HostError::invalid(l("PDF 文件不在允许的受限临时目录中。", "The PDF file is outside the restricted temporary directory.")));
        }
        if meta.len() < PDF_MAGIC.len() as u64 || meta.len() > MAX_PDF_BYTES {
            return Err(HostError::new("too-large", l("PDF 文件大小超出允许范围。", "The PDF file size is outside the allowed bounds.")));
        }
        let mut file = std::fs::File::open(&canonical)?;
        let mut magic = [0u8; 5];
        file.read_exact(&mut magic).map_err(|_| HostError::invalid(l("文件不是有效的 PDF。", "The file is not a valid PDF.")))?;
        if &magic != PDF_MAGIC {
            return Err(HostError::invalid(l("文件不是有效的 PDF。", "The file is not a valid PDF.")));
        }
        Ok(canonical)
    }

    fn insert(&self, path: PathBuf, delete_after_read: bool) -> OfficePdfRegistration {
        let token = random_token();
        let now = Instant::now();
        let mut entries = self.entries.lock().unwrap();
        Self::prune_locked(&mut entries);
        while entries.len() >= MAX_ACTIVE_TOKENS {
            let Some(oldest) = entries.iter().min_by_key(|(_, e)| e.created_at).map(|(k, _)| k.clone()) else {
                break;
            };
            if let Some(evicted) = entries.remove(&oldest) {
                if evicted.delete_after_read {
                    remove_temp_pdf(&evicted.path);
                }
            }
        }
        entries.insert(token.clone(), Entry { path, delete_after_read, created_at: now, expires_at: now + TOKEN_TTL });
        OfficePdfRegistration {
            url: format!("/rainy/ide/office-pdf?token={token}"),
            token,
            expires_in_secs: TOKEN_TTL.as_secs(),
        }
    }

    /// Register an existing temporary `.pdf` file located inside `host.env.tmp` or `<temp_dir>/rainy-office-*`.
    pub fn register(&self, path: &str, delete_after_read: bool) -> HostResult<OfficePdfRegistration> {
        if path.trim().is_empty() || path.contains('\0') {
            return Err(HostError::invalid(l("PDF 路径无效。", "Invalid PDF path.")));
        }
        let canonical = self.allowed_temp_pdf(Path::new(path))?;
        Ok(self.insert(canonical, delete_after_read))
    }

    fn lookup(&self, token: &str, consume: bool) -> Option<Entry> {
        if token.is_empty() || token.len() > 128 {
            return None;
        }
        let mut entries = self.entries.lock().unwrap();
        Self::prune_locked(&mut entries);
        let matched_key = entries.keys().find(|k| constant_time_eq(k.as_bytes(), token.as_bytes())).cloned()?;
        if consume {
            entries.remove(&matched_key)
        } else {
            entries.get(&matched_key).map(|e| Entry {
                path: e.path.clone(),
                delete_after_read: false,
                created_at: e.created_at,
                expires_at: e.expires_at,
            })
        }
    }

    /// Remove all tracked tokens and clean up any hosted temporary PDFs on Host shutdown.
    pub fn close(&self) {
        let mut entries = self.entries.lock().unwrap();
        for (_, entry) in entries.drain() {
            if entry.delete_after_read {
                remove_temp_pdf(&entry.path);
            }
        }
        let _ = std::fs::remove_dir_all(&self.upload_dir);
    }

    /// HTTP route handler for `POST` (stream upload for WSL/SSH) and `GET`/`HEAD` (one-shot streamed read).
    pub async fn serve(self: Arc<Self>, server: Arc<crate::server::HostServer>, request: Request, url: Url) -> Response {
        if !server.origin_ok_if_present(request.headers()) {
            let mut resp = status(StatusCode::FORBIDDEN);
            secure_headers(&mut resp);
            return resp;
        }
        match *request.method() {
            Method::POST => self.handle_upload(request).await,
            Method::GET | Method::HEAD => self.handle_download(request, url).await,
            _ => {
                let mut resp = status(StatusCode::METHOD_NOT_ALLOWED);
                resp.headers_mut().insert(header::ALLOW, HeaderValue::from_static("GET, HEAD, POST"));
                secure_headers(&mut resp);
                resp
            }
        }
    }

    async fn handle_upload(self: Arc<Self>, request: Request) -> Response {
        if crate::store::create_private_dir(&self.upload_dir).is_err() {
            let mut resp = status(StatusCode::INTERNAL_SERVER_ERROR);
            secure_headers(&mut resp);
            return resp;
        }
        let target = self.upload_dir.join(format!("{}.pdf", uuid::Uuid::new_v4().simple()));
        let Ok(mut file) = tokio::fs::OpenOptions::new().write(true).create_new(true).open(&target).await else {
            let mut resp = status(StatusCode::INTERNAL_SERVER_ERROR);
            secure_headers(&mut resp);
            return resp;
        };
        let mut stream = request.into_body().into_data_stream();
        let mut total: u64 = 0;
        let mut head = Vec::with_capacity(5);
        while let Some(chunk) = stream.next().await {
            let Ok(bytes) = chunk else {
                drop(file);
                let _ = tokio::fs::remove_file(&target).await;
                let mut resp = status(StatusCode::BAD_REQUEST);
                secure_headers(&mut resp);
                return resp;
            };
            total = total.saturating_add(bytes.len() as u64);
            if total > MAX_PDF_BYTES {
                drop(file);
                let _ = tokio::fs::remove_file(&target).await;
                let mut resp = status(StatusCode::PAYLOAD_TOO_LARGE);
                secure_headers(&mut resp);
                return resp;
            }
            if head.len() < PDF_MAGIC.len() {
                let need = PDF_MAGIC.len() - head.len();
                head.extend_from_slice(&bytes[..bytes.len().min(need)]);
                if head.len() == PDF_MAGIC.len() && head.as_slice() != PDF_MAGIC {
                    drop(file);
                    let _ = tokio::fs::remove_file(&target).await;
                    let mut resp = status(StatusCode::BAD_REQUEST);
                    secure_headers(&mut resp);
                    return resp;
                }
            }
            if file.write_all(&bytes).await.is_err() {
                drop(file);
                let _ = tokio::fs::remove_file(&target).await;
                let mut resp = status(StatusCode::INTERNAL_SERVER_ERROR);
                secure_headers(&mut resp);
                return resp;
            }
        }
        if head.as_slice() != PDF_MAGIC || file.flush().await.is_err() {
            drop(file);
            let _ = tokio::fs::remove_file(&target).await;
            let mut resp = status(StatusCode::BAD_REQUEST);
            secure_headers(&mut resp);
            return resp;
        }
        drop(file);
        let canonical = realpath(&target).unwrap_or(target);
        let registration = self.insert(canonical, true);
        let body = serde_json::to_vec(&registration).unwrap_or_default();
        let mut response = Response::new(Body::from(body));
        response.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json; charset=utf-8"));
        secure_headers(&mut response);
        response
    }

    async fn handle_download(self: Arc<Self>, request: Request, url: Url) -> Response {
        let query: HashMap<String, String> = url.query_pairs().map(|(k, v)| (k.into_owned(), v.into_owned())).collect();
        if query.len() != 1 {
            let mut resp = status(StatusCode::BAD_REQUEST);
            secure_headers(&mut resp);
            return resp;
        }
        let Some(token) = query.get("token") else {
            let mut resp = status(StatusCode::BAD_REQUEST);
            secure_headers(&mut resp);
            return resp;
        };
        let is_head = request.method() == Method::HEAD;
        let Some(entry) = self.lookup(token, !is_head) else {
            let mut resp = status(StatusCode::NOT_FOUND);
            secure_headers(&mut resp);
            return resp;
        };
        let cleanup_path = entry.delete_after_read.then(|| entry.path.clone());
        let file_path = entry.path.clone();
        let opened = tokio::task::spawn_blocking(move || -> std::io::Result<(std::fs::File, u64)> {
            let meta = std::fs::symlink_metadata(&file_path)?;
            if meta.file_type().is_symlink() || !meta.is_file() {
                return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "not a regular file"));
            }
            let file = std::fs::File::open(&file_path)?;
            Ok((file, meta.len()))
        })
        .await;
        let (file, size) = match opened {
            Ok(Ok(opened)) => opened,
            _ => {
                if let Some(path) = cleanup_path {
                    remove_temp_pdf(&path);
                }
                let mut resp = status(StatusCode::NOT_FOUND);
                secure_headers(&mut resp);
                return resp;
            }
        };
        let mut response = status(StatusCode::OK);
        {
            let headers = response.headers_mut();
            headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/pdf"));
            headers.insert(header::CONTENT_LENGTH, HeaderValue::from(size));
        }
        secure_headers(&mut response);
        if is_head || size == 0 {
            if let Some(path) = cleanup_path {
                remove_temp_pdf(&path);
            }
            return response;
        }
        let (sender, mut receiver) = tokio::sync::mpsc::channel::<Result<Vec<u8>, std::io::Error>>(4);
        tokio::task::spawn_blocking(move || {
            let _guard = DeleteOnDrop { path: cleanup_path };
            {
                let mut file = file;
                let mut buffer = vec![0u8; STREAM_CHUNK_BYTES];
                loop {
                    match file.read(&mut buffer) {
                        Ok(0) => break,
                        Ok(read) => {
                            if sender.blocking_send(Ok(buffer[..read].to_vec())).is_err() {
                                break;
                            }
                        }
                        Err(error) => {
                            let _ = sender.blocking_send(Err(error));
                            break;
                        }
                    }
                }
            }
        });
        *response.body_mut() = Body::from_stream(futures::stream::poll_fn(move |cx| receiver.poll_recv(cx)));
        response
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_restricted_temp_pdf_boundaries_and_one_shot_consumption() {
        let host_tmp = tempfile::tempdir().unwrap();
        let store = OfficePdfStore::new(host_tmp.path().to_path_buf());

        // Reject arbitrary file outside temp boundary.
        let outside = tempfile::tempdir().unwrap();
        let outside_pdf = outside.path().join("outside.pdf");
        std::fs::write(&outside_pdf, b"%PDF-1.7\nhello").unwrap();
        assert!(store.register(outside_pdf.to_str().unwrap(), false).is_err());

        // Reject PDF directly inside host_tmp outside `office-pdf/`.
        let outside_subdir = host_tmp.path().join("stray.pdf");
        std::fs::write(&outside_subdir, b"%PDF-1.7\nhello").unwrap();
        assert!(store.register(outside_subdir.to_str().unwrap(), false).is_err());

        let upload_dir = host_tmp.path().join("office-pdf");
        std::fs::create_dir_all(&upload_dir).unwrap();

        // Reject non-PDF magic inside allowed dir.
        let bad_magic = upload_dir.join("fake.pdf");
        std::fs::write(&bad_magic, b"NOTAPDF").unwrap();
        assert!(store.register(bad_magic.to_str().unwrap(), false).is_err());

        // Reject non-.pdf extension inside allowed dir.
        let bad_ext = upload_dir.join("doc.txt");
        std::fs::write(&bad_ext, b"%PDF-1.7\n").unwrap();
        assert!(store.register(bad_ext.to_str().unwrap(), false).is_err());

        // Accept valid PDF in host_tmp/office-pdf and consume token once.
        let good = upload_dir.join("ok.pdf");
        std::fs::write(&good, b"%PDF-1.7\ncontent").unwrap();
        let reg = store.register(good.to_str().unwrap(), true).unwrap();
        assert!(reg.url.starts_with("/rainy/ide/office-pdf?token="));
        assert!(store.lookup(&reg.token, false).is_some());
        assert!(store.lookup(&reg.token, true).is_some());
        assert!(store.lookup(&reg.token, true).is_none());

        // Accept valid PDF in <system_temp>/rainy-office-*.
        let office_dir = std::env::temp_dir().join(format!("rainy-office-test-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&office_dir).unwrap();
        let office_pdf = office_dir.join("document.pdf");
        std::fs::write(&office_pdf, b"%PDF-1.4\nbody").unwrap();
        let reg2 = store.register(office_pdf.to_str().unwrap(), true).unwrap();
        assert!(store.lookup(&reg2.token, true).is_some());
        remove_temp_pdf(&office_pdf);
        assert!(!office_pdf.exists());
    }
}
