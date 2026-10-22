//! The agent's preview window (`src/main/preview-browser.ts`): a WebView2 control the user does not see, in which the
//! agent opens the project's own server on this machine to read, use and photograph what it built.
//!
//! Only loopback addresses are opened, so the page never reaches a site the user is signed in to; the control keeps
//! nothing on disk and shares no cookies with the viewers.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::*;
use rainy_carrier::locale::localized;
use serde_json::{Value, json};

use super::{Options, WebView};

/// The page renders at this size; a page that reacts to width sees a desktop.
const WIDTH: i32 = 1280;
const HEIGHT: i32 = 800;
/// Longest a navigation may take before the command answers with what it has.
const LOAD_TIMEOUT: Duration = Duration::from_secs(20);
/// Console messages and failed requests kept per page.
const EVENTS_KEPT: usize = 40;
/// Characters of page text one read returns.
const TEXT_LIMIT: usize = 8000;

/// Whether `url` is an http(s) address on this machine.
pub fn is_loopback_url(url: &str) -> bool {
    let Some((scheme, rest)) = url.split_once("://") else { return false };
    if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
        return false;
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host = authority.rsplit_once('@').map_or(authority, |(_, host)| host);
    let host = match host.strip_prefix('[') {
        Some(inner) => inner.split(']').next().unwrap_or_default(),
        None => host.split(':').next().unwrap_or_default(),
    }
    .to_ascii_lowercase();
    host == "localhost" || host == "127.0.0.1" || host == "::1" || host.ends_with(".localhost")
}

/// Reads the page the way a text-only model can act on: its text and the elements it can click or type into.
fn read_script() -> String {
    format!(
        r#"(() => {{
  const visible = (element) => {{
    const rect = element.getBoundingClientRect()
    if (rect.width < 1 || rect.height < 1) return false
    const style = getComputedStyle(element)
    return style.visibility !== 'hidden' && style.display !== 'none' && Number(style.opacity) !== 0
  }}
  const label = (element) => (
    element.getAttribute('aria-label') || element.title || element.placeholder || element.value
    || (element.innerText || element.textContent || '').trim()
  ).replace(/\s+/g, ' ').slice(0, 80)
  const role = (element) => {{
    const explicit = element.getAttribute('role')
    if (explicit) return explicit
    const tag = element.tagName.toLowerCase()
    if (tag === 'a') return 'link'
    if (tag === 'button') return 'button'
    if (tag === 'select') return 'select'
    if (tag === 'textarea') return 'textbox'
    if (tag === 'input') return (element.type === 'checkbox' || element.type === 'radio' || element.type === 'submit' || element.type === 'button') ? element.type : 'textbox'
    return 'control'
  }}
  const nodes = [...document.querySelectorAll('a[href], button, input, select, textarea, [role="button"], [role="link"], [role="tab"], [role="checkbox"], [onclick]')]
  const elements = []
  for (const node of nodes) {{
    if (!visible(node)) continue
    const name = label(node)
    const line = role(node) + (name ? ' "' + name + '"' : '') + (node.disabled ? ' [disabled]' : '')
    if (!elements.includes(line)) elements.push(line)
    if (elements.length >= 60) break
  }}
  return {{ url: location.href, title: document.title, text: (document.body ? document.body.innerText : '').replace(/\n{{3,}}/g, '\n\n').trim().slice(0, {TEXT_LIMIT}), elements }}
}})()"#
    )
}

/// Finds an element by visible text, falling back to a CSS selector, and acts on it in the page.
fn act_script(action: &str, target: &str, value: &str) -> String {
    let (wanted, text, action) = (json!(target), json!(value), json!(action));
    format!(
        r#"(() => {{
    const wanted = {wanted}
    const lower = wanted.toLowerCase()
    const named = (element) => ((element.getAttribute('aria-label') || element.title || element.placeholder || element.name || element.innerText || element.textContent || '').trim().toLowerCase())
    const all = [...document.querySelectorAll('a[href], button, input, select, textarea, [role="button"], [role="link"], [role="tab"], [role="checkbox"], [onclick], label')]
    let found = all.find(element => named(element) === lower) ?? all.find(element => named(element).includes(lower))
    if (!found) {{ try {{ found = document.querySelector(wanted) ?? undefined }} catch (_invalidSelector) {{ found = undefined }} }}
    if (!found) return {{ error: 'no element matches ' + wanted }}
    if (found.tagName === 'LABEL' && found.control) found = found.control
    found.scrollIntoView({{ block: 'center' }})
    if ({action} === 'click') {{ found.focus?.(); found.click() }}
    else if ({action} === 'type') {{
      found.focus?.()
      const setter = Object.getOwnPropertyDescriptor(found.constructor.prototype, 'value')?.set
      if (setter) setter.call(found, {text}); else found.value = {text}
      found.dispatchEvent(new Event('input', {{ bubbles: true }}))
      found.dispatchEvent(new Event('change', {{ bubbles: true }}))
    }} else {{
      found.focus?.()
      for (const type of ['keydown', 'keypress', 'keyup']) {{
        found.dispatchEvent(new KeyboardEvent(type, {{ key: {text}, bubbles: true, cancelable: true }}))
      }}
      if ({text} === 'Enter' && found.form) found.form.requestSubmit?.()
    }}
    return {{ ok: true }}
  }})()"#
    )
}

/// The control and what its page reported.
#[derive(Default)]
struct Preview {
    view: Option<WebView>,
    console: Rc<RefCell<VecDeque<String>>>,
    failed: Rc<RefCell<VecDeque<String>>>,
    /// Signals the end of the current navigation.
    loaded: Rc<RefCell<Option<async_channel::Sender<()>>>>,
}

thread_local! {
    static PREVIEW: RefCell<Preview> = RefCell::default();
}

fn push(list: &RefCell<VecDeque<String>>, line: String) {
    let mut list = list.borrow_mut();
    list.push_back(line);
    while list.len() > EVENTS_KEPT {
        list.pop_front();
    }
}

/// WebView2 asks every site for its icon by itself; a missing one is not the page's fault and is not reported.
fn site_icon(url: &str) -> bool {
    url.split(['?', '#']).next().is_some_and(|address| address.ends_with("/favicon.ico"))
}

fn error(text: String) -> Value {
    json!({ "error": text })
}

/// Close the control; a later command opens a new one.
pub fn close() {
    PREVIEW.with(|preview| {
        let mut preview = preview.borrow_mut();
        if let Some(view) = preview.view.take() {
            view.close();
        }
        preview.console.borrow_mut().clear();
        preview.failed.borrow_mut().clear();
    });
}

/// Await one callback-style call.
async fn call<T: 'static>(start: impl FnOnce(Box<dyn FnOnce(T)>)) -> Option<T> {
    let (tx, rx) = async_channel::bounded(1);
    start(Box::new(move |value| drop(tx.try_send(value))));
    rx.recv().await.ok()
}

fn current() -> Option<WebView> {
    PREVIEW.with(|preview| preview.borrow().view.clone())
}

/// Carry out one command from the Host; the answer is what the page reports, or an error for the model.
pub async fn handle(command: Value, window: AnyWindowHandle, cx: &mut AsyncApp) -> Value {
    let action = command.get("action").and_then(Value::as_str).unwrap_or_default().to_owned();
    let text = |name: &str| command.get(name).and_then(Value::as_str).unwrap_or_default().to_owned();
    let settle = command.get("settleMs").and_then(Value::as_u64);
    match action.as_str() {
        "close" => {
            close();
            json!({})
        }
        "open" => {
            let url = text("url");
            if !is_loopback_url(&url) {
                return error(localized(
                    "预览只能打开本机地址（localhost 或 127.0.0.1）上的项目自己的服务，其他网址请用 web_fetch。",
                    "The preview only opens the project's own server on this machine (localhost or 127.0.0.1); use web_fetch for other addresses.",
                ));
            }
            if let Err(message) = open(&url, window, cx).await {
                return error(message);
            }
            report(settle.unwrap_or(400), cx).await
        }
        _ if current().is_none() => error(localized("预览还没有打开任何页面，请先用 browser_open 打开。", "The preview has no page open yet; open one with browser_open first.")),
        "read" => report(0, cx).await,
        "screenshot" => {
            let view = current().expect("checked above");
            let shot = call(|done| view.devtools("Page.captureScreenshot", r#"{"format":"png"}"#, done)).await;
            let mut answer = report(0, cx).await;
            match shot {
                Some(Ok(json)) => {
                    let data = serde_json::from_str::<Value>(&json).ok().and_then(|value| value.get("data").and_then(Value::as_str).map(str::to_owned));
                    answer["image"] = json!(data);
                }
                Some(Err(message)) => answer["error"] = json!(message),
                None => answer["error"] = json!("the preview closed"),
            }
            answer
        }
        "click" | "type" | "press" => {
            let view = current().expect("checked above");
            let value = if action == "press" { command.get("key").and_then(Value::as_str).unwrap_or("Enter").to_owned() } else { text("text") };
            let script = act_script(&action, &text("target"), &value);
            let result = call(|done| view.execute(&script, done)).await;
            let failure = match result {
                Some(Ok(json)) => serde_json::from_str::<Value>(&json).ok().and_then(|value| value.get("error").and_then(Value::as_str).map(str::to_owned)),
                Some(Err(message)) => Some(message),
                None => Some("the preview closed".to_owned()),
            };
            match failure {
                Some(message) => {
                    let mut answer = report(0, cx).await;
                    answer["error"] = json!(message);
                    answer
                }
                None => report(settle.unwrap_or(500), cx).await,
            }
        }
        other => error(format!("Unknown preview action: {other}")),
    }
}

/// Create the control on first use and load `url`, waiting for the page or the deadline.
async fn open(url: &str, window: AnyWindowHandle, cx: &mut AsyncApp) -> Result<(), String> {
    if current().is_none() {
        let (console, failed, loaded) = PREVIEW.with(|preview| {
            let preview = preview.borrow();
            (preview.console.clone(), preview.failed.clone(), preview.loaded.clone())
        });
        let blocked = failed.clone();
        let folder = std::env::temp_dir().join(format!("rainy-preview-{}", std::process::id()));
        let options = Options {
            folder,
            private: true,
            background: (255, 255, 255),
            script: None,
            on_message: None,
            // Only the first address is checked above; the page itself may then try to leave (a script, a redirect or
            // a link the model clicked). Any navigation off this machine is stopped here.
            allow: Some(Box::new(move |url| {
                let allowed = is_loopback_url(url) || url == "about:blank";
                if !allowed {
                    push(&blocked, format!("{url}: {}", localized("已拦截：预览不离开本机地址", "blocked: the preview stays on this machine")));
                }
                allowed
            })),
            on_new_window: None,
            on_shortcut: None,
        };
        let parent = window.update(cx, |_, window, _| super::hwnd(window)).ok().flatten().ok_or("No window for the preview.")?;
        let created = call(|done| WebView::create(parent, options, done)).await.ok_or("The preview could not start.")??;
        // Laid out at desktop size outside the window's visible area: the page renders, nobody sees it.
        created.set_bounds(-WIDTH - 200, -HEIGHT - 200, WIDTH, HEIGHT);
        created.set_visible(true);
        listen(&created, console, failed, loaded);
        for domain in ["Page.enable", "Runtime.enable", "Log.enable", "Network.enable"] {
            let _ = call(|done| created.devtools(domain, "{}", done)).await;
        }
        // The control's pixels are physical, so on a scaled display the page would lay out narrower than a desktop;
        // fixing the CSS size and the pixel ratio also makes every screenshot WIDTH by HEIGHT.
        let metrics = json!({ "width": WIDTH, "height": HEIGHT, "deviceScaleFactor": 1, "mobile": false }).to_string();
        let _ = call(|done| created.devtools("Emulation.setDeviceMetricsOverride", &metrics, done)).await;
        PREVIEW.with(|preview| preview.borrow_mut().view = Some(created));
    }
    let view = current().ok_or("The preview closed.")?;
    let (loaded_tx, loaded_rx) = async_channel::bounded::<()>(1);
    PREVIEW.with(|preview| {
        let preview = preview.borrow();
        preview.console.borrow_mut().clear();
        preview.failed.borrow_mut().clear();
        *preview.loaded.borrow_mut() = Some(loaded_tx);
    });
    view.navigate(url);
    let timer = cx.background_executor().timer(LOAD_TIMEOUT);
    futures_lite_race(loaded_rx, timer).await;
    Ok(())
}

/// Wait for the page's load signal or the timer, whichever comes first.
async fn futures_lite_race(loaded: async_channel::Receiver<()>, timer: Task<()>) {
    let loaded = async move {
        let _ = loaded.recv().await;
    };
    smol_race(loaded, timer).await;
}

async fn smol_race(first: impl std::future::Future<Output = ()>, second: impl std::future::Future<Output = ()>) {
    use std::pin::pin;
    use std::task::Poll;
    let mut first = pin!(first);
    let mut second = pin!(second);
    std::future::poll_fn(|context| {
        if first.as_mut().poll(context).is_ready() || second.as_mut().poll(context).is_ready() {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    })
    .await
}

/// Console errors and warnings, failed and refused requests, and the end of each load, from the DevTools protocol.
fn listen(view: &WebView, console: Rc<RefCell<VecDeque<String>>>, failed: Rc<RefCell<VecDeque<String>>>, loaded: Rc<RefCell<Option<async_channel::Sender<()>>>>) {
    let requests: Rc<RefCell<HashMap<String, String>>> = Rc::default();
    view.on_devtools_event("Page.loadEventFired", move |_| {
        if let Some(loaded) = loaded.borrow_mut().take() {
            let _ = loaded.try_send(());
        }
    });
    let messages = console.clone();
    view.on_devtools_event("Runtime.consoleAPICalled", move |json| {
        let Ok(event) = serde_json::from_str::<Value>(&json) else { return };
        let level = match event["type"].as_str() {
            Some("error" | "assert") => "error",
            Some("warning") => "warn",
            _ => return,
        };
        let text: Vec<String> = event["args"].as_array().into_iter().flatten().map(|arg| arg["value"].as_str().map_or_else(|| arg["description"].as_str().unwrap_or_default().to_owned(), str::to_owned)).collect();
        let frame = &event["stackTrace"]["callFrames"][0];
        let source = frame["url"].as_str().filter(|url| !url.is_empty()).map(|url| format!(" ({url}:{})", frame["lineNumber"].as_u64().unwrap_or(0) + 1)).unwrap_or_default();
        push(&messages, format!("{level}: {}{source}", text.join(" ")));
    });
    let exceptions = console.clone();
    view.on_devtools_event("Runtime.exceptionThrown", move |json| {
        let Ok(event) = serde_json::from_str::<Value>(&json) else { return };
        let details = &event["exceptionDetails"];
        let text = details["exception"]["description"].as_str().or(details["text"].as_str()).unwrap_or("exception");
        let source = details["url"].as_str().filter(|url| !url.is_empty()).map(|url| format!(" ({url}:{})", details["lineNumber"].as_u64().unwrap_or(0) + 1)).unwrap_or_default();
        push(&exceptions, format!("error: {text}{source}"));
    });
    let logs = console;
    view.on_devtools_event("Log.entryAdded", move |json| {
        let Ok(event) = serde_json::from_str::<Value>(&json) else { return };
        let entry = &event["entry"];
        let level = match entry["level"].as_str() {
            Some("error") => "error",
            Some("warning") => "warn",
            _ => return,
        };
        let url = entry["url"].as_str().unwrap_or_default();
        if site_icon(url) {
            return;
        }
        let source = if url.is_empty() { String::new() } else { format!(" ({url})") };
        push(&logs, format!("{level}: {}{source}", entry["text"].as_str().unwrap_or_default()));
    });
    let sent = requests.clone();
    view.on_devtools_event("Network.requestWillBeSent", move |json| {
        let Ok(event) = serde_json::from_str::<Value>(&json) else { return };
        if let (Some(id), Some(url)) = (event["requestId"].as_str(), event["request"]["url"].as_str()) {
            let mut sent = sent.borrow_mut();
            if sent.len() > 2000 {
                sent.clear();
            }
            sent.insert(id.to_owned(), url.to_owned());
        }
    });
    let broken = failed.clone();
    view.on_devtools_event("Network.responseReceived", move |json| {
        let Ok(event) = serde_json::from_str::<Value>(&json) else { return };
        let status = event["response"]["status"].as_u64().unwrap_or(0);
        let url = event["response"]["url"].as_str().unwrap_or_default();
        // A request that answered 4xx or 5xx did not fail at the network level, but it is still a broken page.
        if status >= 400 && !site_icon(url) {
            push(&broken, format!("{url}: HTTP {status}"));
        }
    });
    view.on_devtools_event("Network.loadingFailed", move |json| {
        let Ok(event) = serde_json::from_str::<Value>(&json) else { return };
        if event["canceled"].as_bool() == Some(true) && event["blockedReason"].is_null() {
            return;
        }
        let url = event["requestId"].as_str().and_then(|id| requests.borrow().get(id).cloned()).unwrap_or_default();
        push(&failed, format!("{url}: {}", event["errorText"].as_str().unwrap_or("failed")));
    });
}

/// What the page shows now, after letting it settle.
async fn report(settle_ms: u64, cx: &mut AsyncApp) -> Value {
    if settle_ms > 0 {
        cx.background_executor().timer(Duration::from_millis(settle_ms)).await;
    }
    let Some(view) = current() else { return error("the preview window closed".to_owned()) };
    let script = read_script();
    let page = match call(|done| view.execute(&script, done)).await {
        Some(Ok(json)) => serde_json::from_str::<Value>(&json).unwrap_or(Value::Null),
        Some(Err(message)) => return error(message),
        None => return error("the preview window closed".to_owned()),
    };
    let (console, failed) = PREVIEW.with(|preview| {
        let preview = preview.borrow();
        (Vec::from(preview.console.borrow().clone()), Vec::from(preview.failed.borrow().clone()))
    });
    json!({
        "url": page["url"], "title": page["title"], "text": page["text"], "elements": page["elements"],
        "console": console, "failed": failed,
    })
}

#[cfg(test)]
mod tests {
    use ::core::prelude::v1::test;

    use super::*;

    #[test]
    fn loopback_only() {
        assert!(is_loopback_url("http://localhost:5173/"));
        assert!(is_loopback_url("http://127.0.0.1:8000/path?q=1"));
        assert!(is_loopback_url("https://[::1]:3000"));
        assert!(is_loopback_url("http://app.localhost"));
        assert!(!is_loopback_url("http://localhost.evil.com/"));
        assert!(!is_loopback_url("http://user@example.com"));
        assert!(!is_loopback_url("file:///C:/x"));
        assert!(!is_loopback_url("javascript:alert(1)"));
        assert!(site_icon("http://127.0.0.1:47400/favicon.ico"));
        assert!(site_icon("http://localhost:5173/favicon.ico?v=2"));
        assert!(!site_icon("http://localhost:5173/missing.json"));
    }
}
