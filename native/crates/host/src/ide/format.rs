//! Formatting of unsaved editor text with local tools (3.x `ide/format.ts`): Ruff for Python, TypeScript's own
//! formatter (through one short session of its language server) for JavaScript and TypeScript, and the installed
//! clang-format for C and C++. The file itself is never written.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use super::framing;
use super::tools::Tools;
use crate::error::{HostError, HostResult};
use crate::paths::{join_relative, relative_path};
use crate::process::{child_env, read_limited, resolve_executable};

const MAX_TEXT_BYTES: usize = 5 * 1024 * 1024;
const MAX_STDERR_BYTES: usize = 65536;
const TIMEOUT: Duration = Duration::from_secs(15);

pub async fn format(request: &Value, root: &Path, tools: &Tools, environment: &BTreeMap<String, String>) -> HostResult<Value> {
    let path = request["path"].as_str().ok_or_else(|| HostError::invalid("Invalid formatter request"))?;
    relative_path(path, false)?;
    let text = request["text"].as_str().ok_or_else(|| HostError::invalid("Invalid formatter request"))?;
    if text.len() > MAX_TEXT_BYTES {
        return Err(HostError::new("too-large", "Editor text exceeds the formatting limit"));
    }
    let file = join_relative(root, path);
    let filename = file.display().to_string();
    let env = child_env(environment);
    let work = async {
        match request["language"].as_str() {
            Some("python") => filter(&[tools.ruff().display().to_string(), "format".into(), "--isolated".into(), "--stdin-filename".into(), filename, "-".into()], root, &env, text).await,
            Some(language @ ("javascript" | "typescript")) => script(&tools.tsc(), root, &file, language, &text.replace("\r\n", "\n"), &env).await,
            Some("c" | "cpp") => {
                let clang = resolve_executable("clang-format", Some(&env)).ok_or_else(|| crate::process::executable_missing("clang-format"))?;
                filter(&[clang.display().to_string(), "--style=LLVM".into(), format!("--assume-filename={filename}")], root, &env, text).await
            }
            _ => Err(HostError::invalid("Invalid formatter request")),
        }
    };
    let formatted = tokio::time::timeout(TIMEOUT, work).await.map_err(|_| HostError::new("aborted", "Formatting timed out"))??;
    // A CRLF document stays CRLF.
    let crlf = text.contains("\r\n") && !text.replace("\r\n", "").contains('\n');
    Ok(json!({ "text": if crlf { formatted.replace("\r\n", "\n").replace('\n', "\r\n") } else { formatted } }))
}

/// Run a formatter that reads the text on standard input and writes the result on standard output.
async fn filter(argv: &[String], root: &Path, env: &BTreeMap<std::ffi::OsString, std::ffi::OsString>, text: &str) -> HostResult<String> {
    let mut command = crate::process::command(&argv[0], &argv[1..], root, env);
    command.env("PYTHONDONTWRITEBYTECODE", "1").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    let mut child = command.spawn().map_err(|error| HostError::new("io-error", format!("{}: {error}", argv[0])))?;
    let mut stdin = child.stdin.take().expect("piped");
    let input = text.to_owned();
    let writing = tokio::spawn(async move {
        let _ = stdin.write_all(input.as_bytes()).await;
        let _ = stdin.shutdown().await;
    });
    let (stdout, stderr) = (child.stdout.take().expect("piped"), child.stderr.take().expect("piped"));
    let (out, err, status) = tokio::join!(read_limited(stdout, MAX_TEXT_BYTES + 1), read_limited(stderr, MAX_STDERR_BYTES), child.wait());
    writing.abort();
    let status = status?;
    if !status.success() || out.len() > MAX_TEXT_BYTES {
        let message = String::from_utf8_lossy(&err).trim().to_owned();
        return Err(HostError::new("io-error", if message.is_empty() { "Formatting did not produce a complete result".to_owned() } else { message }));
    }
    Ok(String::from_utf8_lossy(&out).into_owned())
}

/// Format JavaScript or TypeScript (LF text) with `tsc --lsp --stdio`: open the document, ask for its formatting edits,
/// apply them.
async fn script(tsc: &Path, root: &Path, file: &Path, language: &str, text: &str, env: &BTreeMap<std::ffi::OsString, std::ffi::OsString>) -> HostResult<String> {
    let missing = || HostError::new("not-found", format!("TypeScript's language server is missing: {}", tsc.display()));
    if !tsc.is_file() {
        return Err(missing());
    }
    let (root_uri, uri) = match (url::Url::from_directory_path(root), url::Url::from_file_path(file)) {
        (Ok(root), Ok(file)) => (root.to_string(), file.to_string()),
        _ => return Err(HostError::invalid("Invalid formatter request")),
    };
    let mut command = crate::process::command(tsc, &["--lsp", "--stdio"], root, env);
    command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).kill_on_drop(true);
    let mut child = command.spawn().map_err(|_| missing())?;
    let mut server = Server { input: child.stdin.take().expect("piped"), output: child.stdout.take().expect("piped"), decoder: framing::Decoder::new(MAX_TEXT_BYTES * 4), received: Vec::new() };
    server.request(1, "initialize", json!({ "processId": std::process::id(), "rootUri": root_uri, "capabilities": {} })).await?;
    server.send(json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} })).await?;
    server.send(json!({ "jsonrpc": "2.0", "method": "textDocument/didOpen", "params": { "textDocument": { "uri": uri, "languageId": language, "version": 1, "text": text } } })).await?;
    let edits = server.request(2, "textDocument/formatting", json!({ "textDocument": { "uri": uri }, "options": { "tabSize": 2, "insertSpaces": true } })).await?;
    let _ = server.send(json!({ "jsonrpc": "2.0", "id": 3, "method": "shutdown" })).await;
    let _ = server.send(json!({ "jsonrpc": "2.0", "method": "exit" })).await;
    drop(server);
    if tokio::time::timeout(Duration::from_secs(2), child.wait()).await.is_err() {
        let _ = child.kill().await;
    }
    apply_edits(text, edits.as_array().map(Vec::as_slice).unwrap_or_default())
}

/// One language-server session over the child's standard streams.
struct Server {
    input: tokio::process::ChildStdin,
    output: tokio::process::ChildStdout,
    decoder: framing::Decoder,
    received: Vec<Value>,
}

impl Server {
    async fn send(&mut self, message: Value) -> HostResult<()> {
        self.input.write_all(&framing::encode(&message)).await.map_err(|error| HostError::new("io-error", error.to_string()))
    }

    /// Send a request and wait for its result; the server's own requests meanwhile are answered with nothing, since it
    /// may wait for them before it answers.
    async fn request(&mut self, id: u64, method: &str, params: Value) -> HostResult<Value> {
        self.send(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })).await?;
        let mut chunk = vec![0; 65536];
        loop {
            while !self.received.is_empty() {
                let message = self.received.remove(0);
                if message.get("method").is_some() {
                    if let Some(request) = message.get("id") {
                        self.send(json!({ "jsonrpc": "2.0", "id": request, "result": null })).await?;
                    }
                } else if message["id"] == id {
                    return match message.get("error") {
                        Some(error) => Err(HostError::new("io-error", error["message"].as_str().unwrap_or("The TypeScript formatter failed").to_owned())),
                        None => Ok(message["result"].clone()),
                    };
                }
            }
            let read = self.output.read(&mut chunk).await.map_err(|error| HostError::new("io-error", error.to_string()))?;
            if read == 0 {
                return Err(HostError::new("io-error", "TypeScript's language server stopped"));
            }
            let messages = self.decoder.push(&chunk[..read]).map_err(|error| HostError::new("io-error", error))?;
            self.received.extend(messages);
        }
    }
}

/// Apply language-server text edits (positions in UTF-16 units) to LF text.
fn apply_edits(text: &str, edits: &[Value]) -> HostResult<String> {
    let starts: Vec<usize> = std::iter::once(0).chain(text.match_indices('\n').map(|(at, _)| at + 1)).collect();
    let offset = |position: &Value| -> Option<usize> {
        let line = usize::try_from(position["line"].as_u64()?).ok()?;
        let units = usize::try_from(position["character"].as_u64()?).ok()?;
        let start = *starts.get(line)?;
        let end = starts.get(line + 1).map_or(text.len(), |next| next - 1);
        let mut counted = 0;
        for (at, char) in text[start..end].char_indices() {
            if counted >= units {
                return Some(start + at);
            }
            counted += char.len_utf16();
        }
        Some(end)
    };
    let mut spans = edits
        .iter()
        .map(|edit| Some((offset(&edit["range"]["start"])?, offset(&edit["range"]["end"])?, edit["newText"].as_str()?)))
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| HostError::new("io-error", "The TypeScript formatter returned an invalid edit"))?;
    spans.sort_by_key(|(start, end, _)| std::cmp::Reverse((*start, *end)));
    let mut result = text.to_owned();
    let mut limit = usize::MAX;
    for (start, end, replacement) in spans {
        if start > end || end > limit {
            return Err(HostError::new("io-error", "The TypeScript formatter returned overlapping edits"));
        }
        result.replace_range(start..end, replacement);
        limit = start;
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn applies_utf16_edits_from_the_end() {
        let at = |line: u64, character: u64| json!({ "line": line, "character": character });
        let edit = |start: Value, end: Value, text: &str| json!({ "range": { "start": start, "end": end }, "newText": text });
        let text = "const  x=1;\nlet 😀=\"a\";\n";
        let edits = [edit(at(0, 5), at(0, 7), " "), edit(at(0, 8), at(0, 8), " "), edit(at(0, 9), at(0, 9), " "), edit(at(1, 6), at(1, 6), " "), edit(at(1, 7), at(1, 7), " ")];
        assert_eq!(apply_edits(text, &edits).unwrap(), "const x = 1;\nlet 😀 = \"a\";\n");
        assert!(apply_edits(text, &[edit(at(0, 0), at(0, 3), "a"), edit(at(0, 2), at(0, 4), "b")]).is_err());
    }
}
