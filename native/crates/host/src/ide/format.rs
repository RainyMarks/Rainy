//! Formatting of unsaved editor text with local tools (3.x `ide/format.ts`): Ruff for Python, TypeScript's own
//! formatter (through one short session of its language server) for JavaScript and TypeScript, and the installed
//! clang-format for C and C++. The file itself is never written.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::AsyncWriteExt as _;

use super::lsp_client::{LspStdioClient, document_language_id};
use super::tools::{Kit, Tools};
use crate::error::{HostError, HostResult};
use crate::locale::l;
use crate::paths::{join_relative, relative_path};
use crate::process::{ProcessJob, child_env, contain_child, read_limited, resolve_executable};

const MAX_TEXT_BYTES: usize = 5 * 1024 * 1024;
const MAX_STDERR_BYTES: usize = 65536;
const TIMEOUT: Duration = Duration::from_secs(15);

pub async fn format(request: &Value, root: &Path, tools: &Tools, environment: &BTreeMap<String, String>) -> HostResult<Value> {
    let path = request["path"].as_str().ok_or_else(|| HostError::invalid("Invalid formatter request"))?;
    relative_path(path, false)?;
    let text = request["text"].as_str().ok_or_else(|| HostError::invalid("Invalid formatter request"))?;
    if text.len() > MAX_TEXT_BYTES {
        return Err(HostError::new("too-large", l("文本超过格式化大小上限。", "Editor text exceeds the formatting limit.")));
    }
    let file = join_relative(root, path);
    let filename = file.display().to_string();
    let env = child_env(environment);
    let work = async {
        match request["language"].as_str() {
            Some("python") => filter(&[Tools::require(tools.ruff(), Kit::Python)?.display().to_string(), "format".into(), "--isolated".into(), "--stdin-filename".into(), filename, "-".into()], root, &env, text).await,
            Some(language @ ("javascript" | "typescript")) => script(&Tools::require(tools.tsc(), Kit::Web)?, root, &file, language, &text.replace("\r\n", "\n"), &env).await,
            Some("c" | "cpp") => {
                let clang = resolve_executable("clang-format", Some(&env)).ok_or_else(|| crate::process::executable_missing("clang-format"))?;
                filter(&[clang.display().to_string(), "--style=LLVM".into(), format!("--assume-filename={filename}")], root, &env, text).await
            }
            _ => Err(HostError::invalid("Invalid formatter request")),
        }
    };
    let formatted = tokio::time::timeout(TIMEOUT, work)
        .await
        .map_err(|_| HostError::new("aborted", l("代码格式化超时。", "Formatting timed out.")))??;
    // A CRLF document stays CRLF.
    let crlf = text.contains("\r\n") && !text.replace("\r\n", "").contains('\n');
    Ok(json!({ "text": if crlf { formatted.replace("\r\n", "\n").replace('\n', "\r\n") } else { formatted } }))
}

struct FormatterProcessGuard {
    #[cfg_attr(not(unix), allow(dead_code))]
    pid: Option<u32>,
    job: Option<ProcessJob>,
    armed: bool,
}

impl Drop for FormatterProcessGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        if let Some(job) = self.job.take() {
            job.terminate();
        }
        #[cfg(unix)]
        if let Some(pid) = self.pid
            && pid > 1
            && pid <= i32::MAX as u32
        {
            // SAFETY: child leads its own process group (`process_group(0)` in `crate::process::command`).
            unsafe {
                let _ = libc::kill(-(pid as i32), libc::SIGKILL);
            }
        }
    }
}

/// Run a formatter that reads the text on standard input and writes the result on standard output.
async fn filter(argv: &[String], root: &Path, env: &BTreeMap<std::ffi::OsString, std::ffi::OsString>, text: &str) -> HostResult<String> {
    let mut command = crate::process::command(&argv[0], &argv[1..], root, env);
    command.env("PYTHONDONTWRITEBYTECODE", "1").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    let mut child = command.spawn().map_err(|error| HostError::new("io-error", format!("{}: {error}", argv[0])))?;
    let job = ProcessJob::new();
    if !job.as_ref().is_some_and(|job| job.assign_child(&child)) {
        contain_child(&child);
    }
    let mut guard = FormatterProcessGuard { pid: child.id(), job, armed: true };
    let mut stdin = child.stdin.take().expect("piped");
    let input = text.to_owned();
    let writing = tokio::spawn(async move {
        let _ = stdin.write_all(input.as_bytes()).await;
        let _ = stdin.shutdown().await;
    });
    let (stdout, stderr) = (child.stdout.take().expect("piped"), child.stderr.take().expect("piped"));
    let (out, err, status) = tokio::join!(read_limited(stdout, MAX_TEXT_BYTES + 1), read_limited(stderr, MAX_STDERR_BYTES), child.wait());
    writing.abort();
    guard.armed = false;
    let status = status?;
    if !status.success() || out.len() > MAX_TEXT_BYTES {
        let message = String::from_utf8_lossy(&err).trim().to_owned();
        return Err(HostError::new(
            "io-error",
            if message.is_empty() { l("格式化未产出完整结果。", "Formatting did not produce a complete result.") } else { message },
        ));
    }
    Ok(String::from_utf8_lossy(&out).into_owned())
}

/// Format JavaScript or TypeScript (LF text) with `tsc --lsp --stdio`: open the document, ask for its formatting edits,
/// apply them.
async fn script(tsc: &Path, root: &Path, file: &Path, language: &str, text: &str, env: &BTreeMap<std::ffi::OsString, std::ffi::OsString>) -> HostResult<String> {
    let missing = || HostError::new("not-found", l(&format!("缺少 TypeScript 语言服务器：{}", tsc.display()), &format!("TypeScript's language server is missing: {}", tsc.display())));
    if !tsc.is_file() {
        return Err(missing());
    }
    let (root_uri, uri) = match (url::Url::from_directory_path(root), url::Url::from_file_path(file)) {
        (Ok(root), Ok(file)) => (root.to_string(), file.to_string()),
        _ => return Err(HostError::invalid("Invalid formatter request")),
    };
    let mut client = LspStdioClient::spawn(tsc, &["--lsp", "--stdio"], root, env, MAX_TEXT_BYTES * 4, false).map_err(|_| missing())?;
    let unwrap_rpc = |res: Result<Value, (i64, String)>| -> HostResult<Value> {
        res.map_err(|(_, msg)| {
            HostError::new("io-error", if msg.is_empty() { l("TypeScript 格式化失败。", "The TypeScript formatter failed.") } else { msg })
        })
    };
    unwrap_rpc(client.request("initialize", json!({ "processId": std::process::id(), "rootUri": root_uri, "capabilities": {} }), |_| {}).await?)?;
    client.notify("initialized", json!({})).await?;
    let doc_lang = document_language_id(file, language);
    client
        .notify(
            "textDocument/didOpen",
            json!({ "textDocument": { "uri": uri, "languageId": doc_lang, "version": 1, "text": text } }),
        )
        .await?;
    let edits = unwrap_rpc(
        client
            .request("textDocument/formatting", json!({ "textDocument": { "uri": uri }, "options": { "tabSize": 2, "insertSpaces": true } }), |_| {})
            .await?,
    )?;
    client.shutdown().await;
    apply_edits(text, edits.as_array().map(Vec::as_slice).unwrap_or_default())
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
        .ok_or_else(|| HostError::new("io-error", l("TypeScript 格式化器返回了无效编辑。", "The TypeScript formatter returned an invalid edit.")))?;
    spans.sort_by_key(|(start, end, _)| std::cmp::Reverse((*start, *end)));
    let mut result = text.to_owned();
    let mut limit = usize::MAX;
    for (start, end, replacement) in spans {
        if start > end || end > limit {
            return Err(HostError::new("io-error", l("TypeScript 格式化器返回了重叠编辑。", "The TypeScript formatter returned overlapping edits.")));
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
