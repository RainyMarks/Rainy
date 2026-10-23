//! Strata's public loopback endpoints (3.x `shared/strata-health.ts`): `/health` without triggering a model load, and
//! `/unload` before Rainy stops a server it started. Plain HTTP/1.1 on 127.0.0.1; no proxy, no credentials.

use std::io::{Read as _, Write as _};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use serde::Deserialize;

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
    let split = answer.windows(4).position(|window| window == b"\r\n\r\n").context("The Strata server sent an incomplete answer")?;
    let head = String::from_utf8_lossy(&answer[..split]).into_owned();
    let status = head.split_whitespace().nth(1).and_then(|code| code.parse::<u16>().ok()).context("The Strata server sent an invalid answer")?;
    let mut body = answer[split + 4..].to_vec();
    if head.lines().any(|line| line.to_ascii_lowercase().starts_with("transfer-encoding:") && line.to_ascii_lowercase().contains("chunked")) {
        body = dechunk(&body)?;
    }
    Ok((status, body))
}

fn dechunk(mut data: &[u8]) -> Result<Vec<u8>> {
    let mut body = Vec::new();
    loop {
        let line = data.windows(2).position(|window| window == b"\r\n").context("Invalid chunked answer")?;
        let size = usize::from_str_radix(String::from_utf8_lossy(&data[..line]).split(';').next().unwrap_or("").trim(), 16)?;
        data = &data[line + 2..];
        if size == 0 {
            return Ok(body);
        }
        if data.len() < size || body.len() + size > MAX_BODY {
            bail!("Invalid chunked answer");
        }
        body.extend_from_slice(&data[..size]);
        data = data.get(size + 2..).unwrap_or_default();
    }
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

/// Ask the server to release the model before its process is stopped.
pub fn unload(port: u16) {
    let _ = request(port, "POST", "/unload", "{}", Duration::from_secs(5));
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
