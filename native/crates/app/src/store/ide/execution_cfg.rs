//! Run profiles, target resolution, and breakpoints stored inside workspace state.

use gpui_kit::*;
use serde_json::{Value, json};

use super::keys::split_key;
use super::{Ide, IdeEvent};

/// Why Run starts what it starts (3.x `RunTarget.mode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunMode {
    /// A profile chosen explicitly.
    Pinned,
    /// The open file's remembered settings.
    File,
    /// The open file by its extension.
    Auto,
}

/// The languages Run and Debug take, in menu order, with their names (3.x `runLanguages`, `runLanguageNames`).
pub const RUN_LANGUAGES: [(&str, &str); 6] =
    [("python", "Python"), ("javascript", "JavaScript (Node.js)"), ("typescript", "TypeScript"), ("php", "PHP"), ("c", "C"), ("cpp", "C++")];

/// The run language a file's extension implies.
pub fn run_language(key: &str) -> Option<&'static str> {
    let extension = key.rsplit('.').next().unwrap_or_default().to_ascii_lowercase();
    Some(match extension.as_str() {
        "py" | "pyi" => "python",
        "js" | "mjs" | "cjs" | "jsx" => "javascript",
        "ts" | "tsx" => "typescript",
        "php" => "php",
        "c" | "h" => "c",
        "cc" | "cpp" | "cxx" | "hpp" => "cpp",
        _ => return None,
    })
}

/// Whether a profile runs the file stored as `place` (3.x `runsFile`).
pub(super) fn runs_file(profile: &Value, place: &Value) -> bool {
    profile["program"] == place["path"] && profile.get("rootId").filter(|root| !root.is_null()) == place.get("rootId")
}

impl Ide {
    /// Borrow the raw `execution` JSON object in workspace state without cloning.
    pub fn execution_ref(&self) -> Option<&Value> {
        self.state.as_ref()?.data.get("execution").filter(|value| value.is_object())
    }

    /// The Python of the active run profile, or of the first Python profile with one (`selectedPythonExecutable`).
    pub fn python_path(&self) -> Option<String> {
        let execution = self.execution_ref()?;
        let profiles = execution.get("profiles")?.as_array()?;
        let active = execution.get("activeProfile").and_then(Value::as_str);
        let python = |profile: &&Value| profile.get("language").and_then(Value::as_str) == Some("python");
        let executable = |profile: &Value| profile.get("executable").and_then(Value::as_str).map(str::to_owned);
        match profiles.iter().filter(python).find(|profile| profile.get("name").and_then(Value::as_str) == active) {
            Some(profile) => executable(profile),
            None => profiles.iter().filter(python).find_map(executable),
        }
    }

    /// The project's run settings as 3.x stores them (`IdeExecutionConfiguration`): profiles, the pinned one, breakpoints
    /// and watches.
    pub fn execution(&self) -> Value {
        let mut execution = self.execution_ref().cloned().unwrap_or_else(|| json!({}));
        for (field, empty) in [("profiles", json!([])), ("activeProfile", Value::Null), ("breakpoints", json!([])), ("watches", json!([]))] {
            if execution.get(field).is_none() {
                execution[field] = empty;
            }
        }
        execution
    }

    pub fn set_execution(&mut self, execution: Value, cx: &mut Context<Self>) {
        let Some(state) = self.state.as_mut() else { return };
        state.data["execution"] = execution;
        cx.emit(IdeEvent::ExecutionChanged);
        self.schedule_state_save(cx);
        cx.notify();
    }

    /// The one-based lines with a breakpoint in a file (borrows state without cloning `execution`).
    pub fn breakpoints(&self, key: &str) -> Vec<u32> {
        let Some(execution) = self.execution_ref() else { return Vec::new() };
        execution
            .get("breakpoints")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .find(|source| source["path"].as_str().is_some_and(|path| self.key_of_stored(path, source["rootId"].as_str()) == key))
            .and_then(|source| source["lines"].as_array())
            .map(|lines| lines.iter().filter_map(Value::as_u64).map(|line| line as u32).collect())
            .unwrap_or_default()
    }

    /// Set or clear the breakpoint on a one-based line; returns the file's breakpoints as a debug session takes them.
    pub fn toggle_breakpoint(&mut self, key: &str, line: u32, cx: &mut Context<Self>) -> Value {
        let mut lines = self.breakpoints(key);
        match lines.iter().position(|known| *known == line) {
            Some(index) => drop(lines.remove(index)),
            None => lines.push(line),
        }
        lines.sort_unstable();
        let mut source = Self::stored(key);
        source["lines"] = json!(lines);
        let mut execution = self.execution();
        let mut sources: Vec<Value> = execution["breakpoints"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|known| known["path"].as_str().is_none_or(|path| self.key_of_stored(path, known["rootId"].as_str()) != key))
            .cloned()
            .collect();
        if !lines.is_empty() {
            sources.push(source.clone());
        }
        execution["breakpoints"] = json!(sources);
        self.set_execution(execution, cx);
        source
    }

    /// What Run and Debug start for a file (3.x `runTarget`): borrows state to avoid deep-cloning `execution` per frame.
    pub fn run_target(&self, key: Option<&str>) -> Option<(Value, RunMode)> {
        let profiles = self.execution_ref().and_then(|exec| exec.get("profiles")).and_then(Value::as_array);
        let active_profile = self.execution_ref().and_then(|exec| exec.get("activeProfile")).and_then(Value::as_str);
        if let (Some(name), Some(profiles)) = (active_profile, profiles)
            && let Some(pinned) = profiles.iter().find(|profile| profile["name"] == name)
        {
            return Some((pinned.clone(), RunMode::Pinned));
        }
        let key = key?;
        let place = Self::stored(key);
        if let Some(profiles) = profiles
            && let Some(saved) = profiles.iter().find(|profile| runs_file(profile, &place))
        {
            return Some((saved.clone(), RunMode::File));
        }
        let language = run_language(key)?;
        let mut configuration = json!({ "name": split_key(key).1, "language": language, "program": place["path"], "terminal": true });
        if let Some(root) = place.get("rootId") {
            configuration["rootId"] = root.clone();
        }
        Some((configuration, RunMode::Auto))
    }

    /// Run a file with a chosen language, or by its extension again (`None`), and unpin any profile.
    pub fn choose_file_language(&mut self, key: &str, choice: Option<&str>, cx: &mut Context<Self>) {
        let mut execution = self.execution();
        let place = Self::stored(key);
        let profiles = execution["profiles"].as_array().cloned().unwrap_or_default();
        let saved = profiles.iter().find(|profile| runs_file(profile, &place)).cloned();
        let mut others: Vec<Value> = profiles.into_iter().filter(|profile| Some(profile) != saved.as_ref()).collect();
        execution["activeProfile"] = Value::Null;
        let Some(language) = choice.or_else(|| run_language(key)) else {
            self.set_execution(execution, cx);
            return;
        };
        let label = split_key(key).1.to_owned();
        let mut next = saved.clone().unwrap_or_else(|| {
            let mut name = label.clone();
            let mut suffix = 2;
            while others.iter().any(|profile| profile["name"] == name.as_str()) {
                name = format!("{label} ({suffix})");
                suffix += 1;
            }
            let mut profile = json!({ "name": name, "language": language, "program": place["path"], "terminal": true });
            if let Some(root) = place.get("rootId") {
                profile["rootId"] = root.clone();
            }
            profile
        });
        next["language"] = json!(language);
        if let Some(fields) = next.as_object_mut() {
            if language != "c" && language != "cpp" {
                fields.remove("build");
            }
            if language != "python" {
                fields.remove("pythonModule");
            }
        }
        let plain = ["arguments", "environment"].iter().all(|field| next[*field].as_array().is_none_or(Vec::is_empty) && next[*field].as_object().is_none_or(|map| map.is_empty()))
            && ["executable", "build", "cwd", "pythonModule"].iter().all(|field| next.get(*field).is_none_or(Value::is_null));
        if !(choice.is_none() && plain) {
            others.push(next);
        }
        execution["profiles"] = json!(others);
        self.set_execution(execution, cx);
    }

    /// Pin a profile, or run the open file again (`None`) (3.x `pinRunProfile`).
    pub fn pin_profile(&mut self, name: Option<String>, cx: &mut Context<Self>) {
        let mut execution = self.execution();
        execution["activeProfile"] = name.map_or(Value::Null, Value::String);
        self.set_execution(execution, cx);
    }

    /// Save an edited profile in place of `replacing` (3.x `RunConfigurationDialog` save).
    pub fn save_profile(&mut self, profile: Value, replacing: Option<&str>, cx: &mut Context<Self>) -> Result<(), String> {
        let mut execution = self.execution();
        let name = profile["name"].as_str().unwrap_or_default().to_owned();
        let profiles = execution["profiles"].as_array().cloned().unwrap_or_default();
        let renamed = replacing.is_some_and(|old| old != name);
        if renamed && profiles.iter().any(|known| known["name"] == name.as_str()) {
            return Err(name);
        }
        let old = if renamed { replacing.unwrap_or_default() } else { name.as_str() };
        let replaced = profiles.iter().position(|known| known["name"] == old);
        let mut kept: Vec<Value> = profiles.into_iter().filter(|known| known["name"] != name.as_str() && Some(known["name"].as_str().unwrap_or_default()) != replacing).collect();
        let at = replaced.unwrap_or(kept.len()).min(kept.len());
        let open = self.active.as_deref().map(Self::stored);
        let pinned = open.as_ref().is_none_or(|place| !runs_file(&profile, place));
        kept.insert(at, profile);
        execution["profiles"] = json!(kept);
        execution["activeProfile"] = if pinned { json!(name) } else { Value::Null };
        self.set_execution(execution, cx);
        Ok(())
    }

    /// The breakpoints a debug session of a folder starts with.
    pub fn breakpoints_for_root(&self, root: Option<&str>) -> Value {
        let Some(execution) = self.execution_ref() else { return json!([]) };
        json!(
            execution
                .get("breakpoints")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter(|source| source["rootId"].as_str() == root)
                .cloned()
                .collect::<Vec<_>>()
        )
    }
}
