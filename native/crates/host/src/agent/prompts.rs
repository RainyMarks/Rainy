//! The prompts page (`prompts.*`): the built-in prompt of each model group, which the user may replace, and the
//! instruction files (`AGENTS.md` and its kin) a chat reads.

use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::{Value, json};

use super::instructions::{BUDGET_BYTES, FILES, directories_between, project_root};
use super::profile::PROMPT_GROUPS;
use super::prompt::default_builtin;
use crate::error::{HostError, HostResult};
use crate::locale::l;
use crate::settings::{BUILTIN_PROMPT_MAX_CHARS, Settings};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusParams {
    /// The project whose instruction files to list.
    #[serde(default)]
    pub workspace_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BuiltinParams {
    pub group: String,
    /// Empty, or Rainy's own text, goes back to Rainy's.
    pub text: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveParams {
    #[serde(default)]
    pub workspace_id: Option<String>,
    pub path: String,
    pub text: String,
    /// The file as the page loaded it; `None` when it did not exist.
    #[serde(default)]
    pub expected: Option<String>,
}

/// Each group's own built-in prompt (empty while Rainy's is used) and Rainy's.
pub fn builtin_status(settings: &Settings) -> Value {
    let data = settings.get();
    let groups: Vec<Value> = PROMPT_GROUPS
        .iter()
        .map(|group| {
            json!({
                "id": group,
                "custom": data.builtin_prompts.get(*group).cloned().unwrap_or_default(),
                "defaultText": default_builtin(group).unwrap_or_default(),
            })
        })
        .collect();
    json!({ "maxChars": BUILTIN_PROMPT_MAX_CHARS, "groups": groups })
}

/// Save a group's own built-in prompt; empty, or Rainy's text unchanged, removes it.
pub fn set_builtin(settings: &Settings, params: &BuiltinParams) -> HostResult<Value> {
    let group = params.group.as_str();
    let Some(default) = default_builtin(group) else {
        return Err(HostError::invalid(l("没有这一组模型。", "There is no such model group.")));
    };
    let text = params.text.trim();
    if text.chars().count() > BUILTIN_PROMPT_MAX_CHARS {
        return Err(HostError::new(
            "too-large",
            l(&format!("内置提示词最多 {BUILTIN_PROMPT_MAX_CHARS} 个字符。"), &format!("A built-in prompt can be at most {BUILTIN_PROMPT_MAX_CHARS} characters.")),
        ));
    }
    let custom = (!text.is_empty() && text != default.trim()).then(|| text.to_owned());
    settings.update(|data| {
        match custom {
            Some(text) => data.builtin_prompts.insert(group.to_owned(), text),
            None => data.builtin_prompts.remove(group),
        };
        Ok(())
    })?;
    Ok(builtin_status(settings))
}

/// One instruction file the page offers.
struct Candidate {
    path: PathBuf,
    /// As chats name it: `$RAINY_HOME/AGENTS.md`, or relative to the project root.
    label: String,
    global: bool,
}

/// The files a chat started in `cwd` reads, in the order it reads them: the Host home's `AGENTS.md`, then the
/// project's from its root down to `cwd`. Every name is offered in `cwd`, where a new one goes; folders above it list
/// the files they have.
fn candidates(home: &Path, cwd: Option<&Path>) -> Vec<Candidate> {
    let mut list = vec![Candidate { path: home.join("AGENTS.md"), label: "$RAINY_HOME/AGENTS.md".into(), global: true }];
    let Some(cwd) = cwd else { return list };
    let root = project_root(cwd);
    let directories = directories_between(&root, cwd);
    for (index, directory) in directories.iter().enumerate() {
        let own = index + 1 == directories.len();
        for name in FILES {
            let path = directory.join(name);
            if own || path.is_file() {
                let label = crate::paths::relative_to(&root, &path).filter(|relative| !relative.is_empty()).unwrap_or_else(|| name.to_owned());
                list.push(Candidate { path, label, global: false });
            }
        }
    }
    list
}

fn read_text(path: &Path) -> Option<String> {
    std::fs::read(path).ok().map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
}

/// The instruction files of the project in `cwd` (none without a project) and the Host's own.
pub fn instructions_status(home: &Path, cwd: Option<&Path>) -> Value {
    let files: Vec<Value> = candidates(home, cwd)
        .into_iter()
        .map(|candidate| {
            let size = std::fs::metadata(&candidate.path).ok().filter(|info| info.is_file()).map(|info| info.len());
            // A file over the budget is refused when a chat starts; its text is not sent here either.
            let text = size.filter(|size| *size <= BUDGET_BYTES as u64).and_then(|_| read_text(&candidate.path));
            json!({
                "path": candidate.path.to_string_lossy(),
                "label": candidate.label,
                "global": candidate.global,
                "exists": size.is_some(),
                "bytes": size,
                "text": text,
            })
        })
        .collect();
    json!({ "budgetBytes": BUDGET_BYTES, "root": cwd.map(|cwd| project_root(cwd).to_string_lossy().into_owned()), "files": files })
}

/// Save one of the files [`instructions_status`] lists, unless it changed since the page read it.
pub fn save_instructions(home: &Path, cwd: Option<&Path>, params: &SaveParams) -> HostResult<Value> {
    let path = PathBuf::from(&params.path);
    if !candidates(home, cwd).iter().any(|candidate| candidate.path == path) {
        return Err(HostError::invalid(l("只能保存这里列出的说明文件。", "Only the instruction files listed here can be saved.")));
    }
    if params.text.len() > BUDGET_BYTES {
        return Err(HostError::new(
            "too-large",
            l(&format!("说明文件最多 {BUDGET_BYTES} 字节。"), &format!("An instruction file can be at most {BUDGET_BYTES} bytes.")),
        ));
    }
    let current = read_text(&path);
    if current != params.expected {
        return Err(HostError::new("changed", l("这个文件在别处被改过了，请重新载入后再改。", "This file changed elsewhere; reload it before editing.")));
    }
    if current.is_none() && params.text.trim().is_empty() {
        return Ok(json!({ "saved": false }));
    }
    let mut text = params.text.clone();
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    let failed = |error: std::io::Error| {
        HostError::new("io", l(&format!("无法保存 {}：{error}", path.display()), &format!("{} cannot be saved: {error}", path.display())))
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(failed)?;
    }
    std::fs::write(&path, text).map_err(failed)?;
    Ok(json!({ "saved": true }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offers_the_files_a_chat_reads_and_saves_only_those() {
        let base = std::env::temp_dir().join(format!("rainy-prompts-{}", uuid::Uuid::new_v4()));
        let (home, root) = (base.join("home"), base.join("repo"));
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::create_dir_all(root.join("app")).unwrap();
        std::fs::write(root.join("CLAUDE.md"), "root rules").unwrap();
        let cwd = root.join("app");
        let status = instructions_status(&home, Some(&cwd));
        let labels: Vec<&str> = status["files"].as_array().unwrap().iter().map(|file| file["label"].as_str().unwrap()).collect();
        assert_eq!(labels, ["$RAINY_HOME/AGENTS.md", "CLAUDE.md", "app/AGENTS.md", "app/CLAUDE.md", "app/AGENTS.local.md", "app/CLAUDE.local.md"]);
        assert_eq!(status["files"][1]["text"], "root rules");
        assert_eq!(status["files"][2]["exists"], false);
        // A new file, then an edit that started from what was read.
        let agents = cwd.join("AGENTS.md").to_string_lossy().into_owned();
        let save = |path: &str, text: &str, expected: Option<&str>| {
            save_instructions(&home, Some(&cwd), &SaveParams { workspace_id: None, path: path.into(), text: text.into(), expected: expected.map(str::to_owned) })
        };
        assert_eq!(save(&agents, "Run the tests.", None).unwrap()["saved"], true);
        assert_eq!(std::fs::read_to_string(&agents).unwrap(), "Run the tests.\n");
        assert_eq!(save(&agents, "Old view", None).unwrap_err().code, "changed");
        save(&agents, "Run the tests first.", Some("Run the tests.\n")).unwrap();
        // The Host's own, created with its folder; files that are not listed are refused.
        let global = home.join("AGENTS.md").to_string_lossy().into_owned();
        save(&global, "Answer in Chinese.", None).unwrap();
        assert!(home.join("AGENTS.md").is_file());
        assert!(save(&root.join("README.md").to_string_lossy(), "x", None).is_err());
        // Nothing to save creates nothing.
        let local = cwd.join("AGENTS.local.md").to_string_lossy().into_owned();
        assert_eq!(save(&local, "  ", None).unwrap()["saved"], false);
        assert!(!cwd.join("AGENTS.local.md").exists());
        let _ = std::fs::remove_dir_all(base);
    }
}
