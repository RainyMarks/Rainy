//! The development tools the editor starts (3.x `ide/tools.ts`). Rainy ships native ones in `resources/ide/bin`: Ruff
//! (Python formatting), ty (Python language service), TypeScript 7's native compiler (JavaScript and TypeScript
//! language service) and Biome (JavaScript and TypeScript formatting); debugpy and js-debug are scripts the user's
//! Python and Node.js run.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::process::resolve_executable;

#[derive(Debug, Clone)]
pub struct Tools {
    /// `resources/ide`.
    pub root: PathBuf,
}

fn exe(name: &str) -> String {
    if cfg!(windows) { format!("{name}.exe") } else { name.to_owned() }
}

impl Tools {
    pub fn new(resources: &Path) -> Self {
        Self { root: resources.join("ide") }
    }

    fn bin(&self, name: &str) -> PathBuf {
        self.root.join("bin").join(exe(name))
    }

    pub fn ruff(&self) -> PathBuf {
        self.bin("ruff")
    }

    pub fn ty(&self) -> PathBuf {
        self.bin("ty")
    }

    pub fn biome(&self) -> PathBuf {
        self.bin("biome")
    }

    /// TypeScript 7's native `tsc`, which serves the language protocol with `--lsp --stdio`.
    pub fn tsc(&self) -> PathBuf {
        self.root.join("typescript").join("lib").join(exe("tsc"))
    }

    /// What `tools.status` reports; inspecting never installs or runs anything.
    pub fn status(&self) -> Value {
        let mut tools: Vec<Value> = [
            ("ruff", self.ruff()),
            ("ty", self.ty()),
            ("typescript", self.tsc()),
            ("biome", self.biome()),
            ("debugpy", self.root.join("python").join("debugpy").join("adapter").join("__main__.py")),
            ("js-debug", self.root.join("js-debug").join("src").join("dapDebugServer.js")),
        ]
        .into_iter()
        .map(|(name, path)| {
            let ready = path.is_file();
            json!({ "name": name, "path": ready.then(|| path.display().to_string()), "ready": ready })
        })
        .collect();
        for name in ["node", "python3", "gcc", "g++", "gdb", "cmake", "ninja", "clangd", "clang-format"] {
            let found = resolve_executable(name, None);
            tools.push(json!({ "name": name, "path": found.as_ref().map(|path| path.display().to_string()), "ready": found.is_some() }));
        }
        let ubuntu = std::fs::read_to_string("/etc/os-release").ok().filter(|text| text.lines().any(|line| line == "ID=ubuntu")).and_then(|text| {
            text.lines().find_map(|line| line.strip_prefix("VERSION_ID=").map(|value| value.trim_matches('"').to_owned()))
        });
        json!({
            "platform": if cfg!(windows) { "win32" } else { "linux" },
            "ubuntuVersion": ubuntu,
            "offlineInstallSupported": cfg!(not(windows)) && ubuntu.as_deref() == Some("26.04"),
            "tools": tools,
        })
    }
}
