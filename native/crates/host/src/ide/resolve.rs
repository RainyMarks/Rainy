//! Turn a run configuration into explicit commands (3.x `ide/execution-resolve.ts`, `execution-cmake.ts`): every path
//! stays inside the project, no shell interprets arguments, and nothing starts or is created here.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use crate::error::{HostError, HostResult};
use crate::locale::l;
use crate::paths::{contains, normalize, realpath};
use crate::process::{child_env, executable_missing, resolve_executable};
use crate::runtime::Resolved;

/// A command with its argv, folder and environment overlay; no shell is involved.
#[derive(Debug, Clone, PartialEq)]
pub struct CommandSpec {
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    pub environment: BTreeMap<String, String>,
}

impl CommandSpec {
    pub fn to_json(&self) -> Value {
        json!({ "argv": self.argv, "cwd": self.cwd, "environment": self.environment })
    }
}

/// A resolved launch: the build commands, the program, and the build folder a single-file C/C++ build owns.
#[derive(Debug, Clone)]
pub struct ResolvedRun {
    pub configuration: Value,
    pub workspace_id: String,
    pub root: PathBuf,
    pub language: String,
    pub program: PathBuf,
    pub name: String,
    pub launch: CommandSpec,
    pub build: Vec<CommandSpec>,
    pub build_directory: Option<PathBuf>,
    pub owned_build_directory: Option<PathBuf>,
    pub terminal: bool,
}

impl ResolvedRun {
    /// The page's `IdeResolvedRunSpec`.
    pub fn spec(&self) -> Value {
        let mut spec = json!({
            "workspaceId": self.workspace_id, "workspaceRoot": self.root, "language": self.language, "program": self.program,
            "name": self.name, "launch": self.launch.to_json(), "build": self.build.iter().map(CommandSpec::to_json).collect::<Vec<_>>(),
            "terminal": self.terminal,
        });
        if let Some(root) = self.configuration.get("rootId").filter(|root| root.is_string()) {
            spec["rootId"] = root.clone();
        }
        if let Some(directory) = &self.build_directory {
            spec["buildDirectory"] = json!(directory);
        }
        spec
    }

    pub fn arguments(&self) -> Vec<String> {
        strings(&self.configuration["arguments"])
    }
}

fn strings(value: &Value) -> Vec<String> {
    value.as_array().map(|items| items.iter().filter_map(|item| item.as_str().map(str::to_owned)).collect()).unwrap_or_default()
}

fn error(message: impl Into<String>) -> HostError {
    HostError::new("execution-error", message)
}

fn env_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|first| first.is_ascii_alphabetic() || first == '_') && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Check one run configuration as 3.x's schema did: known fields, a language, paths without NUL, `RAINY_` names
/// reserved, build settings only for C/C++ and module names only for Python.
pub fn check_run_configuration(value: &Value) -> HostResult<()> {
    let invalid = |message: &str| Err(HostError::invalid(message));
    let Some(object) = value.as_object() else { return invalid("A run configuration must be an object.") };
    const FIELDS: [&str; 11] = ["name", "language", "rootId", "program", "pythonModule", "cwd", "arguments", "environment", "executable", "build", "terminal"];
    if object.keys().any(|key| !FIELDS.contains(&key.as_str())) {
        return invalid("A run configuration has an unknown field.");
    }
    let text = |key: &str, required: bool| match object.get(key) {
        None | Some(Value::Null) => !required,
        Some(Value::String(text)) => !text.contains('\0') && !text.is_empty(),
        _ => false,
    };
    if !text("name", true) || !text("program", true) || !text("rootId", false) || !text("cwd", false) || !text("executable", false) || !text("pythonModule", false) {
        return invalid("A run configuration has an invalid field.");
    }
    let language = object.get("language").and_then(Value::as_str).unwrap_or_default();
    if !["python", "javascript", "typescript", "php", "c", "cpp"].contains(&language) {
        return invalid("Unknown run language.");
    }
    if let Some(module) = object.get("pythonModule").and_then(Value::as_str) {
        let valid = module.split('.').all(|part| part.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_') && part.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'));
        if !valid || language != "python" {
            return invalid("Use a dotted Python module name.");
        }
    }
    if let Some(arguments) = object.get("arguments").filter(|value| !value.is_null()) {
        if !arguments.as_array().is_some_and(|items| items.iter().all(|item| item.as_str().is_some_and(|text| !text.contains('\0')))) {
            return invalid("Arguments must be text.");
        }
    }
    if let Some(environment) = object.get("environment").filter(|value| !value.is_null()) {
        let Some(map) = environment.as_object() else { return invalid("The environment must be an object.") };
        for (name, value) in map {
            if !env_name(name) || name.to_uppercase().starts_with("RAINY_") || !value.as_str().is_some_and(|text| !text.contains('\0')) {
                return invalid("RAINY_ environment names are reserved.");
            }
        }
    }
    if object.get("terminal").is_some_and(|value| !value.is_null() && !value.is_boolean()) {
        return invalid("terminal must be true or false.");
    }
    if let Some(build) = object.get("build").filter(|value| !value.is_null()) {
        if language != "c" && language != "cpp" {
            return invalid("Build settings require C or C++.");
        }
        match build["kind"].as_str() {
            Some("single-file") => {}
            Some("cmake") if ["buildDirectory", "target", "executable"].iter().all(|key| build[key].as_str().is_some_and(|text| !text.is_empty())) => {}
            _ => return invalid("Invalid build settings."),
        }
    }
    Ok(())
}

/// Resolve a path the configuration names inside the project and refuse links leading out. `expected` names the kind
/// an existing path must have; without it the path may not exist yet (an output).
pub fn workspace_path(root: &Path, value: &str, expected: Option<&str>) -> HostResult<PathBuf> {
    if value.is_empty() || value.contains('\0') {
        return Err(error("Use a path in the selected workspace execution target."));
    }
    if cfg!(not(windows)) && (value.contains('\\') || (value.len() >= 2 && value.as_bytes()[1] == b':')) {
        return Err(error("Use a path in the selected workspace execution target."));
    }
    if cfg!(windows) && value.starts_with('/') && !value.starts_with("//") {
        return Err(error("A Linux path cannot be used in a Windows workspace."));
    }
    let candidate = normalize(&root.join(value));
    if !contains(root, &candidate) {
        return Err(error("The requested path is outside the selected workspace."));
    }
    let kind = std::fs::metadata(&candidate).ok().map(|info| if info.is_file() { "file" } else if info.is_dir() { "directory" } else { "other" });
    if let Some(kind) = kind {
        let canonical = realpath(&candidate)?;
        if !contains(root, &canonical) {
            return Err(error("The requested path resolves outside the selected workspace."));
        }
        if let Some(expected) = expected.filter(|expected| *expected != kind) {
            return Err(error(format!("The requested path must be a {expected}.")));
        }
        return Ok(canonical);
    }
    if let Some(expected) = expected {
        return Err(error(format!("The requested {expected} does not exist: {value}")));
    }
    let mut parent = candidate.parent().map(Path::to_path_buf).ok_or_else(|| error("The workspace filesystem root is unavailable."))?;
    while !parent.exists() {
        parent = parent.parent().map(Path::to_path_buf).ok_or_else(|| error("The workspace filesystem root is unavailable."))?;
    }
    if !contains(root, &realpath(&parent)?) {
        return Err(error("The output path resolves outside the selected workspace."));
    }
    Ok(candidate)
}

/// A configured absolute executable, or a default name found on the environment's PATH.
pub fn executable(configured: Option<&str>, fallback: &str, environment: &BTreeMap<String, String>) -> HostResult<String> {
    let command = configured.unwrap_or(fallback);
    if configured.is_some_and(|path| !Path::new(path).is_absolute()) {
        return Err(error("IDE execution requires an absolute native path."));
    }
    if !Path::new(command).is_absolute() && (command.contains('/') || (cfg!(windows) && command.contains('\\'))) {
        return Err(error(format!("The command {command:?} is a relative path; use an absolute path or a program name on PATH.")));
    }
    let found = resolve_executable(command, Some(&child_env(environment))).ok_or_else(|| {
        let mut missing = executable_missing(command);
        missing.code = "dependency-unavailable".into();
        missing
    })?;
    Ok(found.display().to_string())
}

/// Resolve one launch without starting anything.
pub fn resolve_run(root: &Path, workspace_id: &str, configuration: &Value, runtime: Option<&Resolved>) -> HostResult<ResolvedRun> {
    check_run_configuration(configuration)?;
    let root = realpath(root)?;
    if !root.is_dir() {
        return Err(error("The selected workspace is not a directory."));
    }
    let text = |key: &str| configuration.get(key).and_then(Value::as_str);
    let language = text("language").unwrap_or_default().to_owned();
    let cwd = workspace_path(&root, text("cwd").unwrap_or("."), Some("directory"))?;
    let mut environment = runtime.map(|runtime| runtime.environment.clone()).unwrap_or_default();
    if let Some(Value::Object(own)) = configuration.get("environment") {
        environment.extend(own.iter().filter_map(|(name, value)| Some((name.clone(), value.as_str()?.to_owned()))));
    }
    let arguments = strings(&configuration["arguments"]);
    let program = workspace_path(&root, text("program").unwrap_or_default(), Some("file"))?;
    let program_text = program.display().to_string();
    let selected = |name: &str| text("executable").map(str::to_owned).or_else(|| runtime.and_then(|runtime| runtime.executables.get(name).cloned()));
    let spec = |argv: Vec<String>| CommandSpec { argv, cwd: cwd.clone(), environment: environment.clone() };
    let mut build = Vec::new();
    let (mut build_directory, mut owned) = (None, None);
    let launch = match language.as_str() {
        "python" => {
            let python = executable(selected("python").as_deref(), if cfg!(windows) { "python.exe" } else { "python3" }, &environment)?;
            let mut argv = vec![python, "-u".into()];
            match text("pythonModule") {
                Some(module) => argv.extend(["-m".to_owned(), module.to_owned()]),
                None => argv.push(program_text.clone()),
            }
            argv.extend(arguments);
            spec(argv)
        }
        "javascript" | "typescript" => {
            // Rainy carries no Node.js: JavaScript and TypeScript run on the user's own, which strips TypeScript's types.
            let node = executable(selected("node").as_deref(), if cfg!(windows) { "node.exe" } else { "node" }, &environment).map_err(|mut missing| {
                missing.message = l(
                    "运行 JavaScript / TypeScript 需要 Node.js（TypeScript 需要 22.6 或更新）。请安装 Node.js，或在 设置 → 运行环境 中选择它。",
                    "Running JavaScript or TypeScript needs Node.js (22.6 or later for TypeScript). Install Node.js, or choose it in Settings → Runtime.",
                );
                missing
            })?;
            let mut argv = vec![node, "--enable-source-maps".to_owned()];
            if language == "typescript" {
                argv.push("--experimental-strip-types".into());
            }
            argv.push(program_text.clone());
            argv.extend(arguments);
            spec(argv)
        }
        "php" => {
            let php = executable(selected("php").as_deref(), if cfg!(windows) { "php.exe" } else { "php" }, &environment).map_err(|mut missing| {
                if text("executable").is_none() && cfg!(windows) {
                    missing.message = l(
                        "未找到 PHP。请在「设置 → 运行环境 → 可选组件」中下载 PHP，或在运行方式中指定已安装的 php.exe。",
                        "PHP was not found. Download PHP in Settings → Runtime environments → Optional components, or choose an installed php.exe in the run configuration.",
                    );
                }
                missing
            })?;
            let mut argv = vec![php, program_text.clone()];
            argv.extend(arguments);
            spec(argv)
        }
        _ => {
            let c = language == "c";
            if configuration["build"]["kind"] == "cmake" {
                let resolved = cmake(&root, configuration, selected(&language).as_deref(), &environment)?;
                build = resolved.0;
                build_directory = Some(resolved.1);
                let mut argv = vec![resolved.2.display().to_string()];
                argv.extend(arguments);
                spec(argv)
            } else {
                let compiler = executable(selected(&language).as_deref(), if c { "gcc" } else { "g++" }, &environment)?;
                let directory = workspace_path(&root, &format!(".rainy-ide/build/{}", uuid::Uuid::new_v4()), None)?;
                let output = directory.join(if cfg!(windows) { "program.exe" } else { "program" });
                let mut argv = vec![compiler, "-g".into(), "-O0".into(), "-fno-omit-frame-pointer".into()];
                argv.extend(strings(&configuration["build"]["flags"]));
                argv.extend([program_text.clone(), "-o".into(), output.display().to_string()]);
                build.push(spec(argv));
                build_directory = Some(directory.clone());
                owned = Some(directory);
                let mut argv = vec![output.display().to_string()];
                argv.extend(arguments);
                spec(argv)
            }
        }
    };
    Ok(ResolvedRun {
        configuration: configuration.clone(),
        workspace_id: workspace_id.to_owned(),
        root,
        language,
        program,
        name: text("name").unwrap_or_default().to_owned(),
        launch,
        build,
        build_directory,
        owned_build_directory: owned,
        terminal: configuration["terminal"].as_bool().unwrap_or(true),
    })
}

/// The private build folder of a single-file build: `.rainy-ide/build/<uuid>`, never through a link.
pub fn create_build_directory(root: &Path, path: &Path) -> HostResult<()> {
    check_build_directory(root, path)?;
    for candidate in [root.join(".rainy-ide"), path.parent().map(Path::to_path_buf).unwrap_or_default()] {
        if std::fs::symlink_metadata(&candidate).is_ok_and(|info| info.file_type().is_symlink()) {
            return Err(error("The IDE build directory cannot be a symbolic link."));
        }
        crate::store::create_private_dir(&candidate)?;
        if !contains(root, &realpath(&candidate)?) {
            return Err(error("The IDE build directory escapes the workspace."));
        }
    }
    std::fs::create_dir(path)?;
    Ok(())
}

pub fn remove_build_directory(root: &Path, path: &Path) -> HostResult<()> {
    check_build_directory(root, path)?;
    let Some(parent) = path.parent().and_then(|parent| realpath(parent).ok()) else { return Ok(()) };
    if !contains(root, &parent) {
        return Err(error("The IDE build parent moved outside the workspace."));
    }
    match std::fs::symlink_metadata(path) {
        Err(_) => Ok(()),
        Ok(info) if info.file_type().is_symlink() => Ok(std::fs::remove_file(path)?),
        Ok(_) => {
            if !contains(root, &realpath(path)?) {
                return Err(error("The IDE build directory moved outside the workspace."));
            }
            Ok(std::fs::remove_dir_all(path)?)
        }
    }
}

fn check_build_directory(root: &Path, path: &Path) -> HostResult<()> {
    let relative = crate::paths::relative_to(root, path).unwrap_or_default();
    let valid = relative.strip_prefix(".rainy-ide/build/").is_some_and(|id| id.len() == 36 && id.chars().all(|c| c.is_ascii_hexdigit() || c == '-'));
    if valid { Ok(()) } else { Err(error("Invalid owned IDE build directory.")) }
}

/// A CMake Debug build: configure and build commands, the build folder and the program it makes.
fn cmake(root: &Path, configuration: &Value, compiler: Option<&str>, environment: &BTreeMap<String, String>) -> HostResult<(Vec<CommandSpec>, PathBuf, PathBuf)> {
    let build = &configuration["build"];
    let text = |key: &str| build[key].as_str();
    workspace_path(root, "CMakeLists.txt", Some("file"))?;
    let directory = workspace_path(root, text("buildDirectory").unwrap_or_default(), None)?;
    if directory == root {
        return Err(error("Choose a separate CMake build directory inside the workspace."));
    }
    let program = workspace_path(root, &normalize(&directory.join(text("executable").unwrap_or_default())).display().to_string(), None)?;
    if !contains(&directory, &program) || program == directory {
        return Err(error("The CMake executable must be inside its build directory."));
    }
    let cmake = executable(None, "cmake", environment)?;
    let c = configuration["language"] == "c";
    let compiler = compiler.map(|path| executable(Some(path), if c { "gcc" } else { "g++" }, environment)).transpose()?;
    let (configure_preset, build_preset) = (text("configurePreset"), text("buildPreset"));
    if build_preset.is_some() && configure_preset.is_none() {
        return Err(error("A build preset requires its configure preset."));
    }
    if configure_preset.is_some() || build_preset.is_some() {
        let presets = read_presets(root)?;
        if let Some(name) = configure_preset {
            inherited(name, &presets.0, &mut HashSet::new())?;
        }
        if let Some(name) = build_preset {
            let preset = inherited(name, &presets.1, &mut HashSet::new())?;
            if preset["configurePreset"].as_str() != configure_preset {
                return Err(error("The selected CMake build preset uses another configure preset."));
            }
            let configure = inherited(configure_preset.unwrap_or_default(), &presets.0, &mut HashSet::new())?;
            let binary = configure["binaryDir"].as_str().ok_or_else(|| error("The CMake configure preset must declare its build directory."))?;
            let declared = workspace_path(root, &expand_preset_directory(binary, root, &configure, environment)?.display().to_string(), None)?;
            if declared != directory {
                return Err(error("The CMake preset build directory differs from the selected build directory."));
            }
        }
    }
    let mut configure = vec![cmake.clone()];
    if let Some(preset) = configure_preset {
        configure.extend(["--preset".to_owned(), preset.to_owned()]);
    }
    configure.extend(["-S".to_owned(), root.display().to_string(), "-B".to_owned(), directory.display().to_string()]);
    if let Some(compiler) = compiler {
        configure.push(format!("-DCMAKE_{}_COMPILER={compiler}", if c { "C" } else { "CXX" }));
    }
    configure.extend(["-DCMAKE_BUILD_TYPE=Debug".to_owned(), "-DCMAKE_EXPORT_COMPILE_COMMANDS=ON".to_owned()]);
    let mut make = vec![cmake, "--build".to_owned()];
    match build_preset {
        Some(preset) => make.extend(["--preset".to_owned(), preset.to_owned()]),
        None => make.push(directory.display().to_string()),
    }
    make.extend(["--config".to_owned(), "Debug".to_owned(), "--target".to_owned(), text("target").unwrap_or_default().to_owned()]);
    let command = |argv: Vec<String>| CommandSpec { argv, cwd: root.to_path_buf(), environment: environment.clone() };
    Ok((vec![command(configure), command(make)], directory, program))
}

type PresetMap = std::collections::HashMap<String, Value>;

fn read_presets(root: &Path) -> HostResult<(PresetMap, PresetMap)> {
    let mut presets = (PresetMap::new(), PresetMap::new());
    let mut loaded = HashSet::new();
    let mut bytes = 0usize;
    fn visit(root: &Path, path: &Path, presets: &mut (PresetMap, PresetMap), loaded: &mut HashSet<PathBuf>, loading: &mut HashSet<PathBuf>, bytes: &mut usize) -> HostResult<()> {
        let canonical = workspace_path(root, &path.display().to_string(), Some("file"))?;
        if loading.contains(&canonical) {
            return Err(error("CMake preset includes contain a cycle."));
        }
        if !loaded.insert(canonical.clone()) {
            return Ok(());
        }
        loading.insert(canonical.clone());
        let text = std::fs::read_to_string(&canonical)?;
        *bytes += text.len();
        if *bytes > 32 * 1024 {
            return Err(error("CMake preset files exceed the configured size limit."));
        }
        let document: Value = serde_json::from_str(&text).map_err(|parse| error(format!("{}: {parse}", canonical.display())))?;
        for include in strings(&document["include"]) {
            if include.contains('$') {
                return Err(error("CMake preset include paths must be explicit workspace paths."));
            }
            let next = canonical.parent().unwrap_or(root).join(include);
            visit(root, &next, presets, loaded, loading, bytes)?;
        }
        for (kind, key) in [(0, "configurePresets"), (1, "buildPresets")] {
            for preset in document[key].as_array().into_iter().flatten() {
                let name = preset["name"].as_str().unwrap_or_default().to_owned();
                let map = if kind == 0 { &mut presets.0 } else { &mut presets.1 };
                if map.insert(name.clone(), preset.clone()).is_some() {
                    return Err(error(format!("Duplicate CMake preset: {name}")));
                }
            }
        }
        loading.remove(&canonical);
        Ok(())
    }
    for name in ["CMakePresets.json", "CMakeUserPresets.json"] {
        let path = root.join(name);
        if path.is_file() {
            visit(root, &path, &mut presets, &mut loaded, &mut HashSet::new(), &mut bytes)?;
        }
    }
    Ok(presets)
}

fn inherited(name: &str, presets: &PresetMap, chain: &mut HashSet<String>) -> HostResult<Value> {
    if !chain.insert(name.to_owned()) {
        return Err(error("CMake preset inheritance contains a cycle."));
    }
    let value = presets.get(name).ok_or_else(|| error(format!("CMake preset not found: {name}")))?;
    let parents = match &value["inherits"] {
        Value::String(parent) => vec![parent.clone()],
        other => strings(other),
    };
    let mut combined = Map::new();
    let mut environment = Map::new();
    for parent in parents.iter().rev() {
        let base = inherited(parent, presets, &mut chain.clone())?;
        if let Value::Object(fields) = &base {
            combined.extend(fields.clone());
        }
        if let Value::Object(env) = &base["environment"] {
            environment.extend(env.clone());
        }
    }
    if let Value::Object(fields) = value {
        combined.extend(fields.clone());
    }
    if let Value::Object(env) = &value["environment"] {
        environment.extend(env.clone());
    }
    combined.insert("environment".into(), Value::Object(environment));
    Ok(Value::Object(combined))
}

fn expand_preset_directory(value: &str, root: &Path, preset: &Value, explicit: &BTreeMap<String, String>) -> HostResult<PathBuf> {
    let mut environment: BTreeMap<String, String> = std::env::vars().filter(|(name, _)| !crate::process::sensitive(name) && !name.to_uppercase().starts_with("RAINY_")).collect();
    environment.extend(explicit.clone());
    fn expand(input: &str, root: &Path, preset: &Value, environment: &BTreeMap<String, String>, expanding: &mut HashSet<String>) -> HostResult<String> {
        let pattern = regex::Regex::new(r"\$\{(sourceDir|sourceParentDir|sourceDirName|presetName)\}|\$(p?env)\{([^}]+)\}").expect("valid pattern");
        let mut out = String::new();
        let mut last = 0;
        for captures in pattern.captures_iter(input) {
            let whole = captures.get(0).expect("match");
            out.push_str(&input[last..whole.start()]);
            last = whole.end();
            let replacement = match captures.get(1).map(|m| m.as_str()) {
                Some("sourceDir") => root.display().to_string(),
                Some("sourceParentDir") => root.parent().unwrap_or(root).display().to_string(),
                Some("sourceDirName") => root.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default(),
                Some(_) => preset["name"].as_str().unwrap_or_default().to_owned(),
                None => {
                    let key = captures.get(3).map(|m| m.as_str()).unwrap_or_default().to_owned();
                    if captures.get(2).map(|m| m.as_str()) == Some("penv") {
                        environment.get(&key).cloned().unwrap_or_default()
                    } else {
                        if !expanding.insert(key.clone()) {
                            return Err(error("CMake preset environment contains a cycle."));
                        }
                        let own = &preset["environment"][&key];
                        let value = if own.is_null() && preset["environment"].get(&key).is_some() {
                            String::new()
                        } else if let Some(text) = own.as_str() {
                            expand(text, root, preset, environment, expanding)?
                        } else {
                            environment.get(&key).cloned().unwrap_or_default()
                        };
                        expanding.remove(&key);
                        value
                    }
                }
            };
            out.push_str(&replacement);
        }
        out.push_str(&input[last..]);
        if out.contains('$') {
            return Err(error("The CMake build directory contains an unsupported preset macro."));
        }
        Ok(out)
    }
    let expanded = expand(value, root, preset, &environment, &mut HashSet::new())?;
    Ok(normalize(&root.join(expanded)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checks_configurations_like_3x() {
        let ok = json!({ "name": "a", "language": "python", "program": "a.py", "environment": { "DEBUG": "1" } });
        assert!(check_run_configuration(&ok).is_ok());
        for bad in [
            json!({ "name": "a", "language": "python", "program": "a.py", "environment": { "RAINY_X": "1" } }),
            json!({ "name": "a", "language": "python", "program": "a.py", "build": { "kind": "single-file" } }),
            json!({ "name": "a", "language": "rust", "program": "a.rs" }),
            json!({ "name": "a", "language": "python", "program": "a.py", "extra": 1 }),
            json!({ "name": "a", "language": "c", "program": "a.c", "pythonModule": "x" }),
        ] {
            assert!(check_run_configuration(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn keeps_paths_inside_the_project() {
        let dir = tempfile::tempdir().unwrap();
        let root = realpath(dir.path()).unwrap();
        std::fs::write(root.join("a.py"), "").unwrap();
        assert_eq!(workspace_path(&root, "a.py", Some("file")).unwrap(), root.join("a.py"));
        assert!(workspace_path(&root, "../x", None).is_err());
        assert!(workspace_path(&root, "missing.py", Some("file")).is_err());
        assert_eq!(workspace_path(&root, "out/new", None).unwrap(), root.join("out").join("new"));
        let directory = root.join(".rainy-ide").join("build").join(uuid::Uuid::new_v4().to_string());
        create_build_directory(&root, &directory).unwrap();
        assert!(directory.is_dir());
        remove_build_directory(&root, &directory).unwrap();
        assert!(!directory.exists());
        assert!(create_build_directory(&root, &root.join("elsewhere")).is_err());
    }
}
