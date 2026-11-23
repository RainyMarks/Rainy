//! `lsp_diagnostics` read-only tool and post-edit error summary helper backed by `ide::language::Languages`.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures::future::BoxFuture;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::{Tool, ToolContext, ToolOutput, Updates, params, spec};
use crate::agent::llm::ToolSpec;
use crate::ide::language::{DiagnosticItem, Languages, language_for_path};

pub const NAME: &str = "lsp_diagnostics";
const MAX_TOOL_DIAGNOSTICS: usize = 100;
const MAX_POST_EDIT_FILES: usize = 5;
const MAX_POST_EDIT_ERRORS_PER_FILE: usize = 15;
const POST_EDIT_TOTAL_TIMEOUT: Duration = Duration::from_secs(6);

#[derive(Deserialize)]
struct Params {
    #[serde(default)]
    file_path: Option<String>,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    severity: Option<String>,
}

pub struct LspDiagnostics {
    context: ToolContext,
    languages: Arc<Languages>,
    spec: ToolSpec,
}

impl LspDiagnostics {
    pub fn new(context: ToolContext, languages: Arc<Languages>) -> Self {
        let spec = spec(
            NAME,
            "Query syntax, type, and semantic diagnostics for a source file from the Host's shared language server pool (ty for Python, tsc for JS/TS, clangd for C/C++). Use this to verify changes quickly without running a full build.",
            json!({
                "type": "object",
                "properties": {
                    "file_path": { "type": "string", "description": "Path to the file to check, relative to the workspace or absolute." },
                    "path": { "type": "string", "description": "Alias for file_path." },
                    "severity": { "type": "string", "enum": ["all", "error", "warning"], "description": "Filter by minimum severity: 'error' (errors only), 'warning' (errors and warnings), or 'all' (default)." },
                },
            }),
        );
        Self { context, languages, spec }
    }
}

fn severity_matches(item: &DiagnosticItem, filter: &str) -> bool {
    match filter {
        "error" => item.severity == "error",
        "warning" => matches!(item.severity.as_str(), "error" | "warning"),
        _ => true,
    }
}

fn format_item(item: &DiagnosticItem) -> String {
    let mut line = format!("L{}:{} [{}] {}", item.line, item.column, item.severity, item.message);
    if let Some(code) = &item.code {
        line.push_str(&format!(" ({code})"));
    }
    if let Some(source) = &item.source {
        line.push_str(&format!(" [{source}]"));
    }
    line
}

impl Tool for LspDiagnostics {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn execute(&self, _call_id: String, args: Value, cancel: CancellationToken, _updates: Updates) -> BoxFuture<'_, Result<ToolOutput, String>> {
        Box::pin(async move {
            let params: Params = params(args)?;
            let requested = params
                .file_path
                .as_deref()
                .or(params.path.as_deref())
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .ok_or("invalid file_path: expected a non-empty file path")?;
            let filter = params.severity.as_deref().unwrap_or("all");
            if !matches!(filter, "all" | "error" | "warning") {
                return Err("invalid severity: expected 'all', 'error', or 'warning'".into());
            }
            let (absolute, display) = self.context.resolve(requested)?;
            let report = tokio::select! {
                res = self.languages.query_diagnostics_in_dir(&self.context.cwd, &absolute, self.context.workspace_id.as_deref()) => res.map_err(|error| error.message)?,
                () = cancel.cancelled() => return Err("tool call aborted".into()),
            };
            if !report.available {
                let reason = report.unavailable_reason.unwrap_or_else(|| "Language server diagnostics are unavailable for this file.".into());
                return Ok(ToolOutput::text(
                    format!("{display}: diagnostics unavailable ({reason})"),
                    json!({ "path": display, "language": report.language, "available": false, "reason": reason, "diagnostics": [] }),
                ));
            }
            let filtered: Vec<&DiagnosticItem> = report.diagnostics.iter().filter(|item| severity_matches(item, filter)).collect();
            let total = filtered.len();
            let shown: Vec<&DiagnosticItem> = filtered.into_iter().take(MAX_TOOL_DIAGNOSTICS).collect();
            let lang = report.language.as_deref().unwrap_or("unknown");
            let text = if shown.is_empty() {
                format!("{display} ({lang}): no diagnostics.")
            } else {
                let mut lines = vec![format!("{display} ({lang}): {total} diagnostic(s)")];
                lines.extend(shown.iter().map(|item| format_item(item)));
                if total > shown.len() {
                    lines.push(format!("... ({} more omitted)", total - shown.len()));
                }
                lines.join("\n")
            };
            Ok(ToolOutput::text(
                self.context.limit_output(NAME, text),
                json!({ "path": display, "language": report.language, "available": true, "total": total, "diagnostics": shown }),
            ))
        })
    }
}

/// Inspect newly edited files for Error-level LSP diagnostics and return a concise summary block (or `None` when
/// no language server is available or no errors exist). Never fails the edit call.
pub async fn post_edit_error_summary(context: &ToolContext, edited_files: &[PathBuf]) -> Option<(String, Vec<Value>)> {
    let languages = context.languages.as_ref()?;
    let candidates: Vec<&PathBuf> = edited_files
        .iter()
        .filter(|path| path.is_file() && language_for_path(path).is_some())
        .take(MAX_POST_EDIT_FILES)
        .collect();
    if candidates.is_empty() {
        return None;
    }
    let work = async {
        let mut lines = Vec::new();
        let mut details = Vec::new();
        for file in candidates {
            let Ok(report) = languages.query_diagnostics_in_dir(&context.cwd, file, context.workspace_id.as_deref()).await else {
                continue;
            };
            if !report.available {
                continue;
            }
            let errors: Vec<&DiagnosticItem> = report.diagnostics.iter().filter(|d| d.severity == "error").collect();
            if errors.is_empty() {
                continue;
            }
            let display = context.display(file);
            for item in errors.iter().take(MAX_POST_EDIT_ERRORS_PER_FILE) {
                lines.push(format!("{display}:{}", format_item(item)));
            }
            if errors.len() > MAX_POST_EDIT_ERRORS_PER_FILE {
                lines.push(format!("{display}: ... ({} more error(s))", errors.len() - MAX_POST_EDIT_ERRORS_PER_FILE));
            }
            details.push(json!({ "path": display, "errors": errors.iter().take(MAX_POST_EDIT_ERRORS_PER_FILE).collect::<Vec<_>>() }));
        }
        (lines, details)
    };
    let (lines, details) = tokio::time::timeout(POST_EDIT_TOTAL_TIMEOUT, work).await.ok()?;
    if lines.is_empty() {
        return None;
    }
    Some((format!("\n\n[LSP diagnostics (errors)]\n{}", lines.join("\n")), details))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_and_filters_diagnostic_items() {
        let err = DiagnosticItem { line: 10, column: 3, severity: "error".into(), message: "type mismatch".into(), code: Some("E0308".into()), source: Some("ty".into()) };
        let warn = DiagnosticItem { line: 12, column: 1, severity: "warning".into(), message: "unused var".into(), code: None, source: None };
        let info = DiagnosticItem { line: 15, column: 1, severity: "info".into(), message: "note".into(), code: None, source: None };
        assert!(severity_matches(&err, "error") && !severity_matches(&warn, "error"));
        assert!(severity_matches(&warn, "warning") && !severity_matches(&info, "warning"));
        assert!(severity_matches(&info, "all"));
        assert_eq!(format_item(&err), "L10:3 [error] type mismatch (E0308) [ty]");
        assert_eq!(format_item(&warn), "L12:1 [warning] unused var");
    }
}
