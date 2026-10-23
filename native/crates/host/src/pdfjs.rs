//! pdf.js for the PDF viewer, served from `resources/pdfjs` under `/rainy/pdfjs/` (3.x `host/pdfjs.ts`): the two
//! browser files, the character maps for CJK fonts a PDF does not embed, and the standard fonts that stand in for the 14
//! base PDF fonts.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::http::{HeaderValue, Method, StatusCode, header};

use crate::server::{self, HostServer};

const PREFIX: &str = "/rainy/pdfjs/";
/// Browser files; anything else under the route must be one of the data files `data_file` accepts.
const SCRIPTS: [&str; 2] = ["pdf.min.js", "pdf.worker.min.js"];

/// A binary character map (`cmaps/UniGB-UCS2-H.bcmap`) or font program (`standard_fonts/FoxitSans.pfb`); one plain name each.
fn data_file(name: &str) -> bool {
    let plain = |stem: &str| !stem.is_empty() && stem.chars().all(|char| char.is_ascii_alphanumeric() || char == '-');
    let ends = |rest: &str, extensions: &[&str]| extensions.iter().any(|extension| rest.strip_suffix(extension).is_some_and(plain));
    match name.split_once('/') {
        Some(("cmaps", rest)) => ends(rest, &[".bcmap"]),
        Some(("standard_fonts", rest)) => ends(rest, &[".pfb", ".ttf"]),
        _ => false,
    }
}

/// Register `/rainy/pdfjs/` over `<resources>/pdfjs`.
pub fn install(server: &HostServer, resources: &Path) {
    let root: PathBuf = resources.join("pdfjs");
    server.route(
        PREFIX,
        Arc::new(move |request, url| {
            let root = root.clone();
            Box::pin(async move {
                let name = url.path().strip_prefix(PREFIX).unwrap_or_default().to_owned();
                let script = SCRIPTS.contains(&name.as_str());
                let head = request.method() == Method::HEAD;
                if (request.method() != Method::GET && !head) || (!script && !data_file(&name)) {
                    return server::empty(StatusCode::NOT_FOUND);
                }
                let Ok(bytes) = tokio::fs::read(root.join(&name)).await else { return server::empty(StatusCode::NOT_FOUND) };
                let length = bytes.len();
                let mut response = axum::response::Response::new(if head { axum::body::Body::empty() } else { axum::body::Body::from(bytes) });
                let headers = response.headers_mut();
                headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(if script { "text/javascript; charset=utf-8" } else { "application/octet-stream" }));
                headers.insert(header::CONTENT_LENGTH, HeaderValue::from(length));
                // The files change only with the application, which reloads the page.
                headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("private, max-age=86400"));
                headers.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
                response
            })
        }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serves_only_pdfjs_data_names() {
        assert!(data_file("cmaps/UniGB-UCS2-H.bcmap"));
        assert!(data_file("standard_fonts/FoxitSans.pfb"));
        assert!(data_file("standard_fonts/LiberationSans-Bold.ttf"));
        for name in ["cmaps/../pdf.min.js", "cmaps/a/b.bcmap", "standard_fonts/.pfb", "LICENSE", "cmaps/x.pfb", "../icon.png"] {
            assert!(!data_file(name), "{name}");
        }
    }
}
