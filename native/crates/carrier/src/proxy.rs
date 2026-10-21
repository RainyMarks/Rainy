//! The Windows system proxy for one address, as the Windows Host asks for it (`src/main/system-proxy.ts`).
//!
//! Reads the current user's proxy settings (manual proxy, bypass list including `<local>`, automatic detection and PAC
//! scripts) through WinHTTP. Nothing here ever changes a setting.

/// The proxy for `url` from a manual setting such as `127.0.0.1:7890` or `http=a:1;https=b:2`, honoring the bypass list.
/// Returns `http(s)://host:port`, or `None` for direct. SOCKS proxies count as none, as the Host cannot use them.
pub fn manual_proxy(setting: &str, bypass: &str, url: &str) -> Option<String> {
    let (scheme, host) = split_url(url)?;
    if bypassed(bypass, &host) {
        return None;
    }
    let mut fallback = None;
    for part in setting.split([';', ' ']).map(str::trim).filter(|part| !part.is_empty()) {
        match part.split_once('=') {
            Some((kind, address)) => {
                let kind = kind.to_ascii_lowercase();
                if kind == scheme {
                    return proxy_address(address);
                }
                if kind == "http" && fallback.is_none() {
                    fallback = proxy_address(address);
                }
            }
            None => return proxy_address(part),
        }
    }
    // Chromium uses the HTTP proxy for HTTPS when only an HTTP proxy is set per scheme.
    if scheme == "https" { fallback } else { None }
}

/// The first HTTP proxy in a WinHTTP or PAC answer such as `PROXY 127.0.0.1:7890; DIRECT` or `127.0.0.1:7890`.
pub fn proxy_from_answer(answer: &str) -> Option<String> {
    for rule in answer.split(';').map(str::trim).filter(|rule| !rule.is_empty()) {
        let mut words = rule.split_whitespace();
        let first = words.next().unwrap_or_default();
        let (kind, address) = match words.next() {
            Some(address) => (first.to_ascii_uppercase(), address),
            None => ("PROXY".to_owned(), first),
        };
        match kind.as_str() {
            "DIRECT" => return None,
            "PROXY" | "HTTP" => return proxy_address(address),
            "HTTPS" => return proxy_address(address).map(|p| p.replacen("http://", "https://", 1)),
            _ => {}
        }
    }
    None
}

fn proxy_address(address: &str) -> Option<String> {
    let address = address.trim().trim_start_matches("http://").trim_end_matches('/');
    let (host, port) = address.rsplit_once(':')?;
    let host_ok = !host.is_empty()
        && (host.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-' || b == b'_')
            || (host.starts_with('[') && host.ends_with(']')));
    let port_ok = !port.is_empty() && port.len() <= 5 && port.bytes().all(|b| b.is_ascii_digit());
    (host_ok && port_ok).then(|| format!("http://{host}:{port}"))
}

fn split_url(url: &str) -> Option<(String, String)> {
    let (scheme, rest) = url.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    let authority = authority.rsplit_once('@').map_or(authority, |(_, host)| host);
    let host = if authority.starts_with('[') {
        authority.split_inclusive(']').next()?.to_owned()
    } else {
        authority.split(':').next()?.to_owned()
    };
    Some((scheme.to_ascii_lowercase(), host.to_ascii_lowercase()))
}

/// Whether `host` matches the Internet Options bypass list (`;`-separated, `*` wildcards, `<local>` for dotless names).
fn bypassed(list: &str, host: &str) -> bool {
    list.split([';', ' ', ',']).map(str::trim).filter(|entry| !entry.is_empty()).any(|entry| {
        let entry = entry.to_ascii_lowercase();
        if entry == "<local>" {
            return !host.contains('.') && !host.contains(':');
        }
        let entry = entry.split_once("://").map_or(entry.as_str(), |(_, rest)| rest);
        wildcard(entry, host)
    })
}

fn wildcard(pattern: &str, text: &str) -> bool {
    let (p, t): (Vec<char>, Vec<char>) = (pattern.chars().collect(), text.chars().collect());
    let (mut pi, mut ti, mut star, mut mark) = (0, 0, None, 0);
    while ti < t.len() {
        if pi < p.len() && p[pi] == t[ti] {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|c| *c == '*')
}

/// The system proxy for `url`, or `None` for direct.
#[cfg(windows)]
pub fn system_proxy(url: &str) -> Option<String> {
    use windows_sys::Win32::Foundation::GlobalFree;
    use windows_sys::Win32::Networking::WinHttp::*;

    unsafe fn take(ptr: *mut u16) -> Option<String> {
        if ptr.is_null() {
            return None;
        }
        // SAFETY: WinHTTP returns NUL-terminated strings it allocated with GlobalAlloc.
        let text = unsafe {
            let len = (0..).take_while(|&i| *ptr.add(i) != 0).count();
            String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len))
        };
        unsafe { GlobalFree(ptr.cast()) };
        Some(text)
    }
    let wide = |text: &str| text.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();

    // SAFETY: plain WinHTTP calls; every string WinHTTP returns is freed by `take`.
    unsafe {
        let mut config: WINHTTP_CURRENT_USER_IE_PROXY_CONFIG = std::mem::zeroed();
        if WinHttpGetIEProxyConfigForCurrentUser(&mut config) == 0 {
            return None;
        }
        let auto_url = take(config.lpszAutoConfigUrl);
        let manual = take(config.lpszProxy);
        let bypass = take(config.lpszProxyBypass).unwrap_or_default();
        if config.fAutoDetect != 0 || auto_url.is_some() {
            let agent = wide("Rainy");
            let session = WinHttpOpen(agent.as_ptr(), WINHTTP_ACCESS_TYPE_NO_PROXY, std::ptr::null(), std::ptr::null(), 0);
            if !session.is_null() {
                let auto_wide = auto_url.as_deref().map(wide);
                let mut options: WINHTTP_AUTOPROXY_OPTIONS = std::mem::zeroed();
                options.fAutoLogonIfChallenged = 1;
                if let Some(auto_wide) = &auto_wide {
                    options.dwFlags = WINHTTP_AUTOPROXY_CONFIG_URL;
                    options.lpszAutoConfigUrl = auto_wide.as_ptr();
                } else {
                    options.dwFlags = WINHTTP_AUTOPROXY_AUTO_DETECT;
                    options.dwAutoDetectFlags = WINHTTP_AUTO_DETECT_TYPE_DHCP | WINHTTP_AUTO_DETECT_TYPE_DNS_A;
                }
                let target = wide(url);
                let mut info: WINHTTP_PROXY_INFO = std::mem::zeroed();
                let resolved = WinHttpGetProxyForUrl(session, target.as_ptr(), &mut options, &mut info) != 0;
                WinHttpCloseHandle(session);
                if resolved {
                    let answer = take(info.lpszProxy);
                    let _ = take(info.lpszProxyBypass);
                    if info.dwAccessType == WINHTTP_ACCESS_TYPE_NO_PROXY {
                        return None;
                    }
                    if let Some(answer) = answer {
                        return proxy_from_answer(&answer);
                    }
                }
            }
        }
        manual.and_then(|manual| manual_proxy(&manual, &bypass, url))
    }
}

#[cfg(not(windows))]
pub fn system_proxy(_url: &str) -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_manual_settings() {
        assert_eq!(manual_proxy("127.0.0.1:7890", "", "https://api.deepseek.com/v1"), Some("http://127.0.0.1:7890".into()));
        assert_eq!(manual_proxy("http=a:1;https=b:2", "", "https://x.com"), Some("http://b:2".into()));
        assert_eq!(manual_proxy("http=a:1", "", "https://x.com"), Some("http://a:1".into()));
        assert_eq!(manual_proxy("socks=s:1", "", "https://x.com"), None);
    }

    #[test]
    fn honors_the_bypass_list() {
        let bypass = "localhost;127.*;*.corp.example;<local>";
        assert_eq!(manual_proxy("p:1", bypass, "http://localhost:3000/"), None);
        assert_eq!(manual_proxy("p:1", bypass, "http://127.0.0.1/"), None);
        assert_eq!(manual_proxy("p:1", bypass, "https://git.corp.example/x"), None);
        assert_eq!(manual_proxy("p:1", bypass, "http://intranet/"), None);
        assert_eq!(manual_proxy("p:1", bypass, "https://api.openai.com/"), Some("http://p:1".into()));
    }

    #[test]
    fn reads_pac_answers() {
        assert_eq!(proxy_from_answer("PROXY 127.0.0.1:7890; DIRECT"), Some("http://127.0.0.1:7890".into()));
        assert_eq!(proxy_from_answer("DIRECT"), None);
        assert_eq!(proxy_from_answer("SOCKS5 a:1; HTTPS b:2"), Some("https://b:2".into()));
        assert_eq!(proxy_from_answer("127.0.0.1:7890"), Some("http://127.0.0.1:7890".into()));
    }
}
