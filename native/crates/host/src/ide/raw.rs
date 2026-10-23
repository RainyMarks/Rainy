//! `/rainy/ide/raw`: the bytes of one project file for the viewers, with single ranges for media seeking and hex paging
//! (3.x `ide/raw.ts`). Served files stay inert: a page navigated to one runs no script and gets no Host origin.

use std::io::{Read as _, Seek as _};
use std::sync::Arc;

use axum::body::Body;
use axum::extract::Request;
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::response::Response;
use url::Url;

use super::files::Files;
use super::preview::media_type;
use crate::activity::Activity;

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

#[cfg(test)]
mod tests {
    use super::*;

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
}
