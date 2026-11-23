//! `symbol_outline` and `module_deps` read-only tools backed by `crate::map`.

use std::sync::Arc;

use futures::future::BoxFuture;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::{Tool, ToolContext, ToolOutput, Updates, params, spec};
use crate::agent::llm::ToolSpec;
use crate::map::ProjectMaps;

pub const OUTLINE_NAME: &str = "symbol_outline";
pub const DEPS_NAME: &str = "module_deps";
const DEFAULT_OUTLINE_LIMIT: usize = 120;
const MAX_OUTLINE_LIMIT: usize = 400;

#[derive(Deserialize)]
struct OutlineParams {
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    file_path: Option<String>,
    #[serde(default)]
    limit: Option<u64>,
}

/// Lists top-level declarations (functions, structs, classes, traits, impls, types) of a file with 1-based line numbers.
pub struct SymbolOutline {
    context: ToolContext,
    spec: ToolSpec,
}

impl SymbolOutline {
    pub fn new(context: ToolContext) -> Self {
        let spec = spec(
            OUTLINE_NAME,
            "List top-level declarations (functions, classes, structs, traits, interfaces, modules) of a source file with 1-based line numbers. Cheaper than reading a large file in full.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File path relative to the workspace or absolute." },
                    "file_path": { "type": "string", "description": "Alias for path." },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 400, "description": "Maximum number of declarations to return (default 120)." },
                },
            }),
        );
        Self { context, spec }
    }
}

impl Tool for SymbolOutline {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn execute(&self, _call_id: String, args: Value, _cancel: CancellationToken, _updates: Updates) -> BoxFuture<'_, Result<ToolOutput, String>> {
        Box::pin(async move {
            let params: OutlineParams = params(args)?;
            let requested = params
                .path
                .as_deref()
                .or(params.file_path.as_deref())
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .ok_or("invalid path: expected a non-empty file path")?;
            let limit = params.limit.map(|n| n.clamp(1, MAX_OUTLINE_LIMIT as u64) as usize).unwrap_or(DEFAULT_OUTLINE_LIMIT);
            let (absolute, display) = self.context.resolve(requested)?;
            let cwd = self.context.cwd.clone();
            let result = tokio::task::spawn_blocking(move || crate::map::symbol_outline(&cwd, &absolute, limit))
                .await
                .map_err(|error| error.to_string())?
                .map_err(|error| error.message)?;
            let lang = if result.language.is_empty() { "text" } else { &result.language };
            let text = if result.symbols.is_empty() {
                format!("{display} ({lang}, {} lines): no top-level declarations found.", result.lines)
            } else {
                let mut lines = vec![format!("{display} ({lang}, {} lines, {} declaration(s)):", result.lines, result.symbols.len())];
                for entry in &result.symbols {
                    lines.push(format!("L{}: [{}] {}", entry.line, entry.kind, entry.signature));
                }
                if result.truncated {
                    lines.push(format!("... (truncated at limit {limit})"));
                }
                lines.join("\n")
            };
            Ok(ToolOutput::text(
                self.context.limit_output(OUTLINE_NAME, text),
                json!({
                    "path": display,
                    "language": result.language,
                    "lines": result.lines,
                    "truncated": result.truncated,
                    "symbols": result.symbols,
                }),
            ))
        })
    }
}

#[derive(Deserialize)]
struct DepsParams {
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    file_path: Option<String>,
}

/// Queries ProjectMap import dependencies for one file or directory prefix.
pub struct ModuleDeps {
    context: ToolContext,
    maps: Arc<ProjectMaps>,
    spec: ToolSpec,
}

impl ModuleDeps {
    pub fn new(context: ToolContext, maps: Arc<ProjectMaps>) -> Self {
        let spec = spec(
            DEPS_NAME,
            "Inspect module dependencies using the Host's ProjectMap index: what internal files a file or directory imports, what files import it, and which external packages it uses.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File or directory path relative to the workspace (or absolute path inside it)." },
                    "file_path": { "type": "string", "description": "Alias for path." },
                },
            }),
        );
        Self { context, maps, spec }
    }
}

impl Tool for ModuleDeps {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn execute(&self, _call_id: String, args: Value, cancel: CancellationToken, _updates: Updates) -> BoxFuture<'_, Result<ToolOutput, String>> {
        Box::pin(async move {
            let params: DepsParams = params(args)?;
            let requested = params
                .path
                .as_deref()
                .or(params.file_path.as_deref())
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .ok_or("invalid path: expected a non-empty file or directory path")?;
            let (absolute, display) = self.context.resolve(requested)?;
            let target = crate::paths::relative_to(&self.context.cwd, &absolute).unwrap_or_else(|| display.clone());
            let deps = tokio::select! {
                res = self.maps.module_deps_in_dir(&self.context.cwd, &target, self.context.workspace_id.as_deref()) => res.map_err(|error| error.message)?,
                () = cancel.cancelled() => return Err("tool call aborted".into()),
            };
            if !deps.found {
                return Ok(ToolOutput::text(
                    format!("No indexed module or file found for \"{display}\"."),
                    json!({ "target": display, "found": false }),
                ));
            }
            let mut lines = vec![format!(
                "{} ({}, {} lines)",
                deps.target,
                if deps.language.is_empty() { "unknown" } else { &deps.language },
                deps.lines
            )];
            let push_list = |lines: &mut Vec<String>, label: &str, items: &[String]| {
                if items.is_empty() {
                    lines.push(format!("{label}: (none)"));
                } else {
                    lines.push(format!("{label} ({}):", items.len()));
                    for item in items {
                        lines.push(format!("  - {item}"));
                    }
                }
            };
            push_list(&mut lines, "Imports", &deps.imports);
            push_list(&mut lines, "Imported by", &deps.imported_by);
            push_list(&mut lines, "External packages", &deps.external_packages);
            Ok(ToolOutput::text(
                self.context.limit_output(DEPS_NAME, lines.join("\n")),
                serde_json::to_value(&deps).unwrap_or_else(|_| json!({ "target": deps.target, "found": true })),
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn outlines_symbols_and_queries_module_deps() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/util.ts"), "export function helper(x: number) { return x + 1; }\n").unwrap();
        std::fs::write(
            dir.path().join("src/main.ts"),
            "import { helper } from './util';\nimport axios from 'axios';\n\nexport class App {}\nexport function run() { return helper(1); }\n",
        )
        .unwrap();
        let ctx = super::super::test_context(dir.path());
        let outline = SymbolOutline::new(ctx.clone());
        let out = outline.execute("c1".into(), json!({ "path": "src/main.ts" }), CancellationToken::new(), super::super::no_updates()).await.unwrap();
        let [rainy_protocol::chat::ResultContent::Text { text }] = out.content.as_slice() else { panic!() };
        assert!(text.contains("App") && text.contains("run"), "unexpected outline: {text}");

        let projects = Arc::new(crate::projects::Projects::load(dir.path().join(".proj.json")).unwrap());
        let maps = Arc::new(ProjectMaps::new(projects, dir.path().join(".maps")));
        let deps = ModuleDeps::new(ctx, maps);
        let out = deps.execute("c2".into(), json!({ "path": "src/main.ts" }), CancellationToken::new(), super::super::no_updates()).await.unwrap();
        let [rainy_protocol::chat::ResultContent::Text { text }] = out.content.as_slice() else { panic!() };
        assert!(text.contains("src/util.ts") && text.contains("axios"), "unexpected deps: {text}");
    }
}
