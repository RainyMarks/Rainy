//! Formatting of unsaved editor text with local tools (3.x `ide/format.ts`): Ruff for Python, Biome for JavaScript
//! and TypeScript, the installed clang-format for C and C++. The file itself is never written.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::AsyncWriteExt as _;

use super::tools::Tools;
use crate::error::{HostError, HostResult};
use crate::paths::{join_relative, relative_path};
use crate::process::{child_env, read_limited, resolve_executable};

const MAX_TEXT_BYTES: usize = 5 * 1024 * 1024;
const MAX_STDERR_BYTES: usize = 65536;
const TIMEOUT: Duration = Duration::from_secs(15);

pub async fn format(request: &Value, root: &Path, tools: &Tools, environment: &std::collections::BTreeMap<String, String>) -> HostResult<Value> {
    let path = request["path"].as_str().ok_or_else(|| HostError::invalid("Invalid formatter request"))?;
    relative_path(path, false)?;
    let text = request["text"].as_str().ok_or_else(|| HostError::invalid("Invalid formatter request"))?;
    if text.len() > MAX_TEXT_BYTES {
        return Err(HostError::new("too-large", "Editor text exceeds the formatting limit"));
    }
    let filename = join_relative(root, path).display().to_string();
    let env = child_env(environment);
    let argv: Vec<String> = match request["language"].as_str() {
        Some("python") => vec![tools.ruff().display().to_string(), "format".into(), "--isolated".into(), "--stdin-filename".into(), filename, "-".into()],
        Some("javascript" | "typescript") => vec![
            tools.biome().display().to_string(),
            "format".into(),
            format!("--stdin-file-path={filename}"),
            "--indent-style=space".into(),
            "--indent-width=2".into(),
        ],
        Some("c" | "cpp") => {
            let clang = resolve_executable("clang-format", Some(&env)).ok_or_else(|| crate::process::executable_missing("clang-format"))?;
            vec![clang.display().to_string(), "--style=LLVM".into(), format!("--assume-filename={filename}")]
        }
        _ => return Err(HostError::invalid("Invalid formatter request")),
    };
    let mut command = crate::process::command(&argv[0], &argv[1..], root, &env);
    command.env("PYTHONDONTWRITEBYTECODE", "1").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn().map_err(|error| HostError::new("io-error", format!("{}: {error}", argv[0])))?;
    let mut stdin = child.stdin.take().expect("piped");
    let input = text.to_owned();
    let writing = tokio::spawn(async move {
        let _ = stdin.write_all(input.as_bytes()).await;
        let _ = stdin.shutdown().await;
    });
    let (stdout, stderr) = (child.stdout.take().expect("piped"), child.stderr.take().expect("piped"));
    let work = async { tokio::join!(read_limited(stdout, MAX_TEXT_BYTES + 1), read_limited(stderr, MAX_STDERR_BYTES), child.wait()) };
    let (out, err, status) = match tokio::time::timeout(TIMEOUT, work).await {
        Ok(result) => result,
        Err(_) => {
            crate::process::kill_tree(&mut child, Duration::from_secs(2)).await;
            return Err(HostError::new("aborted", "Formatting timed out"));
        }
    };
    writing.abort();
    let status = status?;
    if !status.success() || out.len() > MAX_TEXT_BYTES {
        let message = String::from_utf8_lossy(&err).trim().to_owned();
        return Err(HostError::new("io-error", if message.is_empty() { "Formatting did not produce a complete result".to_owned() } else { message }));
    }
    let formatted = String::from_utf8_lossy(&out).into_owned();
    // A CRLF document stays CRLF.
    let crlf = text.contains("\r\n") && !text.replace("\r\n", "").contains('\n');
    Ok(json!({ "text": if crlf { formatted.replace("\r\n", "\n").replace('\n', "\r\n") } else { formatted } }))
}
