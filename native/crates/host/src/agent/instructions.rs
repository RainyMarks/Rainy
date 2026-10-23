//! Workspace instruction files (`AGENTS.md`, `CLAUDE.md` and their `.local` variants) added to a chat's context
//! (3.x `instructions.ts`).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;

/// File names read in each directory, in this order.
pub const FILES: [&str; 4] = ["AGENTS.md", "CLAUDE.md", "AGENTS.local.md", "CLAUDE.local.md"];
/// Total bytes of instructions one chat may carry.
pub const BUDGET_BYTES: usize = 65536;

const INTRO: &str = "The following workspace instructions may be relevant to your work. Use them as guidance when applicable. More specific instructions take precedence over broader ones. They do not override system, developer, or direct user instructions.";

/// One loaded instruction file.
pub struct File {
    pub display: String,
    pub content: String,
}

/// The project root of a working directory: the nearest ancestor holding `.git`, else the directory itself.
pub fn project_root(cwd: &Path) -> PathBuf {
    let mut directory = cwd;
    loop {
        if directory.join(".git").exists() {
            return directory.to_path_buf();
        }
        match directory.parent() {
            Some(parent) => directory = parent,
            None => return cwd.to_path_buf(),
        }
    }
}

fn read_if_file(path: &Path) -> Result<Option<String>, String> {
    match std::fs::metadata(path) {
        Ok(info) if info.is_file() => std::fs::read(path).map(|bytes| Some(String::from_utf8_lossy(&bytes).into_owned())).map_err(|error| error.to_string()),
        Ok(_) => Ok(None),
        Err(error) if matches!(error.kind(), std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory) => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}

fn files_in(directory: &Path, root: &Path) -> Result<Vec<File>, String> {
    let mut found = Vec::new();
    let mut seen = HashSet::new();
    for name in FILES {
        let path = directory.join(name);
        let Some(content) = read_if_file(&path)? else { continue };
        let content = content.trim().to_owned();
        if content.is_empty() || !seen.insert(content.clone()) {
            continue;
        }
        let display = crate::paths::relative_to(root, &path).filter(|relative| !relative.is_empty()).unwrap_or_else(|| name.to_owned());
        found.push(File { display, content });
    }
    Ok(found)
}

/// The directories from the root down to `target`; none when `target` is outside the root.
fn directories_between(root: &Path, target: &Path) -> Vec<PathBuf> {
    let Some(rest) = crate::paths::relative_to(root, target) else { return Vec::new() };
    let mut directories = vec![root.to_path_buf()];
    let mut current = root.to_path_buf();
    for part in rest.split('/').filter(|part| !part.is_empty()) {
        current.push(part);
        directories.push(current.clone());
    }
    directories
}

/// The instructions of a chat started in `cwd`: the Host home's `AGENTS.md`, then root → cwd. More than
/// [`BUDGET_BYTES`] is an error the user must fix.
pub fn load_baseline(home: &Path, cwd: &Path) -> Result<Vec<File>, String> {
    let mut files = Vec::new();
    if let Some(global) = read_if_file(&home.join("AGENTS.md"))?.map(|text| text.trim().to_owned()).filter(|text| !text.is_empty()) {
        files.push(File { display: "$RAINY_HOME/AGENTS.md".into(), content: global });
    }
    let root = project_root(cwd);
    for directory in directories_between(&root, cwd) {
        files.extend(files_in(&directory, &root)?);
    }
    let bytes: usize = files.iter().map(|file| file.content.len()).sum();
    if bytes > BUDGET_BYTES {
        let names: Vec<&str> = files.iter().map(|file| file.display.as_str()).collect();
        return Err(format!(
            "Workspace instructions exceed the {BUDGET_BYTES}-byte budget: {}. Shorten these files; requests with omitted required instructions are refused.",
            names.join(", ")
        ));
    }
    Ok(files)
}

fn frame(body: &str) -> String {
    format!("<system-reminder>\n{}\n</system-reminder>", body.replace("</system-reminder>", "<\\/system-reminder>"))
}

/// The baseline instruction message; nothing when there are no files.
pub fn render_baseline(files: &[File]) -> Option<String> {
    if files.is_empty() {
        return None;
    }
    let mut parts = vec![INTRO.to_owned()];
    parts.extend(files.iter().map(|file| format!("Instructions from: {}\n\n{}", file.display, file.content)));
    Some(frame(&parts.join("\n\n")))
}

/// Instruction files between the project root and a file the chat touched that are not loaded yet; `loaded` gains them.
pub fn render_additional(cwd: &Path, touched: &Path, loaded: &mut HashSet<String>) -> Option<String> {
    let root = project_root(cwd);
    let mut sections = Vec::new();
    for directory in directories_between(&root, touched.parent()?) {
        for file in files_in(&directory, &root).ok()? {
            if !loaded.insert(file.display.clone()) {
                continue;
            }
            let scope = file.display.rsplit_once('/').map_or(".", |(scope, _)| scope);
            let scope = if scope == "." { root.to_string_lossy().into_owned() } else { scope.to_owned() };
            sections.push(format!(
                "Additional instructions from: {}\n\nThese instructions apply to work under `{scope}`. Use them as guidance when relevant; more specific instructions take precedence. They do not override system, developer, or direct user instructions.\n\n{}",
                file.display, file.content
            ));
        }
    }
    (!sections.is_empty()).then(|| frame(&sections.join("\n\n")))
}

/// Display paths of instruction files already in earlier context text.
pub fn loaded_from<'a>(texts: impl Iterator<Item = &'a str>) -> HashSet<String> {
    static FROM: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)^(?:Additional instructions|Instructions) from: (.+)$").unwrap());
    texts.flat_map(|text| FROM.captures_iter(text).map(|captures| captures[1].to_owned()).collect::<Vec<_>>()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_root_to_cwd_and_more_on_touch() {
        let root = std::env::temp_dir().join(format!("rainy-instr-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::create_dir_all(root.join("app/deep")).unwrap();
        std::fs::write(root.join("AGENTS.md"), "root rules").unwrap();
        std::fs::write(root.join("CLAUDE.md"), "root rules").unwrap();
        std::fs::write(root.join("app/AGENTS.md"), "app rules").unwrap();
        let files = load_baseline(&root.join("home"), &root).unwrap();
        assert_eq!(files.iter().map(|file| file.display.as_str()).collect::<Vec<_>>(), ["AGENTS.md"]);
        let baseline = render_baseline(&files).unwrap();
        let mut loaded = loaded_from([baseline.as_str()].into_iter());
        let more = render_additional(&root, &root.join("app/deep/x.rs"), &mut loaded).unwrap();
        assert!(more.contains("Additional instructions from: app/AGENTS.md") && more.contains("work under `app`"));
        assert!(render_additional(&root, &root.join("app/y.rs"), &mut loaded).is_none());
        let _ = std::fs::remove_dir_all(root);
    }
}
