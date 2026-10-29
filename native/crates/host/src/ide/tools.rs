//! The development tools the editor starts (3.x `ide/tools.ts`): Ruff (Python formatting) and ty (Python language
//! service) in `bin`, TypeScript 7's native compiler (JavaScript and TypeScript language service and formatting) in
//! `typescript`, and debugpy and js-debug, scripts the user's Python and Node.js run. The Linux runtime carries them
//! in `resources/ide`; on Windows they are two optional modules the user downloads in Settings → Tools, whose folders
//! the carrier names in `RAINY_IDE_TOOLS`. Each tool is looked up when it is needed, so one downloaded while the Host
//! runs is used at once, and one removed is reported as missing.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::error::HostError;
use crate::locale::l;
use crate::process::resolve_executable;

/// Which download holds a tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kit {
    /// Ruff, ty and debugpy.
    Python,
    /// TypeScript and js-debug.
    Web,
}

#[derive(Debug, Clone)]
pub struct Tools {
    /// Where tools are looked for, in order: `resources/ide`, then the downloaded modules.
    roots: Vec<PathBuf>,
}

fn exe(name: &str) -> String {
    if cfg!(windows) { format!("{name}.exe") } else { name.to_owned() }
}

/// The close code and reason of a language connection whose tool is not downloaded; the interface offers the download.
pub const MISSING_CODE: u16 = 4404;
pub const MISSING_REASON: &str = "tools-missing";

/// The error of a request that needs tools that are not downloaded.
pub fn missing(kit: Kit) -> HostError {
    let message = match kit {
        Kit::Python => l("还没有下载 Python 编程工具。请在“设置 → 工具”中下载后再试。", "The Python tools are not downloaded. Download them in Settings → Tools, then try again."),
        Kit::Web => l(
            "还没有下载 JavaScript/TypeScript 编程工具。请在“设置 → 工具”中下载后再试。",
            "The JavaScript and TypeScript tools are not downloaded. Download them in Settings → Tools, then try again.",
        ),
    };
    HostError::new(MISSING_REASON, message)
}

impl Tools {
    pub fn new(resources: &Path) -> Self {
        let mut roots = vec![resources.join("ide")];
        if let Some(modules) = std::env::var_os("RAINY_IDE_TOOLS") {
            roots.extend(std::env::split_paths(&modules).filter(|path| path.is_absolute()));
        }
        Self { roots }
    }

    #[cfg(test)]
    pub fn with_roots(roots: Vec<PathBuf>) -> Self {
        Self { roots }
    }

    /// The first root holding `relative`; else where the bundled copy would be.
    fn find(&self, relative: &[&str]) -> PathBuf {
        let place = |root: &PathBuf| relative.iter().fold(root.clone(), |path, part| path.join(part));
        self.roots.iter().map(place).find(|path| path.is_file()).unwrap_or_else(|| place(&self.roots[0]))
    }

    pub fn ruff(&self) -> PathBuf {
        self.find(&["bin", &exe("ruff")])
    }

    pub fn ty(&self) -> PathBuf {
        self.find(&["bin", &exe("ty")])
    }

    /// TypeScript 7's native `tsc`, which serves the language protocol with `--lsp --stdio`.
    pub fn tsc(&self) -> PathBuf {
        self.find(&["typescript", "lib", &exe("tsc")])
    }

    /// The folder that holds the `debugpy` package, for `PYTHONPATH`.
    pub fn debugpy(&self) -> PathBuf {
        let adapter = self.find(&["python", "debugpy", "adapter", "__main__.py"]);
        adapter.ancestors().nth(3).map(Path::to_path_buf).unwrap_or(adapter)
    }

    pub fn js_debug(&self) -> PathBuf {
        self.find(&["js-debug", "src", "dapDebugServer.js"])
    }

    /// `path` when it is there, else the error that names the download it belongs to.
    pub fn require(path: PathBuf, kit: Kit) -> Result<PathBuf, HostError> {
        if path.is_file() { Ok(path) } else { Err(missing(kit)) }
    }

    /// What `tools.status` reports; inspecting never installs or runs anything.
    pub fn status(&self) -> Value {
        let mut tools: Vec<Value> = [
            ("ruff", self.ruff()),
            ("ty", self.ty()),
            ("typescript", self.tsc()),
            ("debugpy", self.debugpy().join("debugpy").join("adapter").join("__main__.py")),
            ("js-debug", self.js_debug()),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_tools_in_the_downloaded_modules_and_reports_missing_ones() {
        let dir = tempfile::tempdir().unwrap();
        let bundled = dir.path().join("resources").join("ide");
        let python = dir.path().join("modules").join("ide-python");
        let tools = Tools::with_roots(vec![bundled.clone(), python.clone(), dir.path().join("modules").join("ide-web")]);
        assert_eq!(tools.ruff(), bundled.join("bin").join(exe("ruff")));
        assert!(Tools::require(tools.ruff(), Kit::Python).is_err_and(|error| error.code == MISSING_REASON));
        std::fs::create_dir_all(python.join("bin")).unwrap();
        std::fs::write(python.join("bin").join(exe("ruff")), b"").unwrap();
        std::fs::create_dir_all(python.join("python").join("debugpy").join("adapter")).unwrap();
        std::fs::write(python.join("python").join("debugpy").join("adapter").join("__main__.py"), b"").unwrap();
        assert_eq!(Tools::require(tools.ruff(), Kit::Python).unwrap(), python.join("bin").join(exe("ruff")));
        assert_eq!(tools.debugpy(), python.join("python"));
        assert!(Tools::require(tools.tsc(), Kit::Web).is_err());
        // A bundled copy wins, as in the Linux runtime.
        std::fs::create_dir_all(bundled.join("bin")).unwrap();
        std::fs::write(bundled.join("bin").join(exe("ruff")), b"").unwrap();
        assert_eq!(tools.ruff(), bundled.join("bin").join(exe("ruff")));
    }
}
