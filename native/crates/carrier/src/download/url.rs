//! URL parsing, redirect joining, and HTTP Range spans for release downloads.

use super::{DownloadError, Options};

/// The bytes a request asks for: from `start` to `end`, inclusive, or to the end of the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Span {
    pub start: u64,
    pub end: Option<u64>,
}

impl Span {
    pub const WHOLE: Self = Self {
        start: 0,
        end: None,
    };

    pub fn from(start: u64) -> Self {
        Self { start, end: None }
    }

    /// The `Range` header's value; none for the whole file.
    pub fn header(self) -> Option<String> {
        match self.end {
            Some(end) => Some(format!("bytes={}-{end}", self.start)),
            None if self.start > 0 => Some(format!("bytes={}-", self.start)),
            None => None,
        }
    }
}

/// A parsed download address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Address {
    pub secure: bool,
    /// Lowercase; IPv6 addresses keep their brackets.
    pub host: String,
    pub port: u16,
    /// Path and query, starting with `/`.
    pub path: String,
}

impl Address {
    /// Accept `https://`, and `http://` only for a loopback host when the caller allows it; no user names or passwords.
    pub fn parse(text: &str, options: &Options) -> Result<Self, DownloadError> {
        let insecure = || DownloadError::Insecure(text.to_owned());
        let (scheme, rest) = text.split_once("://").ok_or_else(insecure)?;
        let secure = match scheme.to_ascii_lowercase().as_str() {
            "https" => true,
            "http" => false,
            _ => return Err(insecure()),
        };
        let rest = rest.split('#').next().unwrap_or_default();
        let (authority, path) = rest.split_at(rest.find(['/', '?']).unwrap_or(rest.len()));
        if authority.is_empty() || authority.contains('@') || authority.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return Err(insecure());
        }
        let (host, port) = match authority.strip_prefix('[') {
            Some(inner) => {
                let (address, after) = inner.split_once(']').ok_or_else(insecure)?;
                if !after.is_empty() && !after.starts_with(':') {
                    return Err(insecure());
                }
                (format!("[{address}]"), after.strip_prefix(':'))
            }
            None => match authority.rsplit_once(':') {
                Some((host, port)) => (host.to_owned(), Some(port)),
                None => (authority.to_owned(), None),
            },
        };
        let port = match port {
            Some(digits) => digits.parse::<u16>().ok().filter(|port| *port > 0).ok_or_else(insecure)?,
            None if secure => 443,
            None => 80,
        };
        let host = host.to_ascii_lowercase();
        if host.is_empty() || (!secure && !(options.allow_loopback_http && is_loopback(&host))) {
            return Err(insecure());
        }
        let path = if path.starts_with('/') { path.to_owned() } else { format!("/{path}") };
        if path.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return Err(insecure());
        }
        Ok(Self { secure, host, port, path })
    }

    pub fn origin(&self) -> String {
        let (scheme, default) = if self.secure { ("https", 443) } else { ("http", 80) };
        if self.port == default { format!("{scheme}://{}", self.host) } else { format!("{scheme}://{}:{}", self.host, self.port) }
    }

    pub fn url(&self) -> String {
        format!("{}{}", self.origin(), self.path)
    }

    /// A redirect's `Location`, which may be relative to this address.
    pub fn join(&self, location: &str) -> String {
        let scheme_end = location.find("://");
        if scheme_end.is_some_and(|end| !location[..end].contains(['/', '?'])) {
            return location.to_owned();
        }
        if location.starts_with("//") {
            return format!("{}:{location}", if self.secure { "https" } else { "http" });
        }
        if location.starts_with('/') {
            return format!("{}{location}", self.origin());
        }
        let path = self.path.split('?').next().unwrap_or("/");
        if location.starts_with('?') {
            return format!("{}{path}{location}", self.origin());
        }
        let folder = &path[..path.rfind('/').map_or(0, |slash| slash + 1)];
        format!("{}{folder}{location}", self.origin())
    }
}

pub(super) fn is_loopback(host: &str) -> bool {
    matches!(host, "127.0.0.1" | "localhost" | "[::1]")
}
