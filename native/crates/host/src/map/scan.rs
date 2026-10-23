//! Read a project for its map: list the files (respecting .gitignore), count their lines and follow their imports.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use regex::Regex;

use super::imports::{ImportContext, parse_imports, resolve_import};
use super::languages::file_type;
use super::stack::{declared_dependencies, detect_stack, is_manifest};
use super::{MapFile, MapPackage, StackItem};

/// Limits of one scan.
#[derive(Clone, Copy)]
pub struct ScanLimits {
    /// Files listed; the rest are left out.
    pub max_files: usize,
    /// Files read for lines and imports.
    pub max_read_files: usize,
    /// Bytes read in total.
    pub max_read_bytes: u64,
    /// Larger text files are not read: their lines are estimated from their size.
    pub max_file_bytes: u64,
    /// The scan stops reading after this long and estimates the rest.
    pub deadline: Duration,
}

impl Default for ScanLimits {
    fn default() -> Self {
        Self { max_files: 20_000, max_read_files: 8_000, max_read_bytes: 96 * 1024 * 1024, max_file_bytes: 1024 * 1024, deadline: Duration::from_secs(15) }
    }
}

/// Directories never mapped, whatever .gitignore says.
const EXCLUDED: &[&str] = &[".git", "node_modules", ".venv", "venv", "__pycache__", ".mypy_cache", ".pytest_cache", ".ruff_cache", ".tox", ".gradle", ".next", ".nuxt", ".svelte-kit", ".turbo", ".cache", ".idea", ".vs"];
/// Imports are looked for in files up to this size.
const MAX_IMPORT_BYTES: u64 = 512 * 1024;
/// Bytes per line when a file's lines are estimated from its size.
const BYTES_PER_LINE: u64 = 36;
const READERS: usize = 16;
/// Average characters per line above which a large text file is minified output.
const MINIFIED_LINE_LENGTH: usize = 400;

/// The scanned part of a map.
pub struct ScanResult {
    pub files: Vec<MapFile>,
    pub imports: Vec<usize>,
    pub packages: Vec<MapPackage>,
    pub stack: Vec<StackItem>,
    pub truncated: bool,
}

/// Directories of third-party code copied into a project.
static VENDOR_DIRECTORY: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)(?:^|/)(?:vendors?|third[_-]?party|external|extern)/").unwrap());
/// Minified or bundled output.
static GENERATED_NAME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\.min\.(?:js|css|mjs)$|[.-]bundle\.(?:js|mjs)$").unwrap());

/// Patterns `.gitattributes` marks `linguist-vendored` or `linguist-generated`, as regular expressions.
pub fn vendored_patterns(text: &str) -> Vec<Regex> {
    let mut patterns = Vec::new();
    for line in text.split('\n') {
        let mut fields = line.split_whitespace();
        let Some(pattern) = fields.next().filter(|pattern| !pattern.starts_with('#')) else { continue };
        if !fields.any(|attribute| matches!(attribute, "linguist-vendored" | "linguist-vendored=true" | "linguist-generated" | "linguist-generated=true")) {
            continue;
        }
        let glob = pattern.strip_suffix('/').unwrap_or(pattern);
        // Like .gitignore: a pattern with a slash is relative to the project, one without matches a name anywhere.
        // A directory's files count as matched too, whether or not the pattern ends in `/**`.
        let source = if glob.contains('/') { format!("^{}(?:/.*)?$", glob_source(glob.strip_prefix('/').unwrap_or(glob))) } else { format!("(?:^|/){}(?:/.*)?$", glob_source(glob)) };
        if let Ok(pattern) = Regex::new(&source) {
            patterns.push(pattern);
        }
    }
    patterns
}

/// A gitattributes glob as a regular expression source: `**` spans directories, `*` and `?` stay within one.
fn glob_source(glob: &str) -> String {
    let chars: Vec<char> = glob.chars().collect();
    let mut source = String::new();
    let mut index = 0;
    while index < chars.len() {
        let char = chars[index];
        if char == '*' && chars.get(index + 1) == Some(&'*') {
            if chars.get(index + 2) == Some(&'/') {
                source.push_str("(?:.*/)?");
                index += 2;
            } else {
                source.push_str(".*");
                index += 1;
            }
        } else if char == '*' {
            source.push_str("[^/]*");
        } else if char == '?' {
            source.push_str("[^/]");
        } else {
            source.push_str(&regex::escape(&char.to_string()));
        }
        index += 1;
    }
    source
}

/// Every file under `root` that .gitignore keeps, project-relative with `/`, sorted; at most `limit + 1`.
fn list(root: &Path, limit: usize, cancelled: &AtomicBool) -> Vec<String> {
    let walker = ignore::WalkBuilder::new(root)
        .hidden(false)
        .require_git(false)
        .follow_links(false)
        .filter_entry(|entry| !(entry.file_type().is_some_and(|kind| kind.is_dir()) && entry.file_name().to_str().is_some_and(|name| EXCLUDED.contains(&name))))
        .build();
    let mut paths = Vec::new();
    for entry in walker.flatten() {
        if cancelled.load(Ordering::Relaxed) || paths.len() > limit * 2 {
            break;
        }
        if !entry.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        if let Ok(relative) = entry.path().strip_prefix(root) {
            let path = relative.to_string_lossy().replace('\\', "/");
            if !path.is_empty() {
                paths.push(path);
            }
        }
    }
    paths.sort();
    paths
}

fn count_lines(text: &str) -> u64 {
    if text.is_empty() {
        return 0;
    }
    let lines = text.bytes().filter(|byte| *byte == b'\n').count() as u64;
    if text.ends_with('\n') { lines } else { lines + 1 }
}

/// The code cells of a Jupyter notebook, joined.
fn notebook_code(text: &str) -> String {
    let Ok(notebook) = serde_json::from_str::<serde_json::Value>(text) else { return String::new() };
    let cells = notebook["cells"].as_array().cloned().unwrap_or_default();
    cells
        .iter()
        .filter(|cell| cell["cell_type"] == "code")
        .map(|cell| match &cell["source"] {
            serde_json::Value::Array(lines) => lines.iter().filter_map(serde_json::Value::as_str).collect::<String>(),
            serde_json::Value::String(text) => text.clone(),
            _ => String::new(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// `compilerOptions.paths` of a tsconfig, relative to the project.
fn aliases_of(text: &str) -> Vec<(String, Vec<String>)> {
    static BLOCK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"/\*[\s\S]*?\*/").unwrap());
    static LINE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)(^|[^:])//.*$").unwrap());
    static TRAILING: LazyLock<Regex> = LazyLock::new(|| Regex::new(r",(\s*[}\]])").unwrap());
    // tsconfig allows comments and trailing commas; `//` after a colon is part of a URL such as `$schema`, not a comment.
    let text = BLOCK.replace_all(text, "");
    let text = LINE.replace_all(&text, "$1");
    let text = TRAILING.replace_all(&text, "$1");
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else { return Vec::new() };
    let options = &json["compilerOptions"];
    let base = options["baseUrl"].as_str().map(|base| {
        let base = base.strip_prefix("./").or_else(|| base.strip_prefix('.')).unwrap_or(base);
        base.strip_suffix('/').unwrap_or(base).to_owned()
    });
    let base = base.unwrap_or_default();
    let Some(paths) = options["paths"].as_object() else { return Vec::new() };
    paths
        .iter()
        .filter_map(|(pattern, targets)| {
            let targets = targets.as_array()?;
            let prefix = pattern.strip_suffix('*').unwrap_or(pattern).to_owned();
            if prefix.is_empty() {
                return None;
            }
            let resolved = targets
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(|target| {
                    let target = target.strip_prefix("./").unwrap_or(target);
                    let target = target.strip_suffix('*').unwrap_or(target);
                    format!("{}{}", if base.is_empty() { String::new() } else { format!("{base}/") }, target)
                })
                .collect();
            Some((prefix, resolved))
        })
        .collect()
}

/// Scan a project directory: files, imports between them, external packages and the stack.
pub fn scan_project(root: &Path, limits: ScanLimits, cancelled: &AtomicBool) -> ScanResult {
    let started = Instant::now();
    let listed = list(root, limits.max_files, cancelled);
    let mut truncated = listed.len() > limits.max_files;
    let paths: Vec<String> = listed.into_iter().take(limits.max_files).collect();
    let attributes = if paths.iter().any(|path| path == ".gitattributes") { vendored_patterns(&std::fs::read_to_string(root.join(".gitattributes")).unwrap_or_default()) } else { Vec::new() };
    let types: Vec<_> = paths.iter().map(|path| file_type(path)).collect();
    // Third-party and generated code is shown, but not as the project's own code.
    let files: Vec<Mutex<MapFile>> = paths
        .iter()
        .zip(&types)
        .map(|(path, kind)| {
            let vendored = VENDOR_DIRECTORY.is_match(path) || GENERATED_NAME.is_match(path) || attributes.iter().any(|pattern| pattern.is_match(path));
            let shown = if vendored && (kind.kind == "code" || kind.kind == "test") { "other" } else { kind.kind };
            Mutex::new(MapFile { path: path.clone(), language: kind.language.to_owned(), kind: shown.to_owned(), lines: 0, bytes: 0 })
        })
        .collect();
    let texts: Mutex<Vec<(usize, String)>> = Mutex::default();
    let manifests: Mutex<Vec<(String, String)>> = Mutex::default();
    let (read_files, read_bytes, next) = (AtomicUsize::new(0), AtomicU64::new(0), AtomicUsize::new(0));
    let partial = AtomicBool::new(false);
    std::thread::scope(|scope| {
        for _ in 0..READERS {
            scope.spawn(|| {
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    if index >= paths.len() || cancelled.load(Ordering::Relaxed) {
                        return;
                    }
                    let (path, kind) = (&paths[index], &types[index]);
                    let absolute = root.join(path);
                    let Ok(info) = std::fs::metadata(&absolute) else { continue };
                    if !info.is_file() {
                        continue;
                    }
                    let bytes = info.len();
                    let estimate = if kind.binary || kind.lock { 0 } else { bytes.div_ceil(BYTES_PER_LINE) };
                    {
                        let mut file = files[index].lock().unwrap();
                        file.bytes = bytes;
                        file.lines = estimate;
                    }
                    if kind.binary || kind.lock {
                        continue;
                    }
                    let readable = bytes <= limits.max_file_bytes
                        && read_files.load(Ordering::Relaxed) < limits.max_read_files
                        && read_bytes.load(Ordering::Relaxed) + bytes <= limits.max_read_bytes
                        && started.elapsed() < limits.deadline;
                    if !readable {
                        if bytes <= limits.max_file_bytes {
                            partial.store(true, Ordering::Relaxed);
                        }
                        continue;
                    }
                    read_files.fetch_add(1, Ordering::Relaxed);
                    read_bytes.fetch_add(bytes, Ordering::Relaxed);
                    let Ok(buffer) = std::fs::read(&absolute) else { continue };
                    if buffer[..buffer.len().min(8192)].contains(&0) {
                        let mut file = files[index].lock().unwrap();
                        file.kind = "asset".into();
                        file.lines = 0;
                        continue;
                    }
                    let mut text = String::from_utf8_lossy(&buffer).into_owned();
                    if kind.language == "jupyter" {
                        text = notebook_code(&text);
                    }
                    let lines = count_lines(&text);
                    let minified = bytes > 16 * 1024 && crate::text::js_length(&text) / (lines.max(1) as usize) > MINIFIED_LINE_LENGTH;
                    let shown = {
                        let mut file = files[index].lock().unwrap();
                        file.lines = lines;
                        if minified && (file.kind == "code" || file.kind == "test") {
                            file.kind = "other".into();
                        }
                        file.kind.clone()
                    };
                    let is_code = (shown == "code" || shown == "test") && !minified;
                    if is_manifest(path) && path.split('/').count() <= 3 && bytes <= 256 * 1024 {
                        let mut manifests = manifests.lock().unwrap();
                        if manifests.len() < 12 {
                            manifests.push((path.clone(), text.clone()));
                        }
                    }
                    if is_code && bytes <= MAX_IMPORT_BYTES {
                        texts.lock().unwrap().push((index, text));
                    }
                }
            });
        }
    });
    truncated |= partial.load(Ordering::Relaxed);
    let files: Vec<MapFile> = files.into_iter().map(|file| file.into_inner().unwrap()).collect();
    let mut texts = texts.into_inner().unwrap();
    texts.sort_by_key(|(index, _)| *index);

    let mut context = ImportContext::default();
    for path in &paths {
        context.files.insert(path.clone());
        context.lower.insert(path.to_lowercase(), path.clone());
        let slash = path.rfind('/');
        let directory = slash.map_or("", |slash| &path[..slash]).to_owned();
        match context.directory_index.get(&directory) {
            Some(&position) => context.directories[position].1.push(path.clone()),
            None => {
                context.directory_index.insert(directory.clone(), context.directories.len());
                context.directories.push((directory, vec![path.clone()]));
            }
        }
        let name = path[slash.map_or(0, |slash| slash + 1)..].to_lowercase();
        context.basenames.entry(name.clone()).or_default().push(path.clone());
        if [".java", ".kt", ".scala", ".groovy", ".php"].iter().any(|extension| name.ends_with(extension)) {
            let stem = path[..path.rfind('.').unwrap_or(path.len())].to_lowercase();
            let parts: Vec<&str> = stem.split('/').collect();
            for start in (0..parts.len()).rev() {
                context.classes.entry(parts[start..].join("/")).or_insert_with(|| path.clone());
            }
        }
    }
    let tsconfig = paths.iter().find(|path| *path == "tsconfig.json").or_else(|| paths.iter().find(|path| *path == "jsconfig.json"));
    if let Some(tsconfig) = tsconfig {
        context.aliases = aliases_of(&std::fs::read_to_string(root.join(tsconfig)).unwrap_or_default());
    }
    if paths.iter().any(|path| path == "go.mod") {
        static MODULE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)^module\s+(\S+)").unwrap());
        let text = std::fs::read_to_string(root.join("go.mod")).unwrap_or_default();
        context.go_module = MODULE.captures(&text).map(|captures| captures[1].to_owned());
    }
    let index_of: HashMap<&str, usize> = paths.iter().enumerate().map(|(index, path)| (path.as_str(), index)).collect();
    let mut pairs: HashSet<(usize, usize)> = HashSet::new();
    let mut imports = Vec::new();
    let mut packages: Vec<(String, Vec<usize>)> = Vec::new();
    for (index, text) in &texts {
        let from = &paths[*index];
        let language = types[*index].language;
        for spec in parse_imports(language, text) {
            let target = resolve_import(&context, language, from, &spec);
            for path in &target.files {
                let Some(&to) = index_of.get(path.as_str()) else { continue };
                if to == *index || !pairs.insert((*index, to)) {
                    continue;
                }
                imports.extend([*index, to]);
            }
            if let (true, Some(external)) = (target.files.is_empty(), target.external) {
                match packages.iter_mut().find(|(name, _)| *name == external) {
                    Some((_, users)) => {
                        if !users.contains(index) {
                            users.push(*index);
                        }
                    }
                    None => packages.push((external, vec![*index])),
                }
            }
        }
    }
    let mut packages: Vec<MapPackage> = packages
        .into_iter()
        .map(|(name, mut files)| {
            files.sort_unstable();
            MapPackage { name, files }
        })
        .collect();
    packages.sort_by(|left, right| right.files.len().cmp(&left.files.len()).then_with(|| left.name.cmp(&right.name)));
    let mut manifests = manifests.into_inner().unwrap();
    manifests.sort();
    let declared: Vec<String> = manifests.iter().flat_map(|(path, text)| declared_dependencies(path, text)).collect();
    let stack = detect_stack(&files, &packages, &declared);
    ScanResult { files, imports, packages, stack, truncated }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_gitattributes_and_tsconfig() {
        let patterns = vendored_patterns("vendor/** linguist-vendored\n*.gen.go linguist-generated=true\nsrc/** text\n");
        assert_eq!(patterns.len(), 2);
        assert!(patterns[0].is_match("vendor/a/b.js"));
        assert!(patterns[1].is_match("pkg/x.gen.go"));
        let aliases = aliases_of("{\n // comment\n \"$schema\": \"https://x\",\n \"compilerOptions\": { \"baseUrl\": \"./\", \"paths\": { \"@/*\": [\"src/*\"], }, },\n}");
        assert_eq!(aliases, vec![("@/".to_owned(), vec!["src/".to_owned()])]);
    }

    #[test]
    fn scans_a_project() {
        let root = std::env::temp_dir().join(format!("rainy-map-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/a.ts"), "import { b } from './b'\nimport React from 'react'\n").unwrap();
        std::fs::write(root.join("src/b.ts"), "export const b = 1\n").unwrap();
        std::fs::write(root.join("package.json"), r#"{"dependencies":{"react":"1"}}"#).unwrap();
        std::fs::create_dir_all(root.join("node_modules/x")).unwrap();
        std::fs::write(root.join("node_modules/x/i.js"), "").unwrap();
        let result = scan_project(&root, ScanLimits::default(), &AtomicBool::new(false));
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(result.files.iter().map(|file| file.path.as_str()).collect::<Vec<_>>(), ["package.json", "src/a.ts", "src/b.ts"]);
        assert_eq!(result.imports, [1, 2]);
        assert_eq!(result.packages[0].name, "react");
        assert_eq!(result.stack.iter().map(|item| item.name.as_str()).collect::<Vec<_>>(), ["TypeScript", "React"]);
    }
}
