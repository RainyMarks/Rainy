//! `web_search` and `web_fetch` through DeepSeek's Search and Fetch APIs, paid for with the user's DeepSeek API key.

use std::sync::Arc;
use std::time::Duration;

use futures::future::BoxFuture;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::{Tool, ToolContext, ToolOutput, Updates, params, spec};
use crate::agent::llm::ToolSpec;
use crate::network::Network;
use crate::settings::{LOCAL_NO_KEY, Settings};

/// DeepSeek's API host; only models served from it lend their key to web search.
pub const DEEPSEEK_API: &str = "https://api.deepseek.com";
const SEARCH_TIMEOUT: Duration = Duration::from_secs(30);
const FETCH_TIMEOUT: Duration = Duration::from_secs(60);
const DEFAULT_COUNT: u64 = 8;
const MAX_COUNT: u64 = 20;
/// Fetched pages beyond this are cut before the tool budget applies.
const MAX_PAGE_CHARS: usize = 400_000;

/// What the web tools need from the Host.
#[derive(Clone)]
pub struct WebAccess {
    pub api_key: Arc<dyn Fn() -> Option<String> + Send + Sync>,
    pub network: Arc<Network>,
}

/// Whether a service address is DeepSeek's official API.
pub fn is_deepseek(base_url: &str) -> bool {
    url::Url::parse(base_url).ok().and_then(|url| url.host_str().map(str::to_lowercase)).is_some_and(|host| host == "api.deepseek.com")
}

/// The key web search uses: the selected model's when it is a DeepSeek model, else the first DeepSeek model's.
pub fn deepseek_key(settings: &Settings) -> Option<String> {
    let data = settings.get();
    let providers: Vec<String> = data.models.iter().filter(|model| is_deepseek(&model.base_url)).map(|model| model.provider.clone()).collect();
    let selected = data.selected.map(|selected| selected.provider).filter(|selected| providers.contains(selected));
    selected.into_iter().chain(providers).find_map(|provider| settings.api_key(&provider).filter(|key| !key.is_empty() && key != LOCAL_NO_KEY))
}

/// Why a call failed, in words the model and the user can act on.
fn failure(status: u16, body: &str, what: &str) -> String {
    let message = serde_json::from_str::<Value>(body).ok().and_then(|value| value.pointer("/error/message").and_then(Value::as_str).map(str::to_owned)).unwrap_or_else(|| body.chars().take(300).collect());
    let reason = match status {
        401 => "the DeepSeek API key was rejected; the user can update it in Settings → Models".to_owned(),
        402 => "the DeepSeek account has insufficient balance; the user needs to top it up".into(),
        403 => "DeepSeek refused to fetch this URL under its security policy".into(),
        404 => "no readable page content was found at this URL (unavailable, blocked by the site, or not a web page)".into(),
        429 => format!("the DeepSeek {what} rate limit was exceeded; wait a moment before retrying"),
        // DeepSeek answers 500 rather than 404 for a page on a host that does not exist.
        500.. if what == "fetch" => "DeepSeek could not fetch this page; it may be unreachable, or DeepSeek had a temporary error".into(),
        500.. => "DeepSeek had a temporary error; retry later".into(),
        _ => "invalid request".into(),
    };
    format!("web {what} failed (HTTP {status}): {reason}{}", if message.is_empty() { String::new() } else { format!(" — {message}") })
}

async fn post(web: &WebAccess, path: &str, body: Value, timeout: Duration, cancel: &CancellationToken, what: &str) -> Result<Value, String> {
    let key = (web.api_key)().ok_or("web access needs a DeepSeek API key; the user can add a DeepSeek model with its key in Settings → Models")?;
    let url = format!("{DEEPSEEK_API}{path}");
    let parsed = url::Url::parse(&url).map_err(|error| error.to_string())?;
    let client = web.network.client(&parsed).await;
    for attempt in 0.. {
        let request = client.post(&url).timeout(timeout).bearer_auth(&key).json(&body).send();
        let response = tokio::select! {
            response = request => response,
            () = cancel.cancelled() => return Err("tool call aborted".into()),
        };
        let response = match response {
            Ok(response) => response,
            Err(error) if error.is_timeout() => return Err(format!("web {what} timed out after {} s", timeout.as_secs())),
            Err(error) => return Err(format!("web {what} could not reach DeepSeek: {error}")),
        };
        let status = response.status().as_u16();
        let text = response.text().await.unwrap_or_default();
        if (200..300).contains(&status) {
            return serde_json::from_str(&text).map_err(|_| format!("web {what} returned an unreadable answer"));
        }
        // One quiet retry for the per-second limit and passing server errors.
        if attempt == 0 && (status == 429 || status >= 500) {
            tokio::time::sleep(Duration::from_millis(1200)).await;
            continue;
        }
        return Err(failure(status, &text, what));
    }
    unreachable!()
}

fn string(value: &Value) -> String {
    value.as_str().unwrap_or_default().to_owned()
}

#[derive(Deserialize)]
struct SearchParams {
    query: String,
    count: Option<f64>,
}

pub struct WebSearch {
    web: WebAccess,
    spec: ToolSpec,
}

impl WebSearch {
    pub fn new(web: WebAccess) -> Self {
        let spec = spec(
            "web_search",
            "Search the web with the DeepSeek search engine for current or external information: documentation, papers, news, error messages, library versions. Returns titles, URLs, dates and a summary of each result; use web_fetch to read a page in full.",
            json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Search terms, at most 128 characters. Supports site:, -site:, \"exact phrase\", -word, lang:zh,en, after:2025-01-01 and before:2026." },
                    "count": { "type": "number", "description": format!("Number of results, 1–{MAX_COUNT}. Defaults to {DEFAULT_COUNT}.") },
                },
                "required": ["query"],
            }),
        );
        Self { web, spec }
    }
}

impl Tool for WebSearch {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn execute(&self, _call_id: String, args: Value, cancel: CancellationToken, _updates: Updates) -> BoxFuture<'_, Result<ToolOutput, String>> {
        Box::pin(async move {
            let params: SearchParams = params(args)?;
            let query = params.query.trim().to_owned();
            if query.is_empty() {
                return Err("invalid query: expected search terms".into());
            }
            if query.chars().count() > 128 {
                return Err("invalid query: at most 128 characters".into());
            }
            let count = params.count.map_or(DEFAULT_COUNT, |count| (count.floor().max(1.0) as u64).min(MAX_COUNT));
            let answer = post(&self.web, "/v1/tools/search", json!({ "query": query, "count": count }), SEARCH_TIMEOUT, &cancel, "search").await?;
            let results: Vec<Value> = answer["results"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|item| {
                    let url = string(&item["url"]);
                    (!url.is_empty()).then(|| {
                        let title = Some(string(&item["title"])).filter(|title| !title.is_empty()).unwrap_or_else(|| url.clone());
                        let snippet = string(&item["snippet"]);
                        let summary = Some(string(&item["summary"])).filter(|summary| !summary.is_empty()).unwrap_or_else(|| snippet.clone());
                        let date: String = string(&item["date"]).chars().take(10).collect();
                        json!({ "title": title, "url": url, "site": Some(string(&item["site_name"])).filter(|site| !site.is_empty()), "date": Some(date).filter(|date| !date.is_empty()), "snippet": snippet, "summary": summary })
                    })
                })
                .collect();
            if results.is_empty() {
                return Ok(ToolOutput::text(format!("No web results for \"{query}\"."), json!({ "query": query, "results": [] })));
            }
            let text = results
                .iter()
                .enumerate()
                .map(|(index, item)| {
                    let mut entry = format!("[{}] {}\nURL: {}", index + 1, string(&item["title"]), string(&item["url"]));
                    if let Some(site) = item["site"].as_str() {
                        entry.push_str(&format!("\nSite: {site}"));
                    }
                    if let Some(date) = item["date"].as_str() {
                        entry.push_str(&format!("\nDate: {date}"));
                    }
                    entry + "\n" + &string(&item["summary"])
                })
                .collect::<Vec<_>>()
                .join("\n\n");
            let shown: Vec<Value> = results
                .into_iter()
                .map(|mut item| {
                    let object = item.as_object_mut().unwrap();
                    object.remove("summary");
                    object.retain(|_, value| !value.is_null());
                    item
                })
                .collect();
            Ok(ToolOutput::text(format!("{text}\n\nCite the URLs you rely on."), json!({ "query": query, "results": shown })))
        })
    }
}

/// Check a URL before it leaves for DeepSeek.
pub fn fetchable_url(value: &str) -> Result<url::Url, String> {
    let url = url::Url::parse(value.trim()).map_err(|_| "invalid url: expected an absolute http(s) URL".to_owned())?;
    if url.scheme() != "http" && url.scheme() != "https" {
        return Err("invalid url: only http and https pages can be fetched".into());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("invalid url: URLs with a user name or password cannot be fetched".into());
    }
    if url.as_str().len() > 8192 {
        return Err("invalid url: at most 8192 characters".into());
    }
    Ok(url)
}

#[derive(Deserialize)]
struct FetchParams {
    url: String,
}

pub struct WebFetch {
    web: WebAccess,
    context: ToolContext,
    spec: ToolSpec,
}

impl WebFetch {
    pub fn new(web: WebAccess, context: ToolContext) -> Self {
        let spec = spec(
            "web_fetch",
            "Read one web page as Markdown through DeepSeek. Use it for URLs from web_search or from the user. Pages are fetched by DeepSeek's servers, so pages on this computer or a private network cannot be reached.",
            json!({ "type": "object", "properties": { "url": { "type": "string", "description": "The http(s) URL of the page to read." } }, "required": ["url"] }),
        );
        Self { web, context, spec }
    }
}

impl Tool for WebFetch {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn execute(&self, _call_id: String, args: Value, cancel: CancellationToken, _updates: Updates) -> BoxFuture<'_, Result<ToolOutput, String>> {
        Box::pin(async move {
            let params: FetchParams = params(args)?;
            let url = fetchable_url(&params.url)?;
            let answer = post(&self.web, "/v1/tools/fetch", json!({ "url": url.as_str() }), FETCH_TIMEOUT, &cancel, "fetch").await?;
            let title = string(&answer["title"]);
            let mut content = string(&answer["content"]);
            let characters = crate::text::js_length(&content);
            if content.chars().count() > MAX_PAGE_CHARS {
                content = format!("{}\n\n[page cut at {MAX_PAGE_CHARS} characters]", crate::text::truncate_chars(&content, MAX_PAGE_CHARS));
            }
            let heading = if title.is_empty() { String::new() } else { format!("# {title}\n") };
            let text = self.context.limit_output("web_fetch", format!("{heading}URL: {url}\n\n{content}"));
            Ok(ToolOutput::text(text, json!({ "url": url.as_str(), "title": title, "characters": characters })))
        })
    }
}
