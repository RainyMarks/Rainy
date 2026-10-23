//! The loopback HTTP server of one Host (3.x `server.ts`): the launch token is exchanged once for an HttpOnly session
//! cookie, which every later request and WebSocket upgrade must carry; then routes, sockets and the built pages.

use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::{Arc, RwLock};

use axum::body::Body;
use axum::extract::ws::{WebSocket, WebSocketUpgrade};
use axum::extract::{FromRequestParts as _, Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse as _, Response};
use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use futures::future::BoxFuture;
use serde_json::Value;
use url::Url;

const COOKIE: &str = "rainy_auth";

pub type RouteHandler = Arc<dyn Fn(Request, Url) -> BoxFuture<'static, Response> + Send + Sync>;
pub type SocketHandler = Arc<dyn Fn(WebSocket, Url) -> BoxFuture<'static, ()> + Send + Sync>;

struct Route {
    path: String,
    exact: bool,
    handler: RouteHandler,
}

pub struct HostServer {
    launch_token: String,
    secret: String,
    port: AtomicU16,
    routes: RwLock<Vec<Route>>,
    sockets: RwLock<Vec<(String, SocketHandler)>>,
    renderer_root: PathBuf,
    /// Script globals injected into `index.html`.
    globals: Box<dyn Fn() -> Vec<(String, Value)> + Send + Sync>,
}

/// MIME types of the files the Host serves.
pub fn mime(extension: &str) -> &'static str {
    match extension.to_ascii_lowercase().as_str() {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "wasm" => "application/wasm",
        "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

fn random_token() -> String {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("random bytes");
    URL_SAFE_NO_PAD.encode(bytes)
}

fn same(given: &[u8], expected: &[u8]) -> bool {
    given.len() == expected.len() && given.iter().zip(expected).fold(0u8, |acc, (a, b)| acc | (a ^ b)) == 0
}

fn status(code: StatusCode) -> Response {
    let mut response = Response::new(Body::empty());
    *response.status_mut() = code;
    response
}

impl HostServer {
    pub fn new(renderer_root: PathBuf, globals: impl Fn() -> Vec<(String, Value)> + Send + Sync + 'static) -> Arc<Self> {
        Arc::new(Self {
            launch_token: random_token(),
            secret: random_token(),
            port: AtomicU16::new(0),
            routes: RwLock::default(),
            sockets: RwLock::default(),
            renderer_root,
            globals: Box::new(globals),
        })
    }

    pub fn port(&self) -> u16 {
        self.port.load(Ordering::Relaxed)
    }

    /// The address the carrier opens once to exchange the launch token for the session cookie.
    pub fn launch_url(&self) -> String {
        format!("http://127.0.0.1:{}/?token={}", self.port(), self.launch_token)
    }

    /// Add a route: an exact path, or a prefix ending in `/`. Longer paths win.
    pub fn route(&self, path: &str, handler: RouteHandler) {
        let mut routes = self.routes.write().unwrap();
        routes.push(Route { path: path.to_owned(), exact: !path.ends_with('/'), handler });
        routes.sort_by_key(|route| std::cmp::Reverse(route.path.len()));
    }

    /// Accept WebSocket upgrades on one exact path.
    pub fn socket(&self, path: &str, handler: SocketHandler) {
        self.sockets.write().unwrap().push((path.to_owned(), handler));
    }

    /// Bind `127.0.0.1:<port>` (0 picks a free one) and serve in the background.
    pub async fn listen(self: &Arc<Self>, port: u16) -> anyhow::Result<()> {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
        self.port.store(listener.local_addr()?.port(), Ordering::Relaxed);
        let router = axum::Router::new().fallback(handle).with_state(self.clone());
        tokio::spawn(async move {
            if let Err(error) = axum::serve(listener, router).await {
                log::error!("The Host server stopped: {error}");
            }
        });
        Ok(())
    }

    fn authority_ok(&self, headers: &HeaderMap) -> bool {
        let port = self.port();
        headers.get(header::HOST).and_then(|host| host.to_str().ok()).is_some_and(|host| host == format!("127.0.0.1:{port}") || host == format!("localhost:{port}"))
    }

    fn authenticated(&self, headers: &HeaderMap) -> bool {
        if !self.authority_ok(headers) {
            return false;
        }
        headers.get_all(header::COOKIE).iter().filter_map(|value| value.to_str().ok()).flat_map(|value| value.split(';')).any(|part| {
            let part = part.trim();
            part.split_once('=').is_some_and(|(name, value)| name == COOKIE && same(value.as_bytes(), self.secret.as_bytes()))
        })
    }

    fn same_origin(&self, headers: &HeaderMap) -> bool {
        let port = self.port();
        headers
            .get(header::ORIGIN)
            .and_then(|origin| origin.to_str().ok())
            .is_some_and(|origin| origin == format!("http://127.0.0.1:{port}") || origin == format!("http://localhost:{port}"))
    }

    /// Content Security Policy of a page: scripts only from the Host plus the one inline script with the globals.
    fn page_policy(&self, nonce: &str) -> String {
        let port = self.port();
        [
            "default-src 'self'".to_owned(),
            format!("script-src 'self' 'nonce-{nonce}'"),
            "style-src 'self' 'unsafe-inline'".to_owned(),
            "img-src 'self' data: blob:".to_owned(),
            "media-src 'self' blob:".to_owned(),
            "font-src 'self' data: blob:".to_owned(),
            format!("connect-src 'self' ws://127.0.0.1:{port} ws://localhost:{port}"),
            "worker-src 'self' blob:".to_owned(),
            "frame-src 'self'".to_owned(),
            "object-src 'none'".to_owned(),
            "base-uri 'none'".to_owned(),
            "form-action 'none'".to_owned(),
            "frame-ancestors 'none'".to_owned(),
        ]
        .join("; ")
    }

    async fn serve_renderer(&self, url: &Url) -> Response {
        let root = &self.renderer_root;
        let relative = if url.path() == "/" {
            "index.html".to_owned()
        } else {
            match percent_encoding::percent_decode_str(&url.path()[1..]).decode_utf8() {
                Ok(path) => path.into_owned(),
                Err(_) => return status(StatusCode::BAD_REQUEST),
            }
        };
        let Some(path) = contained(root, &relative) else { return status(StatusCode::NOT_FOUND) };
        let mut file = path;
        if !tokio::fs::metadata(&file).await.is_ok_and(|info| info.is_file()) {
            if Path::new(&relative).extension().is_some() {
                return status(StatusCode::NOT_FOUND);
            }
            file = root.join("index.html");
        }
        let Ok(bytes) = tokio::fs::read(&file).await else { return status(StatusCode::NOT_FOUND) };
        if file.file_name().is_some_and(|name| name == "index.html") {
            let html = String::from_utf8_lossy(&bytes);
            let globals: String = (self.globals)()
                .into_iter()
                .map(|(name, value)| format!("window.{name}={};", value.to_string().replace('<', "\\u003c")))
                .collect();
            let mut nonce = [0u8; 16];
            getrandom::fill(&mut nonce).expect("random bytes");
            let nonce = STANDARD.encode(nonce);
            let body = html.replacen("<head>", &format!("<head><script nonce=\"{nonce}\">{globals}</script>"), 1);
            let mut response = Response::new(Body::from(body));
            let headers = response.headers_mut();
            headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/html; charset=utf-8"));
            headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            if let Ok(policy) = HeaderValue::from_str(&self.page_policy(&nonce)) {
                headers.insert(header::CONTENT_SECURITY_POLICY, policy);
            }
            return response;
        }
        let extension = file.extension().and_then(|extension| extension.to_str()).unwrap_or_default();
        let mut response = Response::new(Body::from(bytes));
        let headers = response.headers_mut();
        headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(mime(extension)));
        headers.insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static(if relative.starts_with("assets/") { "private, max-age=31536000, immutable" } else { "no-cache" }),
        );
        response
    }
}

/// `relative` below `root`, when it names nothing outside it.
pub fn contained(root: &Path, relative: &str) -> Option<PathBuf> {
    let mut path = root.to_path_buf();
    for component in Path::new(relative).components() {
        match component {
            Component::Normal(part) => path.push(part),
            Component::CurDir => {}
            _ => return None,
        }
    }
    Some(path)
}

async fn handle(State(server): State<Arc<HostServer>>, request: Request) -> Response {
    let mut response = route(&server, request).await;
    let headers = response.headers_mut();
    headers.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    headers.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    response
}

async fn route(server: &Arc<HostServer>, request: Request) -> Response {
    let base = format!("http://127.0.0.1:{}", server.port());
    let Ok(url) = Url::parse(&base).and_then(|base| base.join(&request.uri().to_string())) else {
        return status(StatusCode::BAD_REQUEST);
    };
    let headers = request.headers();
    if url.path() == "/" && server.authority_ok(headers) {
        if let Some(token) = url.query_pairs().find(|(name, _)| name == "token").map(|(_, value)| value.into_owned()) {
            if !same(token.as_bytes(), server.launch_token.as_bytes()) {
                return status(StatusCode::FORBIDDEN);
            }
            // `next` lets a pane land on its own page; only paths on this origin are followed.
            let next = url.query_pairs().find(|(name, _)| name == "next").map(|(_, value)| value.into_owned()).unwrap_or_else(|| "/".to_owned());
            let next = if next.starts_with('/') && !next.starts_with("//") && !next.contains('\\') { next } else { "/".to_owned() };
            let mut response = status(StatusCode::FOUND);
            let headers = response.headers_mut();
            if let Ok(cookie) = HeaderValue::from_str(&format!("{COOKIE}={}; Path=/; HttpOnly; SameSite=Strict", server.secret)) {
                headers.insert(header::SET_COOKIE, cookie);
            }
            if let Ok(location) = HeaderValue::from_str(&next) {
                headers.insert(header::LOCATION, location);
            }
            headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            return response;
        }
    }
    if !server.authenticated(headers) {
        return status(StatusCode::FORBIDDEN);
    }
    let upgrade = headers.get(header::UPGRADE).and_then(|value| value.to_str().ok()).is_some_and(|value| value.eq_ignore_ascii_case("websocket"));
    if upgrade {
        let handler = server.sockets.read().unwrap().iter().find(|(path, _)| path == url.path()).map(|(_, handler)| handler.clone());
        let Some(handler) = handler.filter(|_| server.same_origin(headers)) else { return status(StatusCode::FORBIDDEN) };
        let (mut parts, _) = request.into_parts();
        let Ok(upgrade) = WebSocketUpgrade::from_request_parts(&mut parts, &()).await else { return status(StatusCode::BAD_REQUEST) };
        return upgrade
            .max_message_size(64 * 1024 * 1024)
            .max_frame_size(64 * 1024 * 1024)
            .on_upgrade(move |socket| async move { handler(socket, url).await })
            .into_response();
    }
    let handler = server
        .routes
        .read()
        .unwrap()
        .iter()
        .find(|route| if route.exact { url.path() == route.path } else { url.path().starts_with(&route.path) })
        .map(|route| route.handler.clone());
    if let Some(handler) = handler {
        return handler(request, url).await;
    }
    if request.method() != Method::GET && request.method() != Method::HEAD {
        return status(StatusCode::METHOD_NOT_ALLOWED);
    }
    server.serve_renderer(&url).await
}

/// A JSON response that is never cached.
pub fn json_response(code: StatusCode, value: &Value) -> Response {
    let mut response = Response::new(Body::from(value.to_string()));
    *response.status_mut() = code;
    response.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json; charset=utf-8"));
    response.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

pub fn empty(code: StatusCode) -> Response {
    status(code)
}
