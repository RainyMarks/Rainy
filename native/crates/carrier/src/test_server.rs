//! A tiny HTTP/1.1 server on 127.0.0.1 for download tests: each connection gets one answer from a route, then closes.

use std::io::{BufRead as _, BufReader, Write as _};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

/// A request as the server saw it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seen {
    /// Path and query.
    pub path: String,
    /// The `Range` header, if any.
    pub range: Option<String>,
    /// The server's port, for routes that answer with absolute addresses.
    pub port: u16,
}

/// One answer: status, headers and body, sent at once or in paced pieces, optionally pausing part-way.
pub struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    pace: Option<(usize, Duration)>,
    stall: Option<(usize, Duration)>,
}

impl Reply {
    pub fn status(status: u16) -> Self {
        Self { status, headers: Vec::new(), body: Vec::new(), pace: None, stall: None }
    }

    pub fn ok(body: Vec<u8>) -> Self {
        Self { body, ..Self::status(200) }
    }

    pub fn partial(body: Vec<u8>, content_range: &str) -> Self {
        Self { body, headers: vec![("Content-Range".into(), content_range.into())], ..Self::status(206) }
    }

    pub fn redirect(status: u16, location: &str) -> Self {
        Self { headers: vec![("Location".into(), location.into())], ..Self::status(status) }
    }

    /// The whole file, or the part a `Range: bytes=N-` asks for.
    pub fn ranged(seen: &Seen, file: &[u8]) -> Self {
        let start = seen.range.as_deref().and_then(|range| range.strip_prefix("bytes=")?.strip_suffix('-')?.parse::<usize>().ok());
        match start {
            Some(start) if start < file.len() => Self::partial(file[start..].to_vec(), &format!("bytes {start}-{}/{}", file.len() - 1, file.len())),
            _ => Self::ok(file.to_vec()),
        }
    }

    /// Send the body in pieces of `size` bytes, `delay` apart.
    pub fn paced(mut self, size: usize, delay: Duration) -> Self {
        self.pace = Some((size, delay));
        self
    }

    /// Pause for `delay` after the first `bytes` of the body.
    pub fn stall_after(mut self, bytes: usize, delay: Duration) -> Self {
        self.stall = Some((bytes, delay));
        self
    }

    fn send(&self, stream: &mut TcpStream) -> std::io::Result<()> {
        let mut head = format!("HTTP/1.1 {} X\r\nContent-Length: {}\r\nConnection: close\r\n", self.status, self.body.len());
        for (name, value) in &self.headers {
            head.push_str(&format!("{name}: {value}\r\n"));
        }
        head.push_str("\r\n");
        stream.write_all(head.as_bytes())?;
        let mut sent = 0;
        while sent < self.body.len() {
            let mut end = self.pace.map_or(self.body.len(), |(size, _)| (sent + size).min(self.body.len()));
            if let Some((at, _)) = self.stall
                && sent < at
            {
                end = end.min(at);
            }
            stream.write_all(&self.body[sent..end])?;
            stream.flush()?;
            sent = end;
            if let Some((at, delay)) = self.stall
                && sent == at
            {
                thread::sleep(delay);
            }
            if let Some((_, delay)) = self.pace {
                thread::sleep(delay);
            }
        }
        Ok(())
    }
}

type Route = dyn Fn(&Seen) -> Reply + Send + Sync;

pub struct Server {
    port: u16,
    seen: Arc<Mutex<Vec<Seen>>>,
    stop: Arc<AtomicBool>,
}

impl Server {
    /// Serve `route` until the server is dropped.
    pub fn start(route: impl Fn(&Seen) -> Reply + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let (seen, stop) = (Arc::new(Mutex::new(Vec::new())), Arc::new(AtomicBool::new(false)));
        let route: Arc<Route> = Arc::new(route);
        let (log, stopping) = (seen.clone(), stop.clone());
        thread::spawn(move || {
            while !stopping.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let (route, log) = (route.clone(), log.clone());
                        thread::spawn(move || answer(stream, port, &*route, &log));
                    }
                    Err(_) => thread::sleep(Duration::from_millis(5)),
                }
            }
        });
        Self { port, seen, stop }
    }

    pub fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }

    /// The requests so far, in the order they arrived.
    pub fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn answer(stream: TcpStream, port: u16, route: &Route, log: &Mutex<Vec<Seen>>) {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    if reader.read_line(&mut line).unwrap_or(0) == 0 {
        return;
    }
    let path = line.split_whitespace().nth(1).unwrap_or("/").to_owned();
    let mut range = None;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).unwrap_or(0) == 0 || header.trim().is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':')
            && name.trim().eq_ignore_ascii_case("range")
        {
            range = Some(value.trim().to_owned());
        }
    }
    let seen = Seen { path, range, port };
    log.lock().unwrap().push(seen.clone());
    let mut stream = reader.into_inner();
    let _ = route(&seen).send(&mut stream);
    let _ = stream.shutdown(std::net::Shutdown::Both);
}
