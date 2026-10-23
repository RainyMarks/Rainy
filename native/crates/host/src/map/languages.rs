//! What a file is by its name: language and kind, and which files are tests or generated locks.

use std::sync::LazyLock;

use regex::Regex;

/// Extensions of source code, by language id.
fn code(extension: &str) -> Option<&'static str> {
    Some(match extension {
        "ts" | "tsx" | "mts" | "cts" => "typescript",
        "js" | "jsx" | "mjs" | "cjs" => "javascript",
        "vue" => "vue",
        "svelte" => "svelte",
        "astro" => "astro",
        "py" | "pyw" | "pyi" => "python",
        "sage" => "sage",
        "ipynb" => "jupyter",
        "c" | "h" => "c",
        "cc" | "cpp" | "cxx" | "hpp" | "hh" | "hxx" | "ino" => "cpp",
        "go" => "go",
        "rs" => "rust",
        "java" => "java",
        "kt" | "kts" => "kotlin",
        "scala" => "scala",
        "groovy" => "groovy",
        "cs" => "csharp",
        "fs" => "fsharp",
        "vb" => "vb",
        "swift" => "swift",
        "m" | "mm" => "objective-c",
        "dart" => "dart",
        "php" => "php",
        "rb" => "ruby",
        "lua" => "lua",
        "pl" | "pm" => "perl",
        "r" => "r",
        "jl" => "julia",
        "sh" | "bash" | "zsh" | "fish" => "shell",
        "ps1" | "psm1" => "powershell",
        "bat" | "cmd" => "batch",
        "html" | "htm" => "html",
        "css" | "scss" | "sass" | "less" => "css",
        "sql" => "sql",
        "asm" | "s" | "nasm" => "assembly",
        "sol" => "solidity",
        "zig" => "zig",
        "nim" => "nim",
        "v" | "sv" => "verilog",
        "vhd" => "vhdl",
        "hs" => "haskell",
        "ml" => "ocaml",
        "ex" | "exs" => "elixir",
        "erl" => "erlang",
        "clj" => "clojure",
        "elm" => "elm",
        "wat" => "wasm",
        "proto" => "protobuf",
        "graphql" | "gql" => "graphql",
        "cu" => "cuda",
        "glsl" => "glsl",
        "hlsl" => "hlsl",
        _ => return None,
    })
}

fn doc(extension: &str) -> Option<&'static str> {
    Some(match extension {
        "md" | "markdown" | "mdx" => "markdown",
        "rst" => "rst",
        "adoc" => "asciidoc",
        "tex" => "latex",
        "org" => "org",
        _ => return None,
    })
}

fn config(extension: &str) -> Option<&'static str> {
    Some(match extension {
        "json" | "jsonc" | "json5" => "json",
        "yaml" | "yml" => "yaml",
        "toml" => "toml",
        "ini" | "cfg" | "conf" | "properties" | "env" => "ini",
        "xml" | "plist" => "xml",
        "gradle" => "gradle",
        "cmake" => "cmake",
        "mk" => "make",
        "nix" => "nix",
        "tf" => "terraform",
        _ => return None,
    })
}

/// Names that carry no extension or whose extension says nothing.
fn named(name: &str) -> Option<(&'static str, &'static str)> {
    Some(match name {
        "makefile" | "gnumakefile" => ("make", "config"),
        "dockerfile" => ("docker", "config"),
        "cmakelists.txt" => ("cmake", "config"),
        "docker-compose.yml" | "docker-compose.yaml" => ("yaml", "config"),
        "vagrantfile" | "gemfile" => ("ruby", "config"),
        "rakefile" => ("ruby", "code"),
        "go.mod" | "go.work" => ("go", "config"),
        "pipfile" => ("toml", "config"),
        "procfile" | "justfile" | "requirements.txt" | "requirements-dev.txt" | "constraints.txt" | ".gitignore" | ".gitattributes" | ".editorconfig" | ".npmrc" | ".env" => ("", "config"),
        "license" | "licence" | "copying" | "notice" | "readme" | "changelog" | "authors" => ("", "doc"),
        _ => return None,
    })
}

/// Generated dependency locks: config, but their size says nothing about the project.
const LOCKS: &[&str] = &[
    "package-lock.json", "pnpm-lock.yaml", "yarn.lock", "bun.lockb", "cargo.lock", "poetry.lock", "pipfile.lock", "go.sum", "composer.lock", "gemfile.lock", "uv.lock", "pdm.lock", "flake.lock", "packages.lock.json",
];

/// Extensions of files that are not text, or not meant to be read as text.
const BINARY: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "bmp", "ico", "icns", "webp", "avif", "tif", "tiff", "psd", "svg", "mp3", "wav", "ogg", "flac", "m4a", "aac", "mid", "mp4", "webm", "mov", "avi", "mkv", "ttf", "otf", "woff", "woff2",
    "eot", "pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "zip", "tar", "gz", "tgz", "bz2", "xz", "7z", "rar", "zst", "jar", "war", "apk", "ipa", "exe", "dll", "so", "dylib", "a", "lib", "o", "obj", "bin",
    "elf", "out", "pyc", "pyo", "class", "wasm", "node", "pdb", "msi", "deb", "rpm", "img", "iso", "dmg", "vmdk", "qcow2", "raw", "dat", "db", "sqlite", "sqlite3", "pcap", "pcapng", "cap", "mem", "dmp", "core",
    "npy", "npz", "pt", "pth", "onnx", "pkl",
];

static TEST_PATH: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)(?:^|/)(?:tests?|__tests__|specs?|e2e|testing)/").unwrap());
static TEST_NAME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?:\.(?:test|spec)\.[^.]+$|^test_.+\.py$|_test\.(?:py|go|rb|c|cpp|rs)$|Tests?\.(?:java|kt|cs|swift)$)").unwrap());

/// What the map knows about a file before reading it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileType {
    pub language: &'static str,
    pub kind: &'static str,
    /// Its content is binary, so it is not read.
    pub binary: bool,
    /// A generated lock file: its lines are not counted.
    pub lock: bool,
}

/// Classify a file by its project-relative path with `/` separators.
pub fn file_type(path: &str) -> FileType {
    let name = &path[path.rfind('/').map_or(0, |slash| slash + 1)..];
    let lower = name.to_lowercase();
    let plain = |language, kind| FileType { language, kind, binary: false, lock: false };
    if LOCKS.contains(&lower.as_str()) {
        return FileType { language: "", kind: "config", binary: false, lock: true };
    }
    let stripped = ["md", "txt", "rst"].iter().find_map(|extension| lower.strip_suffix(&format!(".{extension}"))).unwrap_or(&lower);
    if let Some((language, kind)) = named(&lower).or_else(|| named(stripped)) {
        return plain(if language.is_empty() && lower.ends_with(".md") { "markdown" } else { language }, kind);
    }
    if lower.starts_with("dockerfile") {
        return plain("docker", "config");
    }
    let extension = match lower.rfind('.') {
        Some(dot) if dot > 0 => &lower[dot + 1..],
        _ => "",
    };
    if let Some(language) = code(extension) {
        let test = TEST_PATH.is_match(&format!("/{path}")) || TEST_NAME.is_match(name);
        return plain(language, if test { "test" } else { "code" });
    }
    if let Some(language) = doc(extension) {
        return plain(language, "doc");
    }
    if let Some(language) = config(extension) {
        return plain(language, "config");
    }
    if BINARY.contains(&extension) {
        return FileType { language: if extension == "svg" { "svg" } else { "" }, kind: "asset", binary: true, lock: false };
    }
    plain(
        match extension {
            "txt" => "txt",
            "log" => "log",
            "csv" => "csv",
            _ => "",
        },
        "other",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_files() {
        assert_eq!((file_type("src/a.ts").language, file_type("src/a.ts").kind), ("typescript", "code"));
        assert_eq!(file_type("tests/a.py").kind, "test");
        assert_eq!(file_type("src/a.spec.ts").kind, "test");
        assert_eq!(file_type("README.md").language, "markdown");
        assert_eq!(file_type("README.md").kind, "doc");
        assert!(file_type("Cargo.lock").lock);
        assert!(file_type("x/logo.png").binary);
        assert_eq!(file_type("Dockerfile.dev").language, "docker");
        assert_eq!(file_type("notes.txt").language, "txt");
    }
}
