//! Imports of source files: what each language names, and which project files or external packages those names lead to.
//! Parsing is by pattern, not by grammar; a missed or extra import only moves a line on the map.

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use regex::Regex;

/// How to resolve an import: `Path` is relative to the importing file (or the project when it starts with `/`); `Module`
/// is a language module name; `Mod` and `Use` are Rust declarations; `Include` is a C/C++ local include.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Style {
    Path,
    Module,
    From,
    Mod,
    Use,
    Include,
    Class,
    Lib,
}

/// One import as written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportSpec {
    pub spec: String,
    pub style: Style,
    /// Names after `from x import`, which may be submodules.
    pub names: Vec<String>,
}

/// What the resolver knows about the project.
#[derive(Default)]
pub struct ImportContext {
    /// Every project file, by exact path.
    pub files: HashSet<String>,
    /// Lower-case path → path, for case-insensitive file systems and package names.
    pub lower: HashMap<String, String>,
    /// Directory → its files, sorted.
    pub directories: Vec<(String, Vec<String>)>,
    pub directory_index: HashMap<String, usize>,
    /// Lower-case slashed suffixes of Java, Kotlin, Scala, Groovy and PHP files without extension → path.
    pub classes: HashMap<String, String>,
    /// Lower-case file name → paths, for C and C++ includes that name a file by a partial path.
    pub basenames: HashMap<String, Vec<String>>,
    /// `module` line of the root `go.mod`.
    pub go_module: Option<String>,
    /// `compilerOptions.paths` of the root `tsconfig.json` or `jsconfig.json`: alias prefix → target prefixes.
    pub aliases: Vec<(String, Vec<String>)>,
}

impl ImportContext {
    fn directory(&self, directory: &str) -> Option<&Vec<String>> {
        self.directory_index.get(directory).map(|index| &self.directories[*index].1)
    }
}

/// Where an import leads: project files, or an external package when no project file matched.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ImportTarget {
    pub files: Vec<String>,
    pub external: Option<String>,
}

impl ImportTarget {
    fn files(files: Vec<String>) -> Self {
        Self { files, external: None }
    }

    fn one(found: Option<String>) -> Self {
        Self::files(found.into_iter().collect())
    }

    fn external(name: impl Into<String>) -> Self {
        Self { files: Vec::new(), external: Some(name.into()) }
    }
}

const JS_EXTENSIONS: &[&str] = &[".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs", ".mts", ".cts", ".vue", ".svelte", ".json", ".d.ts"];
const NODE_BUILTINS: &[&str] = &[
    "assert", "async_hooks", "buffer", "child_process", "cluster", "console", "constants", "crypto", "dgram", "diagnostics_channel", "dns", "domain", "events", "fs", "http", "http2", "https", "inspector",
    "module", "net", "os", "path", "perf_hooks", "process", "punycode", "querystring", "readline", "repl", "stream", "string_decoder", "sys", "timers", "tls", "trace_events", "tty", "url", "util", "v8",
    "vm", "wasi", "worker_threads", "zlib", "test", "sqlite",
];
const PYTHON_STDLIB: &[&str] = &[
    "__future__", "abc", "argparse", "array", "ast", "asyncio", "atexit", "base64", "binascii", "bisect", "builtins", "bz2", "calendar", "cmath", "cmd", "codecs", "collections", "colorsys", "concurrent",
    "configparser", "contextlib", "contextvars", "copy", "copyreg", "cProfile", "csv", "ctypes", "curses", "dataclasses", "datetime", "dbm", "decimal", "difflib", "dis", "doctest", "email", "encodings",
    "enum", "errno", "faulthandler", "fcntl", "filecmp", "fileinput", "fnmatch", "fractions", "ftplib", "functools", "gc", "getopt", "getpass", "gettext", "glob", "graphlib", "grp", "gzip", "hashlib",
    "heapq", "hmac", "html", "http", "imaplib", "importlib", "inspect", "io", "ipaddress", "itertools", "json", "keyword", "linecache", "locale", "logging", "lzma", "mailbox", "marshal", "math",
    "mimetypes", "mmap", "msvcrt", "multiprocessing", "netrc", "numbers", "operator", "optparse", "os", "pathlib", "pdb", "pickle", "pkgutil", "platform", "plistlib", "poplib", "posix", "pprint",
    "profile", "pstats", "pty", "pwd", "py_compile", "queue", "quopri", "random", "re", "readline", "reprlib", "resource", "rlcompleter", "runpy", "sched", "secrets", "select", "selectors", "shelve",
    "shlex", "shutil", "signal", "site", "smtplib", "socket", "socketserver", "sqlite3", "ssl", "stat", "statistics", "string", "stringprep", "struct", "subprocess", "symtable", "sys", "sysconfig",
    "syslog", "tabnanny", "tarfile", "telnetlib", "tempfile", "termios", "textwrap", "threading", "time", "timeit", "tkinter", "token", "tokenize", "tomllib", "trace", "traceback", "tracemalloc", "tty",
    "turtle", "types", "typing", "unicodedata", "unittest", "urllib", "uuid", "venv", "warnings", "wave", "weakref", "webbrowser", "winreg", "winsound", "wsgiref", "xml", "xmlrpc", "zipapp", "zipfile",
    "zipimport", "zlib", "zoneinfo", "_thread",
];
const RUST_ROOTS: &[&str] = &["crate", "self", "super", "std", "core", "alloc"];

macro_rules! regex {
    ($pattern:literal) => {{
        static PATTERN: LazyLock<Regex> = LazyLock::new(|| Regex::new($pattern).unwrap());
        &*PATTERN
    }};
}

fn collect(text: &str, pattern: &Regex, style: Style, out: &mut Vec<ImportSpec>) {
    for captures in pattern.captures_iter(text) {
        if let Some(spec) = captures.get(1).map(|m| m.as_str().trim()).filter(|spec| !spec.is_empty()) {
            out.push(ImportSpec { spec: spec.to_owned(), style, names: Vec::new() });
        }
    }
}

/// The imports a file names, in file order (repeats included). `text` is the file content; for notebooks, the code cells.
pub fn parse_imports(language: &str, text: &str) -> Vec<ImportSpec> {
    let mut out = Vec::new();
    match language {
        "typescript" | "javascript" | "vue" | "svelte" | "astro" => {
            collect(text, regex!(r#"\bfrom\s*['"]([^'"\n]{1,300})['"]"#), Style::Path, &mut out);
            collect(text, regex!(r#"(?m)(?:^|[;\s])import\s*['"]([^'"\n]{1,300})['"]"#), Style::Path, &mut out);
            collect(text, regex!(r#"\b(?:require|import)\s*\(\s*['"]([^'"\n]{1,300})['"]\s*\)"#), Style::Path, &mut out);
        }
        "python" | "sage" | "jupyter" => {
            for captures in regex!(r"(?mR)^[ \t]*import[ \t]+([\w. \t,]+?)[ \t]*(?:#.*)?$").captures_iter(text) {
                for part in captures[1].split(',') {
                    let name = regex!(r"\s+as\s+").split(part.trim()).next().unwrap_or("").trim();
                    if !name.is_empty() {
                        out.push(ImportSpec { spec: name.to_owned(), style: Style::Module, names: Vec::new() });
                    }
                }
            }
            for captures in regex!(r"(?m)^[ \t]*from[ \t]+(\.*[\w.]*)[ \t]+import[ \t]+\(?([^\n#)]*)").captures_iter(text) {
                let names = captures[2]
                    .split(',')
                    .map(|part| regex!(r"\s+as\s+").split(part.trim()).next().unwrap_or("").trim().to_owned())
                    .filter(|name| regex!(r"^\w+$").is_match(name))
                    .collect();
                out.push(ImportSpec { spec: captures[1].to_owned(), style: Style::From, names });
            }
            if language == "sage" {
                collect(text, regex!(r#"\b(?:load|attach)\(\s*['"]([^'"\n]+)['"]\s*\)"#), Style::Path, &mut out);
            }
        }
        "c" | "cpp" | "objective-c" | "cuda" => collect(text, regex!(r#"(?m)^[ \t]*#[ \t]*(?:include|import)[ \t]*"([^"\n]+)""#), Style::Include, &mut out),
        "go" => {
            collect(text, regex!(r#"(?m)^[ \t]*import[ \t]+(?:[\w.]+[ \t]+)?"([^"\n]+)""#), Style::Module, &mut out);
            for block in regex!(r"(?m)^[ \t]*import[ \t]*\(([\s\S]*?)\)").captures_iter(text) {
                collect(&block[1], regex!(r#""([^"\n]+)""#), Style::Module, &mut out);
            }
        }
        "rust" => {
            collect(text, regex!(r"(?m)^[ \t]*(?:pub(?:\([^)\n]*\))?[ \t]+)?mod[ \t]+(\w+)[ \t]*;"), Style::Mod, &mut out);
            collect(text, regex!(r"(?m)^[ \t]*(?:pub(?:\([^)\n]*\))?[ \t]+)?use[ \t]+((?:\w+::)*\w+)"), Style::Use, &mut out);
            collect(text, regex!(r"(?m)^[ \t]*extern[ \t]+crate[ \t]+(\w+)"), Style::Use, &mut out);
        }
        "java" | "kotlin" | "scala" | "groovy" => collect(text, regex!(r"(?mR)^[ \t]*import[ \t]+(?:static[ \t]+)?([\w.]+(?:\.\*|\._)?)[ \t]*;?[ \t]*$"), Style::Class, &mut out),
        "php" => {
            collect(text, regex!(r#"\b(?:require|include)(?:_once)?\s*\(?\s*(?:__DIR__\s*\.\s*)?['"]([^'"\n]+)['"]"#), Style::Path, &mut out);
            collect(text, regex!(r"(?m)^[ \t]*use[ \t]+\\?([\w\\]+)"), Style::Class, &mut out);
        }
        "ruby" => {
            collect(text, regex!(r#"(?m)^[ \t]*require_relative[ \t(]+['"]([^'"\n]+)['"]"#), Style::Path, &mut out);
            collect(text, regex!(r#"(?m)^[ \t]*require[ \t(]+['"]([^'"\n]+)['"]"#), Style::Lib, &mut out);
        }
        "lua" => collect(text, regex!(r#"\brequire\s*\(?\s*['"]([^'"\n]+)['"]"#), Style::Module, &mut out),
        "shell" => {
            for captures in regex!(r#"(?m)^[ \t]*(?:source|\.)[ \t]+["']?([^\s"';&|]+)"#).captures_iter(text) {
                let raw = &captures[1];
                // `"$(dirname "$0")/lib.sh"` and `$DIR/lib.sh` mean a file next to the script.
                let spec = if raw.contains('$') { format!("./{}", &raw[raw.rfind('/').map_or(0, |slash| slash + 1)..]) } else { raw.to_owned() };
                if !spec.contains('$') {
                    out.push(ImportSpec { spec, style: Style::Path, names: Vec::new() });
                }
            }
        }
        "powershell" => {
            collect(text, regex!(r#"(?m)^[ \t]*\.[ \t]+["']?([^\s"']+\.ps1)"#), Style::Path, &mut out);
            collect(text, regex!(r#"\bImport-Module[ \t]+["']?(\.[^\s"']+)"#), Style::Path, &mut out);
        }
        "html" => {
            collect(text, regex!(r#"(?i)<script\b[^>]*\bsrc\s*=\s*["']([^"']+)["']"#), Style::Path, &mut out);
            collect(text, regex!(r#"(?i)<link\b[^>]*\bhref\s*=\s*["']([^"']+\.css)["']"#), Style::Path, &mut out);
        }
        "css" => collect(text, regex!(r#"@(?:import|use|forward)\s+(?:url\()?\s*['"]([^'"\n]+)['"]"#), Style::Path, &mut out),
        _ => {}
    }
    out
}

/// Join a relative path to a directory; `None` when it leaves the project.
pub fn join_path(directory: &str, relative: &str) -> Option<String> {
    let mut parts: Vec<String> = if directory.is_empty() { Vec::new() } else { directory.split('/').map(str::to_owned).collect() };
    for part in relative.replace('\\', "/").split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            part => parts.push(part.to_owned()),
        }
    }
    Some(parts.join("/"))
}

fn dirname(path: &str) -> &str {
    path.rfind('/').map_or("", |slash| &path[..slash])
}

fn find(context: &ImportContext, candidates: &[Option<String>]) -> Option<String> {
    candidates.iter().flatten().filter(|candidate| !candidate.is_empty()).find_map(|candidate| {
        if context.files.contains(candidate) { Some(candidate.clone()) } else { context.lower.get(&candidate.to_lowercase()).cloned() }
    })
}

fn some(paths: impl IntoIterator<Item = String>) -> Vec<Option<String>> {
    paths.into_iter().map(Some).collect()
}

fn js_candidates(base: &str) -> Vec<Option<String>> {
    let mut candidates = vec![base.to_owned()];
    candidates.extend(JS_EXTENSIONS.iter().map(|extension| format!("{base}{extension}")));
    candidates.extend(JS_EXTENSIONS.iter().map(|extension| format!("{base}/index{extension}")));
    // TypeScript sources import their compiled `.js` names.
    if let Some(compiled) = regex!(r"\.(m|c)?jsx?$").captures(base) {
        let stem = &base[..base.len() - compiled[0].len()];
        let flavor = compiled.get(1).map_or("", |m| m.as_str());
        candidates.extend([format!("{stem}.ts"), format!("{stem}.tsx"), format!("{stem}.{flavor}ts")]);
    }
    some(candidates)
}

fn resolve_js(context: &ImportContext, from: &str, spec: &str) -> ImportTarget {
    let clean = regex!(r"[?#].*$").replace(spec, "");
    if regex!(r"(?i)^(?:[a-z]+:)?//").is_match(&clean) || regex!(r"(?i)^(?:data|virtual|https?):").is_match(&clean) {
        return ImportTarget::default();
    }
    if clean.starts_with('.') || clean.starts_with('/') {
        let base = if clean.starts_with('/') { join_path("", &clean) } else { join_path(dirname(from), &clean) };
        return ImportTarget::one(base.and_then(|base| find(context, &js_candidates(&base))));
    }
    for (prefix, targets) in &context.aliases {
        let Some(rest) = clean.strip_prefix(prefix.as_str()) else { continue };
        for target in targets {
            if let Some(found) = join_path("", &format!("{target}{rest}")).and_then(|base| find(context, &js_candidates(&base))) {
                return ImportTarget::files(vec![found]);
            }
        }
    }
    if clean.starts_with("node:") {
        return ImportTarget::default();
    }
    let parts: Vec<&str> = clean.split('/').collect();
    let name = if clean.starts_with('@') { parts[..parts.len().min(2)].join("/") } else { parts[0].to_owned() };
    if NODE_BUILTINS.contains(&name.as_str()) { ImportTarget::default() } else { ImportTarget::external(name) }
}

fn python_module(context: &ImportContext, roots: &[&str], parts: &[&str]) -> Option<String> {
    for root in roots {
        for length in (1..=parts.len()).rev() {
            let Some(base) = join_path(root, &parts[..length].join("/")) else { continue };
            if let Some(found) = find(context, &some([format!("{base}.py"), format!("{base}/__init__.py"), format!("{base}.sage"), format!("{base}.pyi")])) {
                return Some(found);
            }
        }
    }
    None
}

fn resolve_python(context: &ImportContext, from: &str, import: &ImportSpec) -> ImportTarget {
    let spec = import.spec.as_str();
    if spec.starts_with('.') {
        let dots = spec.len() - spec.trim_start_matches('.').len();
        let mut directory = dirname(from).to_owned();
        for _ in 1..dots {
            directory = dirname(&directory).to_owned();
        }
        let rest = &spec[dots..];
        let mut files = Vec::new();
        let base = if rest.is_empty() { Some(directory) } else { join_path(&directory, &rest.replace('.', "/")) };
        let Some(base) = base else { return ImportTarget::files(files) };
        let inside = if base.is_empty() { String::new() } else { format!("{base}/") };
        for name in &import.names {
            if let Some(found) = find(context, &some([format!("{inside}{name}.py"), format!("{inside}{name}/__init__.py")])) {
                files.push(found);
            }
        }
        if files.is_empty() || !rest.is_empty() {
            let candidates = if rest.is_empty() { some([format!("{inside}__init__.py")]) } else { some([format!("{base}.py"), format!("{inside}__init__.py")]) };
            if let Some(found) = find(context, &candidates) {
                files.push(found);
            }
        }
        return ImportTarget::files(files);
    }
    let parts: Vec<&str> = spec.split('.').filter(|part| !part.is_empty()).collect();
    if parts.is_empty() {
        return ImportTarget::default();
    }
    let roots = [dirname(from), "", "src", "lib", "python"];
    if import.style == Style::From {
        let mut files: Vec<String> = Vec::new();
        let joined = parts.join("/");
        for name in &import.names {
            let mut with_name = parts.clone();
            with_name.push(name);
            if let Some(found) = python_module(context, &roots, &with_name) {
                if !found.ends_with(&format!("{joined}.py")) && !found.ends_with(&format!("{joined}/__init__.py")) && !files.contains(&found) {
                    files.push(found);
                }
            }
        }
        if let Some(module) = python_module(context, &roots, &parts) {
            if !files.contains(&module) {
                files.push(module);
            }
        }
        if !files.is_empty() {
            return ImportTarget::files(files);
        }
    } else if let Some(found) = python_module(context, &roots, &parts) {
        return ImportTarget::files(vec![found]);
    }
    if PYTHON_STDLIB.contains(&parts[0]) { ImportTarget::default() } else { ImportTarget::external(parts[0]) }
}

fn rust_crate_root(context: &ImportContext, from: &str) -> String {
    let mut directory = dirname(from).to_owned();
    loop {
        let prefix = if directory.is_empty() { String::new() } else { format!("{directory}/") };
        if find(context, &some([format!("{prefix}main.rs"), format!("{prefix}lib.rs")])).is_some() {
            return directory;
        }
        if directory.is_empty() {
            return dirname(from).to_owned();
        }
        directory = dirname(&directory).to_owned();
    }
}

fn rust_module_directory(from: &str) -> String {
    let name = &from[from.rfind('/').map_or(0, |slash| slash + 1)..];
    if matches!(name, "main.rs" | "lib.rs" | "mod.rs") {
        dirname(from).to_owned()
    } else {
        let parent = dirname(from);
        format!("{}{}", if parent.is_empty() { String::new() } else { format!("{parent}/") }, &name[..name.len().saturating_sub(3)])
    }
}

fn rust_path(context: &ImportContext, directory: &str, parts: &[&str]) -> Option<String> {
    (1..=parts.len()).rev().find_map(|length| join_path(directory, &parts[..length].join("/")).and_then(|base| find(context, &some([format!("{base}.rs"), format!("{base}/mod.rs")]))))
}

fn resolve_rust(context: &ImportContext, from: &str, import: &ImportSpec) -> ImportTarget {
    if import.style == Style::Mod {
        let base = join_path(&rust_module_directory(from), &import.spec);
        return ImportTarget::one(base.and_then(|base| find(context, &some([format!("{base}.rs"), format!("{base}/mod.rs")]))));
    }
    let parts: Vec<&str> = import.spec.split("::").collect();
    let head = parts[0];
    if head == "crate" {
        return ImportTarget::one(rust_path(context, &rust_crate_root(context, from), &parts[1..]));
    }
    if head == "self" || head == "super" {
        let mut directory = rust_module_directory(from);
        let mut rest = &parts[1..];
        if head == "super" {
            directory = dirname(&directory).to_owned();
        }
        while rest.first() == Some(&"super") {
            directory = dirname(&directory).to_owned();
            rest = &rest[1..];
        }
        return ImportTarget::one(rust_path(context, &directory, rest));
    }
    if RUST_ROOTS.contains(&head) { ImportTarget::default() } else { ImportTarget::external(head.replace('_', "-")) }
}

fn resolve_class(context: &ImportContext, language: &str, spec: &str) -> ImportTarget {
    let wildcard = spec.ends_with(".*") || spec.ends_with("._");
    let separator = if language == "php" { '\\' } else { '.' };
    let body = if wildcard { &spec[..spec.len() - 2] } else { spec };
    let parts: Vec<&str> = body.split(separator).filter(|part| !part.is_empty()).collect();
    if parts.is_empty() {
        return ImportTarget::default();
    }
    if wildcard {
        // A wildcard import names a package directory.
        let suffix = format!("/{}", parts.join("/").to_lowercase());
        for (directory, members) in &context.directories {
            if !format!("/{}", directory.to_lowercase()).ends_with(&suffix) {
                continue;
            }
            if let Some(first) = members.iter().find(|path| regex!(r"\.(?:java|kt|scala|groovy)$").is_match(path)) {
                return ImportTarget::files(vec![first.clone()]);
            }
        }
    } else {
        // `import a.b.C.member` (static) names a class one level up.
        for drop in 0..2 {
            if parts.len() <= drop {
                break;
            }
            let kept = &parts[..parts.len() - drop];
            let found = context.classes.get(&kept.join("/").to_lowercase()).or_else(|| if language == "php" && kept.len() > 1 { context.classes.get(&kept[1..].join("/").to_lowercase()) } else { None });
            if let Some(found) = found {
                return ImportTarget::files(vec![found.clone()]);
            }
        }
    }
    if language == "php" || regex!(r"^(?:java|javax|jdk|sun|kotlin|kotlinx|scala|groovy|android|androidx|dalvik)\.").is_match(&format!("{}.", parts.join("."))) {
        return ImportTarget::default();
    }
    let take = if ["com", "org", "io", "net", "dev", "me", "cn"].contains(&parts[0]) { 3 } else { 2 };
    ImportTarget::external(parts[..parts.len().min(take)].join("."))
}

fn is_remote(spec: &str) -> bool {
    regex!(r"(?i)^(?:https?:|//|data:)").is_match(spec)
}

/// Resolve one import of the file `from`, written in `language`: the project files it leads to, or the external package.
pub fn resolve_import(context: &ImportContext, language: &str, from: &str, import: &ImportSpec) -> ImportTarget {
    let spec = import.spec.as_str();
    let here = dirname(from);
    match language {
        "typescript" | "javascript" | "vue" | "svelte" | "astro" => resolve_js(context, from, spec),
        "python" | "sage" | "jupyter" => {
            if import.style == Style::Path {
                ImportTarget::one(find(context, &[join_path(here, spec)]))
            } else {
                resolve_python(context, from, import)
            }
        }
        "c" | "cpp" | "objective-c" | "cuda" => {
            if let Some(local) = find(context, &[join_path(here, spec), join_path("", spec), join_path("include", spec), join_path("src", spec), join_path("inc", spec)]) {
                return ImportTarget::files(vec![local]);
            }
            let clean = regex!(r"^(?:\.\.?/)+").replace(&spec.replace('\\', "/"), "").to_lowercase();
            let name = &clean[clean.rfind('/').map_or(0, |slash| slash + 1)..];
            let matches: Vec<String> = context.basenames.get(name).into_iter().flatten().filter(|path| format!("/{}", path.to_lowercase()).ends_with(&format!("/{clean}"))).cloned().collect();
            ImportTarget::files(if matches.len() == 1 { matches } else { Vec::new() })
        }
        "go" => {
            if let Some(module) = context.go_module.as_deref() {
                if spec == module || spec.starts_with(&format!("{module}/")) {
                    let directory = if spec == module { "" } else { &spec[module.len() + 1..] };
                    let found = context.directory(directory).and_then(|members| members.iter().find(|path| path.ends_with(".go") && !path.ends_with("_test.go")).cloned());
                    return ImportTarget::one(found);
                }
            }
            let parts: Vec<&str> = spec.split('/').collect();
            if !parts[0].contains('.') {
                return ImportTarget::default();
            }
            let take = if parts[0] == "gopkg.in" { 2 } else { 3 };
            ImportTarget::external(parts[..parts.len().min(take)].join("/"))
        }
        "rust" => resolve_rust(context, from, import),
        "java" | "kotlin" | "scala" | "groovy" => resolve_class(context, language, spec),
        "php" => {
            if import.style == Style::Class {
                resolve_class(context, language, spec)
            } else {
                ImportTarget::one(find(context, &[join_path(here, spec), join_path("", spec)]))
            }
        }
        "ruby" => {
            if import.style == Style::Path {
                let base = join_path(here, spec);
                return ImportTarget::one(base.and_then(|base| find(context, &some([base.clone(), format!("{base}.rb")]))));
            }
            match find(context, &[Some(format!("lib/{spec}.rb")), Some(format!("{spec}.rb")), join_path(here, &format!("{spec}.rb"))]) {
                Some(found) => ImportTarget::files(vec![found]),
                None => ImportTarget::external(spec.split('/').next().unwrap_or(spec)),
            }
        }
        "lua" => {
            let path = spec.replace('.', "/");
            ImportTarget::one(find(context, &[Some(format!("{path}.lua")), Some(format!("{path}/init.lua")), join_path(here, &format!("{path}.lua"))]))
        }
        "css" => {
            if is_remote(spec) {
                return ImportTarget::default();
            }
            let Some(base) = (if spec.starts_with('/') { join_path("", spec) } else { join_path(here, spec) }) else { return ImportTarget::default() };
            let parent = dirname(&base);
            let partial = format!("{}_{}", if parent.is_empty() { String::new() } else { format!("{parent}/") }, &base[base.rfind('/').map_or(0, |slash| slash + 1)..]);
            ImportTarget::one(find(context, &some([base.clone(), format!("{base}.scss"), format!("{partial}.scss"), format!("{base}.css"), format!("{partial}.css"), format!("{base}.sass"), format!("{base}.less")])))
        }
        _ => {
            if is_remote(spec) {
                return ImportTarget::default();
            }
            ImportTarget::one(find(context, &[if spec.starts_with('/') { join_path("", spec) } else { join_path(here, spec) }]))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context(paths: &[&str]) -> ImportContext {
        let mut context = ImportContext::default();
        for path in paths {
            context.files.insert((*path).to_owned());
            context.lower.insert(path.to_lowercase(), (*path).to_owned());
        }
        context
    }

    #[test]
    fn parses_and_resolves() {
        let ctx = context(&["src/a.ts", "src/b.ts", "pkg/__init__.py", "pkg/util.py", "main.py", "src/main.rs", "src/net/mod.rs"]);
        let specs = parse_imports("typescript", "import x from './b.js'\nimport 'react'\n");
        assert_eq!(specs.len(), 2);
        assert_eq!(resolve_import(&ctx, "typescript", "src/a.ts", &specs[0]).files, ["src/b.ts"]);
        assert_eq!(resolve_import(&ctx, "typescript", "src/a.ts", &specs[1]).external.as_deref(), Some("react"));
        let specs = parse_imports("python", "import os, numpy as np\nfrom pkg import util\r\nfrom . import x\n");
        assert_eq!(specs.iter().map(|spec| spec.spec.as_str()).collect::<Vec<_>>(), ["os", "numpy", "pkg", "."]);
        assert_eq!(resolve_import(&ctx, "python", "main.py", &specs[1]).external.as_deref(), Some("numpy"));
        assert_eq!(resolve_import(&ctx, "python", "main.py", &specs[2]).files, ["pkg/util.py", "pkg/__init__.py"]);
        let specs = parse_imports("rust", "mod net;\nuse crate::net::Client;\nuse serde_json::Value;\n");
        assert_eq!(resolve_import(&ctx, "rust", "src/main.rs", &specs[0]).files, ["src/net/mod.rs"]);
        assert_eq!(resolve_import(&ctx, "rust", "src/main.rs", &specs[1]).files, ["src/net/mod.rs"]);
        assert_eq!(resolve_import(&ctx, "rust", "src/main.rs", &specs[2]).external.as_deref(), Some("serde-json"));
        assert_eq!(join_path("a", "../../x"), None);
    }
}
