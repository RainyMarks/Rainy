//! Strata's public `/health` on the loopback port (3.x `shared/strata-health.ts`), which reads the server's state
//! without triggering a model load. Plain HTTP/1.1 on 127.0.0.1; no proxy, no credentials.

use std::io::{Read as _, Write as _};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use serde::Deserialize;

use crate::http;
use crate::locale::localized;

const TIMEOUT: Duration = Duration::from_secs(3);
const MAX_BODY: usize = 65536;

/// What `/health` reports; it carries no credential, prompt or answer.
#[derive(Debug, Clone, PartialEq)]
pub struct Health {
    /// `http://127.0.0.1:<port>/v1`.
    pub base_url: String,
    pub model: String,
    pub context_window: u64,
    pub loaded: bool,
    pub authentication_required: bool,
}

#[derive(Deserialize)]
struct Answer {
    status: String,
    service: String,
    model: String,
    max_context: u64,
    loaded: bool,
    api_key: bool,
}

/// One request to the server on `port`; the status and body of its answer.
fn request(port: u16, method: &str, path: &str, body: &str, timeout: Duration) -> Result<(u16, Vec<u8>)> {
    let address = SocketAddr::from(([127, 0, 0, 1], port));
    let mut stream = TcpStream::connect_timeout(&address, timeout)?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    let head = format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\nAccept: application/json\r\n");
    let head = if body.is_empty() { format!("{head}\r\n") } else { format!("{head}Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len()) };
    stream.write_all(head.as_bytes())?;
    let mut answer = Vec::new();
    stream.take((MAX_BODY + 8192) as u64).read_to_end(&mut answer)?;
    let response = http::parse(&answer, || localized("这个端口上的服务返回了无效的 HTTP 回答。", "The service on this port sent an invalid HTTP answer."))?;
    Ok((response.status, response.body))
}

/// The server's public state, also when it is not loaded or needs a key.
pub fn read(port: u16) -> Result<Health> {
    let (status, body) = request(port, "GET", "/health", "", TIMEOUT)?;
    if status != 200 {
        bail!(localized(&format!("Strata 健康检查失败（HTTP {status}）。"), &format!("The Strata status check failed (HTTP {status}).")));
    }
    let answer: Answer = serde_json::from_slice(&body).map_err(|_| anyhow!(localized("Strata 健康检查的回答无效。", "The Strata status check returned an invalid answer.")))?;
    if answer.status != "ok" || answer.service != "strata" || answer.model.is_empty() || answer.max_context == 0 {
        bail!(localized("这个端口上的服务不是 Strata。", "The service on this port is not Strata."));
    }
    Ok(Health { base_url: format!("http://127.0.0.1:{port}/v1"), model: answer.model, context_window: answer.max_context, loaded: answer.loaded, authentication_required: answer.api_key })
}

#[cfg(test)]
mod tests {
    use ::core::prelude::v1::test;

    use super::*;

    #[test]
    fn reads_health_from_a_local_server() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            for body in [r#"{"status":"ok","service":"strata","model":"qwen","max_context":32768,"loaded":true,"api_key":false}"#, r#"{"status":"ok","service":"other"}"#] {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0u8; 1024];
                let _ = stream.read(&mut request);
                let chunked = format!("{:x}\r\n{body}\r\n0\r\n\r\n", body.len());
                let _ = stream.write_all(format!("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n{chunked}").as_bytes());
            }
        });
        let health = read(port).unwrap();
        assert_eq!((health.model.as_str(), health.context_window, health.loaded), ("qwen", 32768, true));
        assert_eq!(health.base_url, format!("http://127.0.0.1:{port}/v1"));
        assert!(read(port).is_err());
        server.join().unwrap();
    }
}
