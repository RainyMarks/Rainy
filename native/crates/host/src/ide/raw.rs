//! `/rainy/ide/raw`: the bytes of one project file for the viewers, with single ranges for media seeking and hex paging
//! (3.x `ide/raw.ts`). Served files stay inert: a page navigated to one runs no script and gets no Host origin.

use std::io::{Read as _, Seek as _};
use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::Request;
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::response::Response;
use futures::StreamExt as _;
use tokio::io::AsyncWriteExt as _;
use url::Url;

use super::files::Files;
use super::preview::media_type;
use crate::activity::Activity;
use crate::server::HostServer;

/// Maximum streamed upload size accepted by `POST /rainy/ide/upload` (512 MiB).
pub const MAX_UPLOAD_BYTES: u64 = 512 * 1024 * 1024;
const UPLOAD_WRITE_CHUNK_BYTES: usize = 256 * 1024;

/// One `bytes` range; anything else is served whole, as RFC 9110 allows. `Err` when it cannot be satisfied.
pub fn byte_range(header: Option<&str>, size: u64) -> Option<Result<(u64, u64), ()>> {
    let spec = header?.trim().strip_prefix("bytes=")?;
    let (first, last) = spec.split_once('-')?;
    if first.contains(',') || last.contains(',') || !first.chars().chain(last.chars()).all(|c| c.is_ascii_digit()) {
        return None;
    }
    if first.is_empty() && last.is_empty() {
        return None;
    }
    if first.is_empty() {
        let suffix: u64 = last.parse().ok()?;
        if suffix == 0 || size == 0 {
            return Some(Err(()));
        }
        return Some(Ok((size.saturating_sub(suffix), size - 1)));
    }
    let start: u64 = first.parse().ok()?;
    let end = if last.is_empty() { size.saturating_sub(1) } else { last.parse::<u64>().ok()?.min(size.saturating_sub(1)) };
    if start >= size || end < start {
        return Some(Err(()));
    }
    Some(Ok((start, end)))
}

fn status(code: StatusCode) -> Response {
    let mut response = Response::new(Body::empty());
    *response.status_mut() = code;
    response
}

fn inert(response: &mut Response) {
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static("sandbox; default-src 'none'; img-src 'self' data:; media-src 'self'; style-src 'unsafe-inline'"));
    headers.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    headers.insert("cross-origin-resource-policy", HeaderValue::from_static("same-origin"));
}

pub async fn serve(files: Arc<Files>, activity: Arc<Activity>, request: Request, url: Url) -> Response {
    let mut response = respond(files, activity, request, url).await;
    inert(&mut response);
    response
}

async fn respond(files: Arc<Files>, activity: Arc<Activity>, request: Request, url: Url) -> Response {
    if request.method() != Method::GET && request.method() != Method::HEAD {
        let mut response = status(StatusCode::METHOD_NOT_ALLOWED);
        response.headers_mut().insert(header::ALLOW, HeaderValue::from_static("GET, HEAD"));
        return response;
    }
    let query: std::collections::HashMap<String, String> = url.query_pairs().map(|(name, value)| (name.into_owned(), value.into_owned())).collect();
    if query.keys().any(|name| !["w", "r", "p", "v"].contains(&name.as_str())) {
        return status(StatusCode::BAD_REQUEST);
    }
    let (Some(workspace), Some(path)) = (query.get("w").filter(|w| !w.is_empty()), query.get("p").filter(|p| !p.is_empty())) else {
        return status(StatusCode::BAD_REQUEST);
    };
    if activity.assert_can_start().is_err() {
        return status(StatusCode::SERVICE_UNAVAILABLE);
    }
    let (workspace, root, path_owned) = (workspace.clone(), query.get("r").cloned(), path.clone());
    let opened = tokio::task::spawn_blocking(move || files.open_bytes(&workspace, root.as_deref(), &path_owned)).await;
    let (mut file, size, version) = match opened {
        Ok(Ok(opened)) => opened,
        Ok(Err(error)) => {
            let code = match error.code.as_str() {
                "version-conflict" => StatusCode::CONFLICT,
                "closed" | "busy" => StatusCode::SERVICE_UNAVAILABLE,
                "invalid-request" | "invalid-path" => StatusCode::BAD_REQUEST,
                _ => StatusCode::NOT_FOUND,
            };
            let mut response = status(code);
            response.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            return response;
        }
        Err(_) => return status(StatusCode::INTERNAL_SERVER_ERROR),
    };
    let etag = format!("\"{version}\"");
    let headers = request.headers();
    let header_text = |name: header::HeaderName| headers.get(name).and_then(|value| value.to_str().ok()).map(str::to_owned);
    let mut response = status(StatusCode::OK);
    {
        let out = response.headers_mut();
        out.insert(header::CONTENT_TYPE, HeaderValue::from_static(media_type(path)));
        out.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
        if let Ok(value) = HeaderValue::from_str(&etag) {
            out.insert(header::ETAG, value);
        }
        // A versioned URL names one observation of the file; an unversioned one is checked on every use.
        out.insert(header::CACHE_CONTROL, HeaderValue::from_static(if query.contains_key("v") { "private, max-age=3600" } else { "private, no-cache" }));
    }
    if header_text(header::IF_NONE_MATCH).is_some_and(|value| value.split(',').any(|tag| tag.trim() == etag || tag.trim() == "*")) {
        *response.status_mut() = StatusCode::NOT_MODIFIED;
        return response;
    }
    let range = match header_text(header::IF_RANGE) {
        Some(value) if value != etag => None,
        _ => byte_range(header_text(header::RANGE).as_deref(), size),
    };
    let (start, end) = match range {
        Some(Err(())) => {
            *response.status_mut() = StatusCode::RANGE_NOT_SATISFIABLE;
            if let Ok(value) = HeaderValue::from_str(&format!("bytes */{size}")) {
                response.headers_mut().insert(header::CONTENT_RANGE, value);
            }
            return response;
        }
        Some(Ok(bounds)) => {
            *response.status_mut() = StatusCode::PARTIAL_CONTENT;
            if let Ok(value) = HeaderValue::from_str(&format!("bytes {}-{}/{size}", bounds.0, bounds.1)) {
                response.headers_mut().insert(header::CONTENT_RANGE, value);
            }
            bounds
        }
        None => (0, size.saturating_sub(1)),
    };
    let length = if size == 0 { 0 } else { end - start + 1 };
    response.headers_mut().insert(header::CONTENT_LENGTH, HeaderValue::from(length));
    if request.method() == Method::HEAD || length == 0 {
        return response;
    }
    // The file is read on a blocking thread in chunks and streamed as it is read.
    let (sender, receiver) = tokio::sync::mpsc::channel::<Result<Vec<u8>, std::io::Error>>(4);
    tokio::task::spawn_blocking(move || {
        if let Err(error) = file.seek(std::io::SeekFrom::Start(start)) {
            let _ = sender.blocking_send(Err(error));
            return;
        }
        let mut left = length;
        let mut buffer = vec![0u8; 256 * 1024];
        while left > 0 {
            let want = buffer.len().min(left as usize);
            match file.read(&mut buffer[..want]) {
                Ok(0) => break,
                Ok(read) => {
                    left -= read as u64;
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
    });
    *response.body_mut() = Body::from_stream(tokio_stream_from(receiver));
    response
}

fn tokio_stream_from(mut receiver: tokio::sync::mpsc::Receiver<Result<Vec<u8>, std::io::Error>>) -> impl futures::Stream<Item = Result<Vec<u8>, std::io::Error>> {
    futures::stream::poll_fn(move |context| receiver.poll_recv(context))
}

struct TempCleanup {
    path: Option<PathBuf>,
}

impl TempCleanup {
    fn disarm(&mut self) {
        self.path = None;
    }
}

impl Drop for TempCleanup {
    fn drop(&mut self) {
        if let Some(path) = self.path.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn upload_error_response(code: StatusCode, err_code: &str, message: &str) -> Response {
    let body = serde_json::json!({ "code": err_code, "message": message }).to_string();
    let mut response = Response::new(Body::from(body));
    *response.status_mut() = code;
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json; charset=utf-8"));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

fn map_host_error(error: &crate::error::HostError) -> StatusCode {
    match error.code.as_str() {
        "outside-workspace" => StatusCode::FORBIDDEN,
        "already-exists" | "version-conflict" => StatusCode::CONFLICT,
        "invalid-request" | "invalid-path" | "not-file" => StatusCode::BAD_REQUEST,
        "not-found" => StatusCode::NOT_FOUND,
        "too-large" => StatusCode::PAYLOAD_TOO_LARGE,
        "closed" | "busy" => StatusCode::SERVICE_UNAVAILABLE,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

/// Streamed file upload into a validated workspace path (`POST /rainy/ide/upload?w=<ws>&r=<root>&p=<rel>&overwrite=<0|1>`).
/// Cookie + Host auth are enforced by `HostServer::route`; this handler additionally enforces same-origin, workspace
/// realpath confinement, 512 MiB byte cap and atomic staging/rename.
pub async fn serve_upload(files: Arc<Files>, server: Arc<HostServer>, activity: Arc<Activity>, request: Request, url: Url) -> Response {
    let mut response = respond_upload(files, server, activity, request, url).await;
    inert(&mut response);
    response
}

async fn respond_upload(files: Arc<Files>, server: Arc<HostServer>, activity: Arc<Activity>, request: Request, url: Url) -> Response {
    if !server.origin_ok_if_present(request.headers()) {
        return status(StatusCode::FORBIDDEN);
    }
    if request.method() != Method::POST {
        let mut response = status(StatusCode::METHOD_NOT_ALLOWED);
        response.headers_mut().insert(header::ALLOW, HeaderValue::from_static("POST"));
        return response;
    }
    let query: std::collections::HashMap<String, String> = url.query_pairs().map(|(k, v)| (k.into_owned(), v.into_owned())).collect();
    if query.keys().any(|name| !["w", "r", "p", "overwrite"].contains(&name.as_str())) {
        return status(StatusCode::BAD_REQUEST);
    }
    let (Some(workspace), Some(path)) = (query.get("w").filter(|w| !w.is_empty()), query.get("p").filter(|p| !p.is_empty())) else {
        return status(StatusCode::BAD_REQUEST);
    };
    let overwrite = match query.get("overwrite").map(String::as_str) {
        None | Some("0") | Some("false") => false,
        Some("1") | Some("true") => true,
        Some(_) => return status(StatusCode::BAD_REQUEST),
    };
    if activity.assert_can_start().is_err() {
        return status(StatusCode::SERVICE_UNAVAILABLE);
    }
    if let Some(len) = request.headers().get(header::CONTENT_LENGTH).and_then(|v| v.to_str().ok()).and_then(|s| s.parse::<u64>().ok()) {
        if len > MAX_UPLOAD_BYTES {
            return upload_error_response(
                StatusCode::PAYLOAD_TOO_LARGE,
                "too-large",
                &crate::locale::l("上传文件超过了 512 MiB 大小上限。", "Uploaded file exceeds the 512 MiB size limit."),
            );
        }
    }

    let (ws_owned, root_owned, path_owned) = (workspace.clone(), query.get("r").cloned(), path.clone());
    let prep_files = files.clone();
    let target = match tokio::task::spawn_blocking(move || prep_files.prepare_upload(&ws_owned, root_owned.as_deref(), &path_owned, overwrite)).await {
        Ok(Ok(target)) => target,
        Ok(Err(error)) => return upload_error_response(map_host_error(&error), &error.code, &error.message),
        Err(_) => return status(StatusCode::INTERNAL_SERVER_ERROR),
    };

    let mut cleanup = TempCleanup { path: Some(target.temp_path.clone()) };
    let Ok(mut file) = tokio::fs::OpenOptions::new().write(true).create_new(true).open(&target.temp_path).await else {
        return status(StatusCode::INTERNAL_SERVER_ERROR);
    };

    let mut stream = request.into_body().into_data_stream();
    let mut written: u64 = 0;
    while let Some(chunk) = stream.next().await {
        let Ok(bytes) = chunk else {
            drop(file);
            return status(StatusCode::BAD_REQUEST);
        };
        written = written.saturating_add(bytes.len() as u64);
        if written > MAX_UPLOAD_BYTES {
            drop(file);
            return upload_error_response(
                StatusCode::PAYLOAD_TOO_LARGE,
                "too-large",
                &crate::locale::l("上传文件超过了 512 MiB 大小上限。", "Uploaded file exceeds the 512 MiB size limit."),
            );
        }
        for slice in bytes.chunks(UPLOAD_WRITE_CHUNK_BYTES) {
            if file.write_all(slice).await.is_err() {
                drop(file);
                return status(StatusCode::INTERNAL_SERVER_ERROR);
            }
        }
    }
    if file.sync_all().await.is_err() {
        drop(file);
        return status(StatusCode::INTERNAL_SERVER_ERROR);
    }
    drop(file);

    let commit_target = target.clone();
    let committed = tokio::task::spawn_blocking(move || files.commit_upload(&commit_target, overwrite, written)).await;
    match committed {
        Ok(Ok(result)) => {
            cleanup.disarm();
            let body = serde_json::to_vec(&result).unwrap_or_default();
            let mut response = Response::new(Body::from(body));
            let headers = response.headers_mut();
            headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json; charset=utf-8"));
            headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            response
        }
        Ok(Err(error)) => upload_error_response(map_host_error(&error), &error.code, &error.message),
        Err(_) => status(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn reads_ranges_like_3x() {
        assert_eq!(byte_range(Some("bytes=0-9"), 100), Some(Ok((0, 9))));
        assert_eq!(byte_range(Some("bytes=90-"), 100), Some(Ok((90, 99))));
        assert_eq!(byte_range(Some("bytes=-10"), 100), Some(Ok((90, 99))));
        assert_eq!(byte_range(Some("bytes=95-200"), 100), Some(Ok((95, 99))));
        assert_eq!(byte_range(Some("bytes=100-"), 100), Some(Err(())));
        assert_eq!(byte_range(Some("bytes=0-1,5-6"), 100), None);
        assert_eq!(byte_range(Some("items=0-1"), 100), None);
        assert_eq!(byte_range(None, 100), None);
    }

    #[tokio::test]
    async fn upload_route_enforces_origin_traversal_and_atomic_publish() {
        let dir = tempfile::tempdir().unwrap();
        let ws_root = dir.path().join("workspace");
        std::fs::create_dir_all(&ws_root).unwrap();
        let projects = Arc::new(crate::projects::Projects::load(dir.path().join("projects.json")).unwrap());
        let project = projects.open(ws_root.to_str().unwrap(), None).unwrap();
        let files = Arc::new(Files::new(projects, |_, _, _| {}, |_| BTreeMap::new()));
        let activity = Arc::new(Activity::default());
        let server = HostServer::new(dir.path().to_path_buf(), Vec::new);

        // Cross-origin request is rejected with 403.
        let bad_origin_url = Url::parse(&format!("http://127.0.0.1:0/rainy/ide/upload?w={}&p=a.bin", project.id)).unwrap();
        let bad_origin_req = Request::builder()
            .method(Method::POST)
            .uri(bad_origin_url.as_str())
            .header(header::ORIGIN, "https://evil.example")
            .body(Body::from("evil"))
            .unwrap();
        let resp = serve_upload(files.clone(), server.clone(), activity.clone(), bad_origin_req, bad_origin_url).await;
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);

        // Path traversal is rejected with 400/403 and writes nothing outside workspace.
        let trav_url = Url::parse(&format!("http://127.0.0.1:0/rainy/ide/upload?w={}&p=../escape.txt", project.id)).unwrap();
        let trav_req = Request::builder().method(Method::POST).uri(trav_url.as_str()).body(Body::from("nope")).unwrap();
        let resp = serve_upload(files.clone(), server.clone(), activity.clone(), trav_req, trav_url).await;
        assert!(matches!(resp.status(), StatusCode::BAD_REQUEST | StatusCode::FORBIDDEN));
        assert!(!dir.path().join("escape.txt").exists());

        // Valid streamed upload succeeds and returns FileUploadResult JSON.
        let ok_url = Url::parse(&format!("http://127.0.0.1:0/rainy/ide/upload?w={}&p=weights.bin", project.id)).unwrap();
        let ok_req = Request::builder().method(Method::POST).uri(ok_url.as_str()).body(Body::from("hello-stream")).unwrap();
        let resp = serve_upload(files.clone(), server.clone(), activity.clone(), ok_req, ok_url).await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(std::fs::read(ws_root.join("weights.bin")).unwrap(), b"hello-stream");

        // Duplicate upload without overwrite=1 is rejected with 409 Conflict.
        let dup_url = Url::parse(&format!("http://127.0.0.1:0/rainy/ide/upload?w={}&p=weights.bin", project.id)).unwrap();
        let dup_req = Request::builder().method(Method::POST).uri(dup_url.as_str()).body(Body::from("overwrite-attempt")).unwrap();
        let resp = serve_upload(files.clone(), server.clone(), activity.clone(), dup_req, dup_url).await;
        assert_eq!(resp.status(), StatusCode::CONFLICT);
        assert_eq!(std::fs::read(ws_root.join("weights.bin")).unwrap(), b"hello-stream");
    }
}
