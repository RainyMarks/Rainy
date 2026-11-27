//! Server extraction, normalization, naming, environment unwrapping, and config file decoding.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;

use crate::text::utf16_len;

use super::js_json::{Json, JsonObject, compare_js, has_js_space, is_blank, is_js_space, js_ordered, js_regex, js_trim, number_text};
use super::repair::{add_json_command, not_json, read_values};
use super::{
    ExistingServer, ImportedServer, MCP_JSON_MAX_LENGTH, MCP_JSON_MAX_SERVERS, McpImport, McpImportEnvironment, McpImportMessage, McpServerConfig,
};

fn as_object(value: Option<&Json>) -> Option<&JsonObject> {
    match value {
        Some(Json::Object(object)) => Some(object),
        _ => None,
    }
}

/// `value ?? undefined`: a value that is neither missing nor `null`.
fn present(value: Option<&Json>) -> Option<&Json> {
    value.filter(|value| !matches!(value, Json::Null))
}

/// `a ?? b ?? …`.
fn coalesce<'a>(values: &[Option<&'a Json>]) -> Option<&'a Json> {
    values.iter().find_map(|value| present(*value))
}

static SERVER_TYPE: LazyLock<Regex> = LazyLock::new(|| js_regex(r"(?i)^(?:stdio|local|remote|http|sse|streamable[-_]?http)$"));

/// Whether one object describes a single server rather than a set of them.
fn looks_like_server(value: &Json) -> bool {
    let Json::Object(value) = value else { return false };
    ["command", "url", "serverUrl", "httpUrl", "baseUrl"].iter().any(|key| value.has(key))
        || as_object(value.get("transport")).is_some()
        || matches!(value.get("type"), Some(Json::String(kind)) if SERVER_TYPE.is_match(kind))
}

/// Whether every value of an object describes a server: a name-to-server map without its container key.
fn looks_like_server_map(value: &JsonObject) -> bool {
    !value.entries.is_empty() && value.entries.iter().all(|(_, server)| looks_like_server(server))
}

/// The `(name, server)` entries of one value, from whichever container it uses; unnamed entries get an empty name.
fn server_entries(root: &Json) -> Vec<(String, &Json)> {
    fn from_container(value: Option<&Json>) -> Vec<(String, &Json)> {
        match value {
            Some(Json::Array(items)) => items
                .iter()
                .map(|entry| {
                    let name = match as_object(Some(entry)).and_then(|entry| entry.get("name")) {
                        Some(Json::String(name)) => name.clone(),
                        _ => String::new(),
                    };
                    (name, entry)
                })
                .collect(),
            Some(Json::Object(object)) => object.ordered().into_iter().map(|(key, value)| (key.to_owned(), value)).collect(),
            _ => Vec::new(),
        }
    }
    if let Json::Array(_) = root {
        return from_container(Some(root));
    }
    let Json::Object(object) = root else { return Vec::new() };
    // Most clients: `mcpServers` (an array in Continue); VS Code `.vscode/mcp.json`: `servers`; VS Code settings.json:
    // `mcp.servers`; Zed: `context_servers`; opencode: `mcp` whose values are servers (Gemini CLI's `mcp` is a name filter).
    let mcp = as_object(object.get("mcp"));
    let containers = [
        object.get("mcpServers"),
        object.get("mcp_servers"),
        object.get("servers"),
        mcp.and_then(|mcp| mcp.get("servers")),
        object.get("context_servers"),
        mcp.filter(|mcp| looks_like_server_map(mcp)).and(object.get("mcp")),
        as_object(object.get("experimental")).and_then(|experimental| experimental.get("modelContextProtocolServers")),
    ];
    let mut entries: Vec<(String, &Json)> = containers.into_iter().flat_map(from_container).collect();
    // ~/.claude.json keeps servers per project next to the user's own.
    if let Some(projects) = as_object(object.get("projects")) {
        for (_, project) in projects.ordered() {
            if let Json::Object(project) = project {
                entries.extend(from_container(project.get("mcpServers")));
            }
        }
    }
    if !entries.is_empty() {
        return entries;
    }
    if looks_like_server(root) {
        let name = match object.get("name") {
            Some(Json::String(name)) => name.clone(),
            _ => String::new(),
        };
        return vec![(name, root)];
    }
    if looks_like_server_map(object) {
        return object.ordered().into_iter().map(|(key, value)| (key.to_owned(), value)).collect();
    }
    Vec::new()
}

/// Text of a scalar written where text belongs; numbers and booleans are written out, anything else is refused.
fn scalar(value: Option<&Json>) -> Option<String> {
    match value {
        Some(Json::String(text)) => Some(text.clone()),
        Some(Json::Number(number)) => Some(number_text(*number)),
        Some(Json::Bool(flag)) => Some(flag.to_string()),
        _ => None,
    }
}

/// The first of some fields that holds non-empty text; Cherry Studio exports leave unused fields as `""`.
fn first_text(value: &JsonObject, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| scalar(value.get(key)).map(|text| js_trim(&text).to_owned()).filter(|text| !text.is_empty()))
}

static MCP_SERVER_NAME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z0-9_-]{1,32}$").expect("valid pattern"));
static BARE_EXTENSION: LazyLock<Regex> = LazyLock::new(|| js_regex(r"(?i)\.(?:m?js|cjs|ts|py|exe|cmd|jar)$"));
static BARE_PREFIX: LazyLock<Regex> = LazyLock::new(|| js_regex(r"(?i)^(?:mcp[-_]?server[-_]|server[-_]|mcp[-_])"));
static BARE_SUFFIX: LazyLock<Regex> = LazyLock::new(|| js_regex(r"(?i)[-_](?:mcp[-_]?server|mcp|server)$"));
static NAME_MCP_PART: LazyLock<Regex> = LazyLock::new(|| js_regex(r"(?i)^mcp[-_]server[-_]|[-_]mcp(?:[-_]server)?$"));

/// A package, image or file name without its scope, extension and the MCP words that say nothing about the server.
fn bare_name(part: &str) -> String {
    let part = part.strip_prefix('@').unwrap_or(part);
    let part = BARE_EXTENSION.replacen(part, 1, "");
    let part = BARE_PREFIX.replacen(&part, 1, "");
    BARE_SUFFIX.replacen(&part, 1, "").into_owned()
}

/// A name the Host accepts, built from what the text called the server: other characters become hyphens.
pub fn clean_server_name(source: &str, fallback: &str) -> String {
    let trimmed = js_trim(source);
    if MCP_SERVER_NAME.is_match(trimmed) {
        return trimmed.to_owned();
    }
    let mut hyphenated = String::new();
    let mut replacing = false;
    for c in trimmed.chars() {
        if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
            hyphenated.push(c);
            replacing = false;
        } else if !replacing {
            hyphenated.push('-');
            replacing = true;
        }
    }
    let mut cleaned = hyphenated.trim_matches(['-', '_']).to_owned();
    if cleaned.len() > 32 {
        // `@modelcontextprotocol/server-github` is named after its package; `awslabs.aws-documentation-mcp-server` loses the MCP part.
        let last = trimmed.split(['\\', '/']).rfind(|part| !part.is_empty());
        if let Some(last) = last
            && last != trimmed
        {
            return clean_server_name(&bare_name(last), fallback);
        }
        cleaned = NAME_MCP_PART.replacen(&cleaned, 1, "").into_owned();
    }
    let cleaned = cleaned[..cleaned.len().min(32)].trim_end_matches(['-', '_']);
    if MCP_SERVER_NAME.is_match(cleaned) { cleaned.to_owned() } else { fallback.to_owned() }
}

/// Path parts that say nothing about which server it is.
const GENERIC_PARTS: &[&str] = &["index", "main", "server", "cli", "dist", "build", "src", "bin", "lib", "mcp", "app", "run", "latest"];

/// `pkg==1.2`, `pkg>=1`: the text up to a version operator whose rest stays on one line.
pub(super) fn without_version(word: &str) -> &str {
    // Only an operator after the last line break keeps its rest on one line; looking there alone keeps a long pasted
    // text from being searched once per operator.
    let start = word.char_indices().rev().find(|(_, c)| matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}')).map_or(0, |(at, c)| at + c.len_utf8());
    for (offset, _) in word[start..].char_indices() {
        let at = start + offset;
        if ["==", ">=", "<=", "~=", "!="].iter().any(|operator| word[at..].starts_with(operator)) {
            return &word[..at];
        }
    }
    word
}

/// A name for a server the text left unnamed: the package, image or script it runs, or the site of its URL.
/// `npx -y @modelcontextprotocol/server-github` gives `github`, `@playwright/mcp@latest` gives `playwright`.
pub fn unnamed_server_hint(server: &McpServerConfig) -> String {
    if server.transport != "stdio" {
        let Ok(url) = url::Url::parse(server.url.as_deref().unwrap_or_default()) else { return String::new() };
        let hostname = url.host_str().unwrap_or_default();
        if hostname.contains(':') || (!hostname.is_empty() && hostname.chars().all(|c| c.is_ascii_digit() || c == '.')) {
            return String::new();
        }
        let labels: Vec<&str> = hostname.split('.').filter(|label| !["www", "api", "mcp"].iter().any(|word| label.eq_ignore_ascii_case(word))).collect();
        let label = if labels.len() >= 2 { labels[labels.len() - 2] } else { labels.first().copied().unwrap_or_default() };
        return label.to_owned();
    }
    let words: Vec<&str> = std::iter::once(server.command.as_deref().unwrap_or_default())
        .chain(server.args.iter().flatten().map(String::as_str))
        .filter(|word| !word.is_empty() && !word.starts_with('-'))
        .collect();
    let mut pick = words
        .iter()
        .find(|word| word.to_ascii_lowercase().contains("mcp"))
        .or_else(|| words.get(1))
        .or_else(|| words.first())
        .copied()
        .unwrap_or_default();
    // `pkg@latest`, `pkg==1.2`: the version says nothing about the server.
    if let Some(at) = pick.rfind('@')
        && at > 0
        && !pick[at..].contains(['\\', '/'])
    {
        pick = &pick[..at];
    }
    let pick = without_version(pick);
    pick.split(['\\', '/']).map(bare_name).rfind(|part| !part.is_empty() && !GENERIC_PARTS.contains(&part.to_lowercase().as_str())).unwrap_or_default()
}

/// `name`, or `name-2`, `name-3`… — the first one not in `taken` (compared without case), cut to 32 characters.
fn free_name(name: &str, taken: &HashSet<String>) -> String {
    if !taken.contains(&name.to_lowercase()) {
        return name.to_owned();
    }
    (2..)
        .map(|index| {
            let suffix = format!("-{index}");
            format!("{}{suffix}", &name[..name.len().min(32 - suffix.len())])
        })
        .find(|candidate| !taken.contains(&candidate.to_lowercase()))
        .unwrap_or_default()
}

/// Split `npx -y pkg` written as one string into words; quotes group words.
fn split_command_line(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let last_double = chars.iter().rposition(|c| *c == '"');
    let last_single = chars.iter().rposition(|c| *c == '\'');
    let mut words = Vec::new();
    let mut index = 0;
    while index < chars.len() {
        let c = chars[index];
        let closing = match c {
            '"' if last_double.is_some_and(|last| last > index) => last_double,
            '\'' if last_single.is_some_and(|last| last > index) => last_single,
            _ => None,
        };
        if closing.is_some() {
            let end = (index + 1..chars.len()).find(|&at| chars[at] == c).unwrap_or(chars.len());
            words.push(chars[index + 1..end].iter().collect());
            index = end + 1;
        } else if !is_js_space(c) {
            let end = (index..chars.len()).find(|&at| is_js_space(chars[at])).unwrap_or(chars.len());
            words.push(chars[index..end].iter().collect());
            index = end;
        } else {
            index += 1;
        }
    }
    words
}

/// The command words of a server: `command` + `args`, opencode's command array, Zed's `{ path, args, env }`, or a whole
/// command line in `command` or in `args`; with Zed's environment and whether a string was split into words.
struct CommandWords {
    words: Vec<String>,
    env: Option<Json>,
    split: bool,
}

fn command_words(value: &JsonObject) -> Option<CommandWords> {
    let mut command = present(value.get("command")).cloned();
    let mut args = present(value.get("args")).cloned();
    let mut env = None;
    if let Some(Json::Object(nested)) = &command {
        let nested = nested.clone();
        if let Some(nested_args) = present(nested.get("args")) {
            args = Some(nested_args.clone());
        }
        env = present(nested.get("env")).cloned();
        command = coalesce(&[nested.get("path"), nested.get("command")]).cloned();
    }
    let mut split = false;
    if let Some(Json::Array(items)) = &command {
        let words = items.iter().map(|item| scalar(Some(item))).collect::<Option<Vec<String>>>()?;
        let mut rest: Vec<Json> = words.iter().skip(1).cloned().map(Json::String).collect();
        match &args {
            None => {}
            Some(Json::Array(more)) => rest.extend(more.iter().cloned()),
            Some(Json::String(line)) => {
                split = true;
                rest.extend(split_command_line(line).into_iter().map(Json::String));
            }
            Some(_) => return None,
        }
        command = words.first().cloned().map(Json::String);
        args = Some(Json::Array(rest));
    }
    let program = scalar(command.as_ref()).map(|program| js_trim(&program).to_owned()).filter(|program| !program.is_empty());
    let Some(program) = program else { return Some(CommandWords { words: Vec::new(), env, split }) };
    let rest = match &args {
        None => Vec::new(),
        Some(Json::String(line)) => {
            split = true;
            split_command_line(line)
        }
        Some(Json::Array(items)) => items.iter().map(|item| scalar(Some(item))).collect::<Option<Vec<String>>>()?,
        Some(_) => return None,
    };
    // README snippets sometimes put the whole command line in `command`; a path with spaces stays whole.
    let path = program.starts_with(['\\', '/', '~', '.']) || windows_drive_path(&program);
    if rest.is_empty() && has_js_space(&program) && !path {
        let words = split_command_line(&program);
        return Some(CommandWords { split: words.len() > 1, words, env });
    }
    Some(CommandWords { words: std::iter::once(program).chain(rest).collect(), env, split })
}

/// A string map (environment) with every value written out as text; `null` values are left out.
fn string_map(value: Option<&Json>) -> Option<Vec<(String, String)>> {
    match value {
        None | Some(Json::Null) => Some(Vec::new()),
        Some(Json::Object(object)) => {
            let mut entries = Vec::new();
            for (key, entry) in object.ordered() {
                if matches!(entry, Json::Null) {
                    continue;
                }
                entries.push((key.to_owned(), scalar(Some(entry))?));
            }
            Some(entries)
        }
        _ => None,
    }
}

/// Request headers as a map, a list of `{ name, value }` (the MCP Registry) or a list of `Name: value` lines.
fn header_list(value: Option<&Json>) -> Option<Vec<(String, String)>> {
    let Some(Json::Array(items)) = value else { return string_map(value) };
    let mut entries = Vec::new();
    for item in items {
        match item {
            Json::String(line) => {
                let colon = line.find(':').filter(|colon| *colon > 0)?;
                entries.push((js_trim(&line[..colon]).to_owned(), js_trim(&line[colon + 1..]).to_owned()));
            }
            Json::Object(object) => match (object.get("name"), scalar(object.get("value"))) {
                (Some(Json::String(name)), Some(value)) => entries.push((name.clone(), value)),
                _ => return None,
            },
            _ => return None,
        }
    }
    Some(entries)
}

/// A string list such as `includeTools`; anything else is no list.
fn names(value: Option<&Json>) -> Vec<String> {
    match value {
        Some(Json::Array(items)) => items
            .iter()
            .filter_map(|item| match item {
                Json::String(name) if !name.is_empty() => Some(name.clone()),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// Values left for the user or for the client to fill in, which Rainy passes on unchanged: `${input:token}` (VS Code),
/// `${env:NAME}` (Cursor, Windsurf), `${NAME}` and `${NAME:-default}` (Claude Code), `{env:NAME}` (opencode), `$NAME`
/// (Gemini CLI), `${{ secrets.NAME }}` (Continue), `<YOUR_TOKEN>`, `YOUR_API_KEY`, `your-key-here`, `sk-xxx`.
static PLACEHOLDER: LazyLock<Regex> = LazyLock::new(|| {
    js_regex(
        &[
            r"\$\{\{[^}]{0,80}\}\}",
            r"\$\{[^}]{1,80}\}",
            r"\{(?:env|file):[^}]{1,80}\}",
            r"\$[A-Z_][A-Z0-9_]{2,64}(?-u:\b)",
            r"<[^<>\s]{2,64}>",
            r"(?-u:\b)YOUR[_-][A-Z0-9_-]{1,64}",
            r"(?-u:\b)your[-_][a-z0-9_-]{0,64}(?:key|token|here|secret|password)(?-u:\b)",
            r"(?-u:\b)(?:sk-)?x{3,64}(?-u:\b)",
        ]
        .join("|"),
    )
});

/// The first placeholder in some fields, as a note.
fn placeholder_note(fields: &[(&str, &str)]) -> Option<McpImportMessage> {
    fields.iter().find_map(|(field, text)| PLACEHOLDER.find(text).map(|found| McpImportMessage::with("mcpJsonPlaceholder", &[("field", field), ("value", found.as_str())])))
}

fn windows_drive_path(word: &str) -> bool {
    let mut chars = word.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic()) && chars.next() == Some(':') && matches!(chars.next(), Some('\\' | '/'))
}

/// A path written for another system than the one the Host runs commands on.
fn foreign_path(word: &str, environment: Option<McpImportEnvironment>) -> bool {
    match environment {
        None => false,
        Some(McpImportEnvironment::Windows) => word.starts_with("/Users/") || word.starts_with("/home/"),
        Some(_) => windows_drive_path(word),
    }
}

/// Shells and interpreters that run code given as an argument.
pub(super) static CODE_RUNNER: LazyLock<Regex> = LazyLock::new(|| {
    js_regex(r"(?i)^(?:[^\n\r\x{2028}\x{2029}]*[\\/])?(?:bash|sh|zsh|dash|ksh|fish|powershell|pwsh|node|python[0-9.]*|py|deno|bun|ruby|perl|php)(?:\.exe)?$")
});
/// Programs that pass a command line on to a shell.
pub(super) static LAUNCHER: LazyLock<Regex> = LazyLock::new(|| js_regex(r"(?i)^(?:[^\n\r\x{2028}\x{2029}]*[\\/])?(?:cmd|wsl|ssh)(?:\.exe)?$"));
/// Options that hand an interpreter code: `-c`, clustered as in `bash -lc`, `-e`, `--eval`, `-Command`… (not cmd's `/c`).
pub(super) static SCRIPT_FLAG: LazyLock<Regex> = LazyLock::new(|| js_regex(r"(?i)^(?:-[eilx]{0,3}c|-e|--eval|-p|--print|-Command|-EncodedCommand|-ec)$"));

/// Characters that make an argument shell code rather than a value.
pub(super) fn shell_code(word: &str) -> bool {
    word.contains([';', '|', '&', '<', '>', '$', '`', '\'', '"', '(', ')', '{', '}'])
}

/// For each word, whether one that `matches` came at or before it.
pub(super) fn seen_by(words: &[&str], matches: impl Fn(&str) -> bool) -> Vec<bool> {
    let mut seen = false;
    words
        .iter()
        .map(|word| {
            seen = seen || matches(word);
            seen
        })
        .collect()
}

/// Whether a command runs code written into its arguments (`bash -lc "…; curl … | sh"`, `node -e …`,
/// `powershell -NoProfile -Command …`) rather than a package or a file: the preview shows it in full and asks the user
/// to check it.
fn inline_script(program: &str, args: &[String]) -> bool {
    let words: Vec<&str> = std::iter::once(program).chain(args.iter().map(String::as_str)).collect();
    let runner = seen_by(&words, |word| CODE_RUNNER.is_match(word));
    args.iter().any(|arg| has_js_space(arg) && (arg.contains([';', '|', '`']) || arg.contains("&&") || arg.contains("$(")))
        || words.iter().enumerate().any(|(index, word)| index > 0 && runner[index - 1] && SCRIPT_FLAG.is_match(word))
}

fn remote_type(kind: &str) -> bool {
    matches!(kind, "http" | "streamable-http" | "streamable_http" | "streamablehttp" | "remote" | "sse")
}

fn local_type(kind: &str) -> bool {
    matches!(kind, "stdio" | "local" | "command")
}

fn http_url(url: &str) -> bool {
    url::Url::parse(url).is_ok_and(|url| matches!(url.scheme(), "http" | "https"))
}

/// Read one server; the name is assigned by the caller.
fn read_server(source: &str, entry: &Json, environment: Option<McpImportEnvironment>) -> Result<(McpServerConfig, Vec<McpImportMessage>), McpImportMessage> {
    let name = if source.is_empty() { "—" } else { source };
    let Json::Object(entry) = entry else { return Err(McpImportMessage::with("mcpJsonServerInvalid", &[("name", name)])) };
    // Continue nests the command or URL in `transport`.
    let merged;
    let value = match as_object(entry.get("transport")) {
        Some(transport) => {
            let mut object = JsonObject::default();
            for (key, field) in transport.ordered().into_iter().chain(entry.ordered()) {
                object.insert(key.to_owned(), field.clone());
            }
            object.insert("transport".into(), present(transport.get("type")).cloned().unwrap_or(Json::Null));
            merged = object;
            &merged
        }
        None => entry,
    };
    let mut notes = Vec::new();
    let declared = first_text(value, &["type", "transportType", "transport"]).map(|kind| kind.to_lowercase().chars().filter(|c| !is_js_space(*c)).collect::<String>());
    if let Some(kind) = &declared
        && !local_type(kind)
        && !remote_type(kind)
    {
        return Err(McpImportMessage::with("mcpJsonUnknownType", &[("name", name), ("type", kind)]));
    }
    let url = first_text(value, &["url", "serverUrl", "httpUrl", "baseUrl", "endpoint"]);
    let has_command = match value.get("command") {
        Some(Json::String(command)) => !js_trim(command).is_empty(),
        Some(Json::Array(items)) => !items.is_empty(),
        Some(Json::Object(_)) => true,
        _ => false,
    };
    let mut enabled = !matches!(value.get("disabled"), Some(Json::Bool(true)))
        && !matches!(value.get("enabled"), Some(Json::Bool(false)))
        && !matches!(value.get("isActive"), Some(Json::Bool(false)));
    // Allow-lists become Rainy's allowed tools; deny-lists need the server's tool list, which only connecting gives.
    let include = names(coalesce(&[value.get("includeTools"), value.get("enabled_tools"), value.get("tools")]));
    let exclude = names(coalesce(&[value.get("excludeTools"), value.get("disabledTools"), value.get("disabled_tools")]));
    let everything = include.is_empty() || include.iter().any(|tool| tool == "*");
    let mut tools: Vec<String> = Vec::new();
    if !everything {
        for tool in include.iter().filter(|tool| !exclude.contains(tool)) {
            if !tools.contains(tool) {
                tools.push(tool.clone());
            }
        }
    }
    if !exclude.is_empty() && everything {
        notes.push(McpImportMessage::new("mcpJsonExcludeTools"));
    }
    // Rainy's empty list allows every tool; a server the original allowed none of comes in switched off instead.
    if !everything && tools.is_empty() {
        enabled = false;
        notes.push(McpImportMessage::new("mcpJsonNoTools"));
    }
    if value.has("oauth") || value.has("headersHelper") || value.has("authProviderType") {
        notes.push(McpImportMessage::new("mcpJsonOAuth"));
    }
    if let Some(url) = url
        && (!has_command || declared.as_deref().is_some_and(remote_type))
    {
        if !http_url(&url) {
            return Err(McpImportMessage::with("mcpJsonBadUrl", &[("name", name)]));
        }
        let options = as_object(value.get("requestOptions")).or_else(|| as_object(value.get("requestInit")));
        let headers = header_list(coalesce(&[value.get("headers"), value.get("http_headers"), options.and_then(|options| options.get("headers"))]));
        let Some(mut headers) = headers else { return Err(McpImportMessage::with("mcpJsonBadHeader", &[("name", name), ("key", "headers")])) };
        // Continue's `apiKey` is sent as a bearer token.
        if let Some(key) = first_text(value, &["apiKey"])
            && !headers.iter().any(|(header, _)| header.to_lowercase() == "authorization")
        {
            headers.push(("Authorization".into(), format!("Bearer {key}")));
        }
        let mut seen = HashSet::new();
        for (key, header) in &headers {
            let token = !key.is_empty() && key.chars().all(|c| c.is_ascii_alphanumeric() || "!#$%&'*+.^_`|~-".contains(c));
            let latin = header.chars().all(|c| c == '\t' || (' '..='~').contains(&c) || ('\u{80}'..='\u{FF}').contains(&c));
            if !token || !latin || !seen.insert(key.to_lowercase()) {
                return Err(McpImportMessage::with("mcpJsonBadHeader", &[("name", name), ("key", key)]));
            }
        }
        let fields: Vec<(&str, &str)> = std::iter::once(("URL", url.as_str())).chain(headers.iter().map(|(key, header)| (key.as_str(), header.as_str()))).collect();
        if let Some(placeholder) = placeholder_note(&fields) {
            notes.push(placeholder);
        }
        let server = McpServerConfig {
            name: String::new(),
            enabled,
            transport: "streamable-http".into(),
            command: None,
            args: None,
            env: None,
            url: Some(url),
            headers: (!headers.is_empty()).then(|| js_ordered(headers)),
            tools,
        };
        return Ok((server, notes));
    }
    let Some(read) = command_words(value) else { return Err(McpImportMessage::with("mcpJsonBadArg", &[("name", name)])) };
    let mut words = read.words.into_iter();
    let Some(first) = words.next() else { return Err(McpImportMessage::with("mcpJsonNoCommand", &[("name", name)])) };
    let mut program = Some(first);
    let mut args: Vec<String> = words.collect();
    if read.split {
        notes.push(McpImportMessage::new("mcpJsonCommandSplit"));
    }
    // `cmd /c npx …` and `npx.cmd` are how Windows clients start Node tools; a Linux Host runs the tool itself. WSL can
    // still start Windows programs named with `.exe`, so `cmd.exe` and `uvx.exe` stay there.
    if let Some(environment) = environment
        && environment != McpImportEnvironment::Windows
    {
        let ssh = environment == McpImportEnvironment::Ssh;
        let shell = program.as_deref().is_some_and(|program| program.eq_ignore_ascii_case("cmd") || (ssh && program.eq_ignore_ascii_case("cmd.exe")));
        let wrapped = shell && args.first().is_some_and(|arg| arg.eq_ignore_ascii_case("/c")) && args.len() > 1;
        if wrapped {
            program = Some(args[1].clone());
            args.drain(..2);
        }
        // `cmd /c "npx -y pkg"` hands cmd the whole command line as one argument.
        if wrapped && args.is_empty() && program.as_deref().is_some_and(has_js_space) {
            let mut words = split_command_line(program.as_deref().unwrap_or_default()).into_iter();
            program = words.next();
            args = words.collect();
        }
        let extensions: &[&str] = if ssh { &[".cmd", ".bat", ".exe"] } else { &[".cmd", ".bat"] };
        let bare = program.as_deref().and_then(|program| {
            let lower = program.to_ascii_lowercase();
            let extension = extensions.iter().find(|extension| lower.ends_with(*extension))?;
            let stem = &program[..program.len() - extension.len()];
            (!stem.is_empty() && !stem.contains(['\\', '/'])).then(|| stem.to_owned())
        });
        if bare.is_some() {
            program = bare.clone();
        }
        // Only the program: its arguments may hold a token the preview hides.
        if wrapped || bare.is_some() {
            notes.push(McpImportMessage::with("mcpJsonUnwrapped", &[("command", program.as_deref().unwrap_or("undefined"))]));
        }
    }
    // `cmd /c ""`, or a command line that starts with an empty quoted word, leaves nothing to start.
    let Some(program) = program.filter(|program| !is_blank(program)) else { return Err(McpImportMessage::with("mcpJsonNoCommand", &[("name", name)])) };
    let env = string_map(coalesce(&[value.get("env"), value.get("environment"), read.env.as_ref()]));
    let Some(env) = env else { return Err(McpImportMessage::with("mcpJsonBadEnv", &[("name", name), ("key", "env")])) };
    for (key, text) in &env {
        let mut chars = key.chars();
        let valid = chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_') && chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
        if !valid || text.contains(['\r', '\n']) {
            return Err(McpImportMessage::with("mcpJsonBadEnv", &[("name", name), ("key", key)]));
        }
    }
    if program.contains(['\r', '\n']) || args.iter().any(|arg| arg.contains(['\r', '\n'])) {
        return Err(McpImportMessage::with("mcpJsonBadArg", &[("name", name)]));
    }
    let script = inline_script(&program, &args);
    if script {
        notes.push(McpImportMessage::new("mcpJsonInlineScript"));
    }
    if first_text(value, &["cwd"]).is_some() {
        notes.push(McpImportMessage::new("mcpJsonCwd"));
    }
    if value.has("envFile") {
        notes.push(McpImportMessage::new("mcpJsonEnvFile"));
    }
    if let Some(elsewhere) = std::iter::once(&program).chain(&args).find(|word| foreign_path(word, environment)) {
        notes.push(McpImportMessage::with("mcpJsonForeignPath", &[("value", elsewhere)]));
    }
    // In a script `$NAME` is the script's own variable, not a value left to fill in.
    let mut fields: Vec<(&str, &str)> = Vec::new();
    if !script {
        fields.extend(std::iter::once(&program).chain(&args).map(|word| ("args", word.as_str())));
    }
    fields.extend(env.iter().map(|(key, text)| (key.as_str(), text.as_str())));
    if let Some(placeholder) = placeholder_note(&fields) {
        notes.push(placeholder);
    }
    let server = McpServerConfig {
        name: String::new(),
        enabled,
        transport: "stdio".into(),
        command: Some(program),
        args: (!args.is_empty()).then_some(args),
        env: (!env.is_empty()).then(|| js_ordered(env)),
        url: None,
        headers: None,
        tools,
    };
    Ok((server, notes))
}

/// What a server runs or reaches, without its name, switch or tool choice: two servers alike here are the same server.
fn server_identity(server: &McpServerConfig) -> String {
    let sorted = |mut pairs: Vec<(String, String)>| {
        pairs.sort_by(|(a, _), (b, _)| compare_js(a, b));
        pairs.into_iter().map(|(key, value)| serde_json::json!([key, value])).collect::<Vec<_>>()
    };
    let env = sorted(server.env.clone().unwrap_or_default());
    let headers = sorted(server.headers.iter().flatten().map(|(key, value)| (key.to_lowercase(), value.clone())).collect());
    serde_json::json!([
        server.transport,
        server.command.as_deref().unwrap_or_default(),
        server.args.clone().unwrap_or_default(),
        env,
        server.url.as_deref().unwrap_or_default(),
        headers
    ])
    .to_string()
}

/// Read MCP servers from pasted JSON.
///
/// `existing` are the servers already configured, or just their names: new ones get free names next to them, and one
/// that runs the same as a configured server is left out, so pasting a configuration again adds no copies.
/// `environment` is where the Host runs commands, so commands and paths written for another system are pointed out.
/// The result is the servers to add with notes for each and the entries that could not be read, or why nothing could
/// be read.
pub fn parse_mcp_json(text: &str, existing: &[ExistingServer], environment: Option<McpImportEnvironment>) -> Result<McpImport, McpImportMessage> {
    if utf16_len(text) > MCP_JSON_MAX_LENGTH {
        return Err(McpImportMessage::new("mcpJsonTooLarge"));
    }
    let trimmed = js_trim(text);
    if trimmed.is_empty() {
        return Err(McpImportMessage::new("mcpJsonEmpty"));
    }
    let address;
    let values;
    let mut backslashes = false;
    let add_json = add_json_command(trimmed);
    let mut entries: Vec<(String, &Json)> = if trimmed.strip_prefix("https://").or_else(|| trimmed.strip_prefix("http://")).is_some_and(|rest| !rest.is_empty() && !has_js_space(rest)) {
        // A bare address from a server's page.
        let mut object = JsonObject::default();
        object.insert("url".into(), Json::String(trimmed.to_owned()));
        address = Json::Object(object);
        vec![(String::new(), &address)]
    } else {
        let read = read_values(add_json.as_ref().map_or(trimmed, |(_, json)| json));
        let (read, doubled) = match read {
            Ok(read) => read,
            Err(detail) => return Err(not_json(trimmed).unwrap_or_else(|| McpImportMessage::with("mcpJsonInvalid", &[("detail", &detail)]))),
        };
        values = read;
        backslashes = doubled;
        let mut entries: Vec<(String, &Json)> = values.iter().flat_map(server_entries).collect();
        if let Some((name, _)) = &add_json
            && entries.len() == 1
        {
            entries[0].0 = name.clone();
        }
        entries
    };
    if entries.is_empty() {
        return Err(not_json(trimmed).unwrap_or_else(|| McpImportMessage::new("mcpJsonNoServers")));
    }
    if entries.len() > MCP_JSON_MAX_SERVERS {
        return Err(McpImportMessage::with("mcpJsonTooMany", &[("max", &MCP_JSON_MAX_SERVERS.to_string())]));
    }
    let mut taken: HashSet<String> = existing.iter().map(|entry| entry.name().to_lowercase()).collect();
    let mut configured = HashMap::new();
    for entry in existing {
        if let ExistingServer::Server(server) = entry {
            configured.insert(server_identity(server), server.name.clone());
        }
    }
    let mut servers = Vec::new();
    let mut problems = Vec::new();
    // The same server copied twice, from two tabs of a README, is read once.
    let mut seen = HashSet::new();
    for (source, value) in entries.drain(..) {
        let (mut server, notes) = match read_server(&source, value, environment) {
            Ok(read) => read,
            Err(problem) => {
                problems.push(problem);
                continue;
            }
        };
        let identity = server_identity(&server);
        if let Some(same) = configured.get(&identity) {
            problems.push(McpImportMessage::with("mcpJsonDuplicate", &[("name", if source.is_empty() { "—" } else { &source }), ("existing", same)]));
            continue;
        }
        if !seen.insert(identity) {
            continue;
        }
        // A server written without a usable name is called after its package or site.
        let mut name = clean_server_name(&source, "");
        if name.is_empty() {
            name = clean_server_name(&unnamed_server_hint(&server), "server");
        }
        let name = free_name(&name, &taken);
        taken.insert(name.to_lowercase());
        let mut all = Vec::new();
        if !source.is_empty() && name != source {
            all.push(McpImportMessage::with("mcpJsonRenamed", &[("from", &source), ("to", &name)]));
        }
        all.extend(notes);
        server.name = name;
        servers.push(ImportedServer { source, server, notes: all });
    }
    Ok(McpImport { servers, problems, notices: if backslashes { vec![McpImportMessage::new("mcpJsonBackslashes")] } else { Vec::new() } })
}

static ADD_JSON_START: LazyLock<Regex> = LazyLock::new(|| js_regex(r"^claude\s+mcp\s+add-json\s"));
static CONTAINER_KEY: LazyLock<Regex> = LazyLock::new(|| js_regex(r#""(?:mcpServers|mcp_servers|servers|context_servers)"\s*:\s*[{\[]"#));
static COMMAND_KEY: LazyLock<Regex> = LazyLock::new(|| js_regex(r#""command"\s*:"#));
static NAMED_FRAGMENT: LazyLock<Regex> = LazyLock::new(|| js_regex(r#"^"[^"\n]+"\s*:\s*\{"#));
static SERVER_KEY: LazyLock<Regex> = LazyLock::new(|| js_regex(r#""(?:command|url|serverUrl|httpUrl|baseUrl)"\s*:"#));

/// Whether pasted text is clearly an MCP configuration rather than a value for one field, so a paste into an empty new
/// server form imports it: a server container, a server with its command, or a named server fragment. A lone
/// `{"url": …}` may be a setting the server itself needs, so it is pasted as it is.
pub fn looks_like_mcp_json(text: &str) -> bool {
    let trimmed = js_trim(text);
    let trimmed = match trimmed.strip_prefix("```") {
        Some(rest) => rest.find('\n').map_or(trimmed, |line| &rest[line + 1..]),
        None => trimmed,
    };
    if ADD_JSON_START.is_match(trimmed) {
        return true;
    }
    if trimmed.starts_with(['{', '[']) && CONTAINER_KEY.is_match(trimmed) {
        return true;
    }
    if trimmed.starts_with('{') && COMMAND_KEY.is_match(trimmed) {
        return true;
    }
    NAMED_FRAGMENT.is_match(trimmed) && SERVER_KEY.is_match(trimmed)
}

/// Identifies a server read from the text across previews, which may rename it or move it in the list.
pub fn import_key(entry: &ImportedServer) -> String {
    serde_json::json!([entry.source, server_identity(&entry.server)]).to_string()
}

fn decode_utf16(bytes: &[u8], big_endian: bool) -> String {
    let mut chunks = bytes.chunks_exact(2);
    let units = (&mut chunks).map(|pair| if big_endian { u16::from_be_bytes([pair[0], pair[1]]) } else { u16::from_le_bytes([pair[0], pair[1]]) });
    let mut out: String = char::decode_utf16(units).map(|decoded| decoded.unwrap_or(char::REPLACEMENT_CHARACTER)).collect();
    if !chunks.remainder().is_empty() {
        out.push(char::REPLACEMENT_CHARACTER);
    }
    out
}

#[cfg(windows)]
fn decode_gb18030(bytes: &[u8]) -> String {
    use windows_sys::Win32::Globalization::MultiByteToWideChar;
    const CP_GB18030: u32 = 54936;
    if bytes.is_empty() {
        return String::new();
    }
    let Ok(byte_len) = i32::try_from(bytes.len()) else {
        return String::from_utf8_lossy(bytes).into_owned();
    };
    // SAFETY: `bytes` is a valid non-empty slice of length `byte_len`; `cchWideChar = 0` queries the required UTF-16 length.
    let needed = unsafe { MultiByteToWideChar(CP_GB18030, 0, bytes.as_ptr(), byte_len, std::ptr::null_mut(), 0) };
    if needed <= 0 {
        return String::from_utf8_lossy(bytes).into_owned();
    }
    let mut wide = vec![0u16; needed as usize];
    // SAFETY: `wide` holds `needed` UTF-16 code units as returned by the size query above.
    let written = unsafe { MultiByteToWideChar(CP_GB18030, 0, bytes.as_ptr(), byte_len, wide.as_mut_ptr(), needed) };
    if written <= 0 {
        return String::from_utf8_lossy(bytes).into_owned();
    }
    String::from_utf16_lossy(&wide[..written as usize])
}

#[cfg(not(windows))]
fn decode_gb18030(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// Text of a chosen configuration file: UTF-8, UTF-16 with its byte-order mark (Windows PowerShell's `>`), or else
/// GB18030, which covers the GBK that Chinese Windows writes.
pub fn decode_config_file(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        return decode_utf16(rest, false);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        return decode_utf16(rest, true);
    }
    let text = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    match std::str::from_utf8(text) {
        Ok(text) => text.to_owned(),
        Err(_) => decode_gb18030(bytes),
    }
}

/// The text of a chosen file; a file far larger than any configuration is refused before it is read.
pub fn read_config_file(path: &Path) -> Result<String, McpImportMessage> {
    let failed = |error: std::io::Error| McpImportMessage::with("mcpImportFileError", &[("error", &error.to_string())]);
    let size = std::fs::metadata(path).map_err(failed)?.len();
    // The text limit is in characters; four bytes hold any of them.
    if size > (MCP_JSON_MAX_LENGTH * 4) as u64 {
        return Err(McpImportMessage::new("mcpJsonTooLarge"));
    }
    std::fs::read(path).map(|bytes| decode_config_file(&bytes)).map_err(failed)
}
