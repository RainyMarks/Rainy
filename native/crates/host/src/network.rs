//! Outbound HTTP for model services and MCP servers (3.x `network.ts`): direct, or through the proxy the user or the
//! system chose. Commands and terminals are not affected; the hosts they reach often sit on networks a proxy cannot reach.

use std::collections::HashMap;
use std::future::Future;
use std::net::IpAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use serde::Serialize;
use url::Url;

use crate::settings::ProxySettings;

/// Shown instead of a proxy password; sending it back keeps the stored password.
pub const PASSWORD_MASK: &str = "***";
/// How long the carrier's answer for one origin is reused; system proxy changes apply after at most this long.
const SYSTEM_ANSWER: Duration = Duration::from_secs(30);
const MAX_SYSTEM_ANSWERS: usize = 64;
const MAX_CLIENTS: usize = 4;
/// Stands for "any remote service" when Settings shows the system proxy.
const REFERENCE_URL: &str = "https://example.com/";

/// Asks the Windows carrier which proxy the system uses for a URL: `http://host:port`, or `None` for direct.
pub type SystemProxy = Arc<dyn Fn(String) -> Pin<Box<dyn Future<Output = anyhow::Result<Option<String>>> + Send>> + Send + Sync>;

fn private_v4(address: std::net::Ipv4Addr) -> bool {
    let [a, b, ..] = address.octets();
    a == 127 || a == 10 || a == 0 || (a == 172 && (16..=31).contains(&b)) || (a == 192 && b == 168) || (a == 169 && b == 254)
}

/// Hosts that never go through a proxy: this machine, private networks, `.local` names and single-label names (as
/// Windows' `<local>` rule), where local model servers live.
pub fn bypasses_proxy(hostname: &str) -> bool {
    let host = hostname.trim_start_matches('[').trim_end_matches(']').trim_end_matches('.').to_lowercase();
    if host == "localhost" || host.ends_with(".localhost") || host.ends_with(".local") {
        return true;
    }
    match host.parse::<IpAddr>() {
        Ok(IpAddr::V4(address)) => private_v4(address),
        Ok(IpAddr::V6(address)) => {
            if let Some(mapped) = address.to_ipv4_mapped() {
                return private_v4(mapped);
            }
            let first = address.segments()[0];
            address.is_loopback() || address.is_unspecified() || (first & 0xfe00) == 0xfc00 || (first & 0xffc0) == 0xfe80
        }
        Err(_) => !host.contains('.'),
    }
}

/// Normalize a proxy address: `host:port` or an `http(s)://` URL, optionally with `user:password@`.
pub fn proxy_url(text: Option<&str>) -> Option<String> {
    let value = text?.trim();
    if value.is_empty() {
        return None;
    }
    let has_scheme = value.find("://").is_some_and(|at| value[..at].chars().all(|c| c.is_ascii_alphanumeric() || "+.-".contains(c)));
    let url = Url::parse(&if has_scheme { value.to_owned() } else { format!("http://{value}") }).ok()?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none_or(str::is_empty) {
        return None;
    }
    let credentials = if url.username().is_empty() {
        String::new()
    } else {
        format!("{}{}@", url.username(), url.password().map(|password| format!(":{password}")).unwrap_or_default())
    };
    let port = url.port().map(|port| format!(":{port}")).unwrap_or_default();
    Some(format!("{}://{credentials}{}{port}", url.scheme(), url.host_str()?))
}

fn masked(url: &str) -> String {
    match Url::parse(url) {
        Ok(mut parsed) if parsed.password().is_some() => {
            let _ = parsed.set_password(Some(PASSWORD_MASK));
            parsed.as_str().trim_end_matches('/').replace("%2A%2A%2A", PASSWORD_MASK)
        }
        _ => url.to_owned(),
    }
}

fn no_proxy_covers(entry: &str, host: &str) -> bool {
    let lower = entry.to_lowercase();
    let name = lower.trim_start_matches('*').trim_start_matches('.');
    let name = name.rsplit_once(':').filter(|(_, port)| port.chars().all(|c| c.is_ascii_digit())).map_or(name, |(name, _)| name);
    entry == "*" || host == name || host.ends_with(&format!(".{name}"))
}

/// The proxy of `HTTPS_PROXY`/`HTTP_PROXY` for one URL, honouring `NO_PROXY`.
pub fn environment_proxy(url: &Url) -> Option<String> {
    let var = |names: [&str; 2]| names.iter().find_map(|name| std::env::var(name).ok().filter(|value| !value.is_empty()));
    let host = url.host_str().unwrap_or_default().trim_start_matches('[').trim_end_matches(']').to_lowercase();
    let no_proxy = var(["NO_PROXY", "no_proxy"]).unwrap_or_default();
    if no_proxy.split([' ', ',', '\t']).filter(|entry| !entry.is_empty()).any(|entry| no_proxy_covers(entry, &host)) {
        return None;
    }
    let https = var(["HTTPS_PROXY", "https_proxy"]);
    let http = var(["HTTP_PROXY", "http_proxy"]);
    let chosen = if url.scheme() == "https" { https.or(http) } else { http.or(https) };
    proxy_url(chosen.as_deref())
}

#[derive(Debug, Clone, Serialize)]
pub struct ProxyStatus {
    pub settings: ProxySettings,
    pub effective: Option<String>,
    pub system: Option<String>,
}

/// The proxy settings and the HTTP clients every model and MCP request uses.
pub struct Network {
    windows: bool,
    settings: RwLock<ProxySettings>,
    system: Option<SystemProxy>,
    answers: Mutex<Vec<(String, Instant, Option<String>)>>,
    clients: Mutex<HashMap<Option<String>, reqwest::Client>>,
    listeners: Mutex<Vec<Box<dyn Fn() + Send + Sync>>>,
}

impl Network {
    pub fn new(windows: bool, system: Option<SystemProxy>) -> Self {
        Self { windows, settings: RwLock::default(), system, answers: Mutex::default(), clients: Mutex::default(), listeners: Mutex::default() }
    }

    pub fn configure(&self, settings: Option<ProxySettings>) {
        *self.settings.write().unwrap() = settings.unwrap_or_default();
        for listener in self.listeners.lock().unwrap().iter() {
            listener();
        }
    }

    pub fn on_change(&self, listener: impl Fn() + Send + Sync + 'static) {
        self.listeners.lock().unwrap().push(Box::new(listener));
    }

    pub fn settings(&self) -> ProxySettings {
        self.settings.read().unwrap().clone()
    }

    /// Settings typed in Settings, where a masked password stands for the stored one.
    pub fn with_stored_password(&self, settings: ProxySettings) -> ProxySettings {
        let stored = self.settings();
        if settings.mode != "manual" || stored.mode != "manual" {
            return settings;
        }
        let (Some(typed), Some(kept)) = (proxy_url(settings.url.as_deref()), proxy_url(stored.url.as_deref())) else { return settings };
        let (Ok(next), Ok(previous)) = (Url::parse(&typed), Url::parse(&kept)) else { return settings };
        let password = next.password().unwrap_or_default();
        if password != PASSWORD_MASK && password != "%2A%2A%2A" {
            return settings;
        }
        if next.scheme() != previous.scheme() || next.host_str() != previous.host_str() || next.port() != previous.port() || next.username() != previous.username() {
            return settings;
        }
        ProxySettings { mode: "manual".into(), url: Some(kept) }
    }

    /// The system proxy for one URL: the carrier's answer on Windows (settings, bypass list and PAC), else the environment.
    pub async fn system_proxy(&self, url: &Url) -> Option<String> {
        let Some(system) = self.system.clone().filter(|_| self.windows) else { return environment_proxy(url) };
        let origin = url.origin().ascii_serialization();
        if let Some((_, _, proxy)) = self.answers.lock().unwrap().iter().find(|(known, at, _)| *known == origin && at.elapsed() < SYSTEM_ANSWER) {
            return proxy.clone();
        }
        // Without an answer the environment decides, which on Windows usually means direct.
        let proxy = match system(format!("{origin}/")).await {
            Ok(answer) => proxy_url(answer.as_deref()),
            Err(_) => environment_proxy(url),
        };
        let mut answers = self.answers.lock().unwrap();
        answers.retain(|(known, ..)| *known != origin);
        answers.push((origin, Instant::now(), proxy.clone()));
        if answers.len() > MAX_SYSTEM_ANSWERS {
            answers.remove(0);
        }
        proxy
    }

    /// The proxy a request to `url` goes through under `settings`; `None` for direct.
    pub async fn proxy_for(&self, settings: &ProxySettings, url: &Url) -> Option<String> {
        if settings.mode == "off" || bypasses_proxy(url.host_str().unwrap_or_default()) {
            return None;
        }
        if settings.mode == "manual" {
            return proxy_url(settings.url.as_deref());
        }
        self.system_proxy(url).await
    }

    /// What Settings shows; proxy passwords are masked.
    pub async fn status(&self, target: Option<&str>) -> ProxyStatus {
        let reference = Url::parse(REFERENCE_URL).expect("valid URL");
        let url = target.and_then(|target| Url::parse(target).ok()).unwrap_or_else(|| reference.clone());
        let settings = self.settings();
        let effective = self.proxy_for(&settings, &url).await;
        let system = self.system_proxy(&reference).await;
        let shown = match (settings.mode.as_str(), proxy_url(settings.url.as_deref())) {
            ("manual", Some(stored)) if Url::parse(&stored).is_ok_and(|url| url.password().is_some()) => ProxySettings { mode: "manual".into(), url: Some(masked(&stored)) },
            _ => settings,
        };
        ProxyStatus { settings: shown, effective: effective.as_deref().map(masked), system: system.as_deref().map(masked) }
    }

    /// The client for requests to `url` under the current settings.
    pub async fn client(&self, url: &Url) -> reqwest::Client {
        self.client_with(&self.settings(), url).await
    }

    /// The client for requests to `url` under `settings`, for testing a choice before it is saved.
    pub async fn client_with(&self, settings: &ProxySettings, url: &Url) -> reqwest::Client {
        let proxy = self.proxy_for(settings, url).await;
        let mut clients = self.clients.lock().unwrap();
        if let Some(client) = clients.get(&proxy) {
            return client.clone();
        }
        let mut builder = reqwest::Client::builder().no_proxy().connect_timeout(Duration::from_secs(30)).user_agent(concat!("Rainy/", env!("CARGO_PKG_VERSION")));
        if let Some(address) = &proxy {
            match reqwest::Proxy::all(address) {
                Ok(proxy) => builder = builder.proxy(proxy),
                Err(error) => log::warn!("The proxy {} is not usable: {error}", masked(address)),
            }
        }
        let client = builder.build().unwrap_or_default();
        if clients.len() >= MAX_CLIENTS {
            clients.clear();
        }
        clients.insert(proxy, client.clone());
        client
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bypasses_local_hosts() {
        for host in ["localhost", "a.localhost", "printer.local", "llm", "127.0.0.1", "10.1.2.3", "172.20.0.1", "192.168.1.5", "169.254.1.1", "[::1]", "fd00::1", "fe80::1", "::ffff:192.168.0.1"] {
            assert!(bypasses_proxy(host), "{host}");
        }
        for host in ["api.deepseek.com", "8.8.8.8", "172.32.0.1", "2001:db8::1", "::ffff:8.8.8.8"] {
            assert!(!bypasses_proxy(host), "{host}");
        }
    }

    #[test]
    fn normalizes_proxy_addresses() {
        assert_eq!(proxy_url(Some("127.0.0.1:7890")).as_deref(), Some("http://127.0.0.1:7890"));
        assert_eq!(proxy_url(Some("http://u:p@proxy:8080/")).as_deref(), Some("http://u:p@proxy:8080"));
        assert_eq!(proxy_url(Some("socks5://proxy:1080")), None);
        assert_eq!(proxy_url(Some("  ")), None);
        assert_eq!(masked("http://u:secret@proxy:8080"), "http://u:***@proxy:8080");
    }

    #[test]
    fn honours_no_proxy_entries() {
        assert!(no_proxy_covers(".example.com", "api.example.com"));
        assert!(no_proxy_covers("example.com:443", "example.com"));
        assert!(no_proxy_covers("*", "anything"));
        assert!(!no_proxy_covers("example.com", "notexample.com"));
    }
}
