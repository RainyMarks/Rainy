//! What a project is built with: its main languages, the frameworks and libraries it declares or imports, and build tools.

use std::sync::LazyLock;

use regex::Regex;

use super::{MapFile, MapPackage, StackItem};

fn language_name(language: &str) -> Option<&'static str> {
    Some(match language {
        "typescript" => "TypeScript",
        "javascript" => "JavaScript",
        "vue" => "Vue",
        "svelte" => "Svelte",
        "astro" => "Astro",
        "python" => "Python",
        "sage" => "SageMath",
        "jupyter" => "Jupyter",
        "c" => "C",
        "cpp" => "C++",
        "go" => "Go",
        "rust" => "Rust",
        "java" => "Java",
        "kotlin" => "Kotlin",
        "scala" => "Scala",
        "groovy" => "Groovy",
        "csharp" => "C#",
        "fsharp" => "F#",
        "vb" => "Visual Basic",
        "swift" => "Swift",
        "objective-c" => "Objective-C",
        "dart" => "Dart",
        "php" => "PHP",
        "ruby" => "Ruby",
        "lua" => "Lua",
        "perl" => "Perl",
        "r" => "R",
        "julia" => "Julia",
        "shell" => "Shell",
        "powershell" => "PowerShell",
        "batch" => "Batch",
        "html" => "HTML",
        "css" => "CSS",
        "sql" => "SQL",
        "assembly" => "Assembly",
        "solidity" => "Solidity",
        "zig" => "Zig",
        "nim" => "Nim",
        "verilog" => "Verilog",
        "vhdl" => "VHDL",
        "haskell" => "Haskell",
        "ocaml" => "OCaml",
        "elixir" => "Elixir",
        "erlang" => "Erlang",
        "clojure" => "Clojure",
        "elm" => "Elm",
        "wasm" => "WebAssembly",
        "cuda" => "CUDA",
        "glsl" => "GLSL",
        "hlsl" => "HLSL",
        "protobuf" => "Protocol Buffers",
        "graphql" => "GraphQL",
        _ => return None,
    })
}

/// Known packages: names as imported or declared (lower case; a trailing `/` or `.` matches a prefix), shown name, kind.
const KNOWN: &[(&[&str], &str, &str)] = &[
    (&["react", "react-dom"], "React", "framework"),
    (&["vue"], "Vue", "framework"),
    (&["svelte"], "Svelte", "framework"),
    (&["@angular/"], "Angular", "framework"),
    (&["next"], "Next.js", "framework"),
    (&["nuxt"], "Nuxt", "framework"),
    (&["electron"], "Electron", "framework"),
    (&["express"], "Express", "framework"),
    (&["koa"], "Koa", "framework"),
    (&["fastify"], "Fastify", "framework"),
    (&["@nestjs/"], "NestJS", "framework"),
    (&["@tauri-apps/"], "Tauri", "framework"),
    (&["solid-js"], "Solid", "framework"),
    (&["preact"], "Preact", "framework"),
    (&["hono"], "Hono", "framework"),
    (&["@codemirror/", "codemirror"], "CodeMirror", "library"),
    (&["@xterm/", "xterm"], "xterm.js", "library"),
    (&["monaco-editor"], "Monaco", "library"),
    (&["zod"], "Zod", "library"),
    (&["jquery"], "jQuery", "library"),
    (&["three"], "three.js", "library"),
    (&["d3"], "D3", "library"),
    (&["socket.io", "socket.io-client"], "Socket.IO", "library"),
    (&["@prisma/client", "prisma"], "Prisma", "library"),
    (&["mongoose"], "Mongoose", "library"),
    (&["sequelize"], "Sequelize", "library"),
    (&["typeorm"], "TypeORM", "library"),
    (&["redux", "@reduxjs/toolkit"], "Redux", "library"),
    (&["axios"], "Axios", "library"),
    (&["lodash"], "Lodash", "library"),
    (&["tailwindcss"], "Tailwind CSS", "library"),
    (&["antd"], "Ant Design", "library"),
    (&["@mui/"], "MUI", "library"),
    (&["element-plus"], "Element Plus", "library"),
    (&["ws"], "ws", "library"),
    (&["node-pty"], "node-pty", "library"),
    (&["@modelcontextprotocol/"], "MCP SDK", "library"),
    (&["openai"], "OpenAI SDK", "library"),
    (&["@anthropic-ai/"], "Anthropic SDK", "library"),
    (&["vite"], "Vite", "tool"),
    (&["webpack"], "webpack", "tool"),
    (&["esbuild"], "esbuild", "tool"),
    (&["rollup"], "Rollup", "tool"),
    (&["vitest"], "Vitest", "tool"),
    (&["jest"], "Jest", "tool"),
    (&["@playwright/test", "playwright", "playwright-core"], "Playwright", "tool"),
    (&["puppeteer"], "Puppeteer", "library"),
    (&["electron-builder"], "electron-builder", "tool"),
    (&["flask"], "Flask", "framework"),
    (&["django"], "Django", "framework"),
    (&["fastapi"], "FastAPI", "framework"),
    (&["tornado"], "Tornado", "framework"),
    (&["streamlit"], "Streamlit", "framework"),
    (&["gradio"], "Gradio", "framework"),
    (&["pwn", "pwntools"], "pwntools", "library"),
    (&["crypto", "cryptodome", "pycryptodome", "pycryptodomex"], "PyCryptodome", "library"),
    (&["z3", "z3-solver"], "Z3", "library"),
    (&["sympy"], "SymPy", "library"),
    (&["gmpy2"], "gmpy2", "library"),
    (&["sage"], "SageMath", "library"),
    (&["angr"], "angr", "library"),
    (&["scapy"], "Scapy", "library"),
    (&["capstone"], "Capstone", "library"),
    (&["unicorn"], "Unicorn", "library"),
    (&["keystone", "keystone-engine"], "Keystone", "library"),
    (&["libnum"], "libnum", "library"),
    (&["ropgadget"], "ROPgadget", "tool"),
    (&["numpy"], "NumPy", "library"),
    (&["pandas"], "pandas", "library"),
    (&["scipy"], "SciPy", "library"),
    (&["matplotlib"], "Matplotlib", "library"),
    (&["torch"], "PyTorch", "framework"),
    (&["tensorflow"], "TensorFlow", "framework"),
    (&["sklearn", "scikit-learn"], "scikit-learn", "library"),
    (&["transformers"], "Transformers", "library"),
    (&["requests"], "Requests", "library"),
    (&["httpx"], "HTTPX", "library"),
    (&["aiohttp"], "aiohttp", "library"),
    (&["bs4", "beautifulsoup4"], "Beautiful Soup", "library"),
    (&["selenium"], "Selenium", "library"),
    (&["sqlalchemy"], "SQLAlchemy", "library"),
    (&["pydantic"], "Pydantic", "library"),
    (&["pil", "pillow"], "Pillow", "library"),
    (&["cv2", "opencv-python"], "OpenCV", "library"),
    (&["pytest"], "pytest", "tool"),
    (&["click"], "Click", "library"),
    (&["typer"], "Typer", "library"),
    (&["github.com/gin-gonic/gin"], "Gin", "framework"),
    (&["github.com/labstack/echo"], "Echo", "framework"),
    (&["github.com/gofiber/fiber"], "Fiber", "framework"),
    (&["github.com/spf13/cobra"], "Cobra", "library"),
    (&["gorm.io/gorm"], "GORM", "library"),
    (&["google.golang.org/grpc"], "gRPC", "library"),
    (&["tokio"], "Tokio", "library"),
    (&["serde"], "Serde", "library"),
    (&["actix-web"], "Actix Web", "framework"),
    (&["axum"], "Axum", "framework"),
    (&["rocket"], "Rocket", "framework"),
    (&["clap"], "clap", "library"),
    (&["reqwest"], "reqwest", "library"),
    (&["tauri"], "Tauri", "framework"),
    (&["bevy"], "Bevy", "framework"),
    (&["org.springframework.", "org.springframework", "spring-boot-starter", "spring-boot-starter-web"], "Spring", "framework"),
    (&["junit", "org.junit.", "org.junit"], "JUnit", "tool"),
    (&["laravel/framework"], "Laravel", "framework"),
    (&["symfony/"], "Symfony", "framework"),
    (&["topthink/framework", "topthink/think"], "ThinkPHP", "framework"),
    (&["rails"], "Rails", "framework"),
    (&["sinatra"], "Sinatra", "framework"),
];

/// Files whose presence names a tool.
static TOOL_FILES: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    [
        (r"(?i)^(?:.*/)?dockerfile(?:\.[^/]+)?$", "Docker"),
        (r"(?i)^(?:.*/)?(?:docker-)?compose\.ya?ml$", "Docker Compose"),
        (r"(?i)^(?:.*/)?cmakelists\.txt$", "CMake"),
        (r"(?i)^(?:.*/)?(?:gnu)?makefile$", "Make"),
        (r"(?i)^\.github/workflows/", "GitHub Actions"),
        (r"(?i)^vite\.config\.[cm]?[jt]s$", "Vite"),
        (r"(?i)^webpack\.config\.[cm]?[jt]s$", "webpack"),
        (r"(?i)^electron-builder(?:\.config)?\.[a-z]+$", "electron-builder"),
        (r"(?i)^(?:.*/)?conftest\.py$|^pytest\.ini$", "pytest"),
        (r"(?i)^vitest\.config\.[cm]?[jt]s$", "Vitest"),
        (r"(?i)^jest\.config\.[cm]?[jt]s$", "Jest"),
        (r"(?i)^playwright\.config\.[cm]?[jt]s$", "Playwright"),
        (r"(?i)^(?:.*/)?build\.gradle(?:\.kts)?$", "Gradle"),
        (r"(?i)^(?:.*/)?pom\.xml$", "Maven"),
        (r"(?i)^(?:.*/)?\.?gitlab-ci\.yml$", "GitLab CI"),
        (r"(?i)^(?:.*/)?pnpm-workspace\.yaml$", "pnpm"),
    ]
    .into_iter()
    .map(|(pattern, tool)| (Regex::new(pattern).unwrap(), tool))
    .collect()
});

/// Manifests read for declared dependencies, by file name.
static MANIFESTS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:^|/)(?:package\.json|requirements[\w.-]*\.txt|pyproject\.toml|pipfile|setup\.py|go\.mod|cargo\.toml|pom\.xml|build\.gradle(?:\.kts)?|composer\.json|gemfile)$").unwrap()
});

fn lookup(name: &str) -> Option<(&'static str, &'static str)> {
    let lower = name.to_lowercase();
    KNOWN.iter().find_map(|(names, shown, kind)| {
        names.iter().any(|candidate| if candidate.ends_with('/') || candidate.ends_with('.') { lower.starts_with(candidate) } else { lower == *candidate }).then_some((*shown, *kind))
    })
}

macro_rules! regex {
    ($pattern:literal) => {{
        static PATTERN: LazyLock<Regex> = LazyLock::new(|| Regex::new($pattern).unwrap());
        &*PATTERN
    }};
}

/// Dependency names declared by one manifest, as their ecosystem writes them.
pub fn declared_dependencies(path: &str, text: &str) -> Vec<String> {
    let name = path[path.rfind('/').map_or(0, |slash| slash + 1)..].to_lowercase();
    let mut names = Vec::new();
    let mut add = |value: Option<&str>| {
        if let Some(clean) = value.map(str::trim).filter(|clean| !clean.is_empty() && clean.chars().count() <= 100) {
            names.push(clean.to_owned());
        }
    };
    if name == "package.json" || name == "composer.json" {
        if let Ok(serde_json::Value::Object(json)) = serde_json::from_str::<serde_json::Value>(text) {
            for field in ["dependencies", "devDependencies", "peerDependencies", "require", "require-dev"] {
                if let Some(serde_json::Value::Object(value)) = json.get(field) {
                    for key in value.keys() {
                        add(Some(key));
                    }
                }
            }
        }
    } else if name.starts_with("requirements") {
        for line in text.split('\n') {
            let line = line.split('#').next().unwrap_or("");
            add(regex!(r"^\s*([A-Za-z0-9][\w.-]*)").captures(line).and_then(|captures| captures.get(1)).map(|m| m.as_str()));
        }
    } else if name == "pyproject.toml" || name == "setup.py" || name == "pipfile" {
        for captures in regex!(r#"["']([A-Za-z0-9][\w.-]*)\s*(?:\[[^\]]*\])?\s*(?:[<>=!~;][^"']*)?["']"#).captures_iter(text) {
            add(captures.get(1).map(|m| m.as_str()));
        }
        if name == "pipfile" {
            for captures in regex!(r"(?m)^([A-Za-z0-9][\w.-]*)\s*=").captures_iter(text) {
                add(captures.get(1).map(|m| m.as_str()));
            }
        }
    } else if name == "go.mod" {
        for captures in regex!(r"(?m)^\s*(?:require\s+)?([a-z0-9.-]+\.[a-z]+/[\w./-]+)\s+v").captures_iter(text) {
            add(captures.get(1).map(|m| m.as_str()));
        }
    } else if name == "cargo.toml" {
        let mut in_dependencies = false;
        for line in text.split('\n') {
            if let Some(section) = regex!(r"^\s*\[([^\]]+)\]").captures(line) {
                in_dependencies = section[1].trim().ends_with("dependencies");
                continue;
            }
            if in_dependencies {
                add(regex!(r"^\s*([A-Za-z0-9_-]+)\s*=").captures(line).and_then(|captures| captures.get(1)).map(|m| m.as_str()));
            }
        }
    } else if name == "pom.xml" {
        for captures in regex!(r"(?s)<dependency>.*?<groupId>([^<]+)</groupId>.*?<artifactId>([^<]+)</artifactId>").captures_iter(text) {
            add(captures.get(1).map(|m| m.as_str()));
            add(captures.get(2).map(|m| m.as_str()));
        }
    } else if name.starts_with("build.gradle") {
        for captures in regex!(r#"['"]([\w.-]+):([\w.-]+)(?::[^'"]*)?['"]"#).captures_iter(text) {
            add(captures.get(1).map(|m| m.as_str()));
            add(captures.get(2).map(|m| m.as_str()));
        }
    } else if name == "gemfile" {
        for captures in regex!(r#"(?m)^\s*gem\s+['"]([^'"]+)['"]"#).captures_iter(text) {
            add(captures.get(1).map(|m| m.as_str()));
        }
    }
    names
}

/// Whether a path names a dependency manifest the stack reads.
pub fn is_manifest(path: &str) -> bool {
    MANIFESTS.is_match(path)
}

fn rank(kind: &str) -> u8 {
    match kind {
        "language" => 0,
        "framework" => 1,
        "library" => 2,
        _ => 3,
    }
}

/// The project's stack: languages first, then frameworks, libraries and tools.
pub fn detect_stack(files: &[MapFile], packages: &[MapPackage], declared: &[String]) -> Vec<StackItem> {
    let mut language_lines: Vec<(&str, u64)> = Vec::new();
    let mut code_lines = 0u64;
    for file in files {
        if file.kind != "code" || language_name(&file.language).is_none() {
            continue;
        }
        match language_lines.iter_mut().find(|(language, _)| *language == file.language) {
            Some(entry) => entry.1 += file.lines,
            None => language_lines.push((&file.language, file.lines)),
        }
        code_lines += file.lines;
    }
    language_lines.sort_by(|left, right| right.1.cmp(&left.1));
    let languages: Vec<StackItem> = language_lines
        .iter()
        .enumerate()
        .filter(|(index, (_, lines))| *index == 0 || *lines as f64 >= code_lines as f64 * 0.08)
        .take(4)
        .map(|(_, (language, _))| StackItem { name: language_name(language).unwrap_or_default().to_owned(), kind: "language".into() })
        .collect();
    let mut usage: Vec<(String, &str, u64)> = Vec::new();
    let mut note = |name: &str, kind: &'static str, score: u64| match usage.iter_mut().find(|(known, _, _)| known == name) {
        Some(entry) => entry.2 += score,
        None => usage.push((name.to_owned(), kind, score)),
    };
    for item in packages {
        if let Some((shown, kind)) = lookup(&item.name) {
            note(shown, kind, 1 + item.files.len() as u64);
        }
    }
    for name in declared {
        if let Some((shown, kind)) = lookup(name) {
            note(shown, kind, 1);
        }
    }
    for file in files {
        for (pattern, tool) in TOOL_FILES.iter() {
            if pattern.is_match(&file.path) {
                note(tool, "tool", 1);
            }
        }
    }
    let shown: Vec<&str> = languages.iter().map(|item| item.name.as_str()).collect();
    let mut rest: Vec<(String, &str, u64)> = usage.into_iter().filter(|(name, _, _)| !shown.contains(&name.as_str())).collect();
    rest.sort_by(|left, right| rank(left.1).cmp(&rank(right.1)).then(right.2.cmp(&left.2)).then(left.0.cmp(&right.0)));
    let mut rest: Vec<StackItem> = rest.into_iter().map(|(name, kind, _)| StackItem { name, kind: kind.to_owned() }).collect();
    // Without any known library, the most imported external packages say the most.
    if !rest.iter().any(|item| item.kind == "library" || item.kind == "framework") {
        let mut popular: Vec<&MapPackage> = packages.iter().filter(|item| item.files.len() >= 2).collect();
        popular.sort_by(|left, right| right.files.len().cmp(&left.files.len()));
        let popular = popular.into_iter().take(4).map(|item| StackItem { name: item.name.clone(), kind: "library".into() });
        rest.splice(0..0, popular);
    }
    languages.into_iter().chain(rest).take(16).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_manifests() {
        assert_eq!(declared_dependencies("package.json", r#"{"dependencies":{"react":"1"},"devDependencies":{"vite":"1"}}"#), ["react", "vite"]);
        assert_eq!(declared_dependencies("requirements.txt", "numpy==1\n# x\nflask>=2 # web\n"), ["numpy", "flask"]);
        assert_eq!(declared_dependencies("a/Cargo.toml", "[package]\nname = \"x\"\n[dependencies]\ntokio = \"1\"\n[dev-dependencies]\nserde = \"1\"\n"), ["tokio", "serde"]);
        assert!(is_manifest("sub/pyproject.toml"));
    }
}
