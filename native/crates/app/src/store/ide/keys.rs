//! Multi-root file keys, language detection, and absolute-path workspace matching.

use rainy_protocol::{IdeWorkspace, WorkspaceRoot};

const PREFIX: char = '\0';

/// A root-qualified file key.
pub fn file_key(path: &str, root: Option<&WorkspaceRoot>) -> String {
    match root {
        Some(root) if !root.primary => root_key(Some(&root.root_id), path),
        _ => path.to_owned(),
    }
}

/// The key of a path under the root the Host names by id: none, empty or `primary` is the primary root.
pub fn root_key(root: Option<&str>, path: &str) -> String {
    match root {
        Some(root) if !root.is_empty() && root != "primary" => format!("{PREFIX}{root}/{path}"),
        _ => path.to_owned(),
    }
}

/// The root id (none for the primary root) and relative path of a key.
pub fn split_key(key: &str) -> (Option<&str>, &str) {
    match key.strip_prefix(PREFIX) {
        Some(rest) => match rest.split_once('/') {
            Some((root, path)) => (Some(root), path),
            None => (Some(rest), ""),
        },
        None => (None, key),
    }
}

/// The key of a child of a directory key.
pub fn child_key(directory: &str, name: &str) -> String {
    let (root, path) = split_key(directory);
    let path = if path.is_empty() { name.to_owned() } else { format!("{path}/{name}") };
    root_key(root, &path)
}

/// The key of the directory holding a key.
pub fn parent_key(key: &str) -> String {
    let (root, path) = split_key(key);
    let parent = path.rsplit_once('/').map_or("", |(parent, _)| parent);
    root_key(root, parent)
}

/// The file name of a key.
pub fn file_name(key: &str) -> &str {
    let (_, path) = split_key(key);
    path.rsplit('/').next().unwrap_or(path)
}

/// The editor language of a path, in GPUI Kit's names; plain text when unknown.
pub fn language_of(path: &str) -> &'static str {
    let extension = path.rsplit('.').next().unwrap_or_default().to_ascii_lowercase();
    match extension.as_str() {
        "rs" => "rust",
        "py" | "pyi" => "python",
        "js" | "mjs" | "cjs" | "jsx" => "javascript",
        "ts" | "mts" | "cts" => "typescript",
        "tsx" => "tsx",
        "c" | "h" => "c",
        "cc" | "cpp" | "cxx" | "hpp" | "hh" => "cpp",
        "cs" => "csharp",
        "go" => "go",
        "java" => "java",
        "kt" | "kts" => "kotlin",
        "json" | "jsonc" => "json",
        "md" | "markdown" => "markdown",
        "yml" | "yaml" => "yaml",
        "toml" => "toml",
        "html" | "htm" => "html",
        "css" => "css",
        "scss" => "scss",
        "sh" | "bash" | "zsh" => "bash",
        "ps1" | "psm1" => "powershell",
        "sql" => "sql",
        "php" => "php",
        "rb" => "ruby",
        "lua" => "lua",
        "swift" => "swift",
        "scala" => "scala",
        "zig" => "zig",
        "xml" | "svg" => "xml",
        "diff" | "patch" => "diff",
        _ => "text",
    }
}

/// The folder of a Host path in the path's own spelling (`parentDirectory`); a file at the root of a drive gives the
/// drive's root.
pub fn parent_directory(path: &str) -> String {
    let Some(separator) = path.rfind(['/', '\\']) else { return path.to_owned() };
    let parent = &path[..separator];
    let drive = parent.len() == 2 && parent.as_bytes()[0].is_ascii_alphabetic() && parent.as_bytes()[1] == b':';
    if drive {
        format!("{parent}\\")
    } else if parent.is_empty() {
        "/".to_owned()
    } else {
        parent.to_owned()
    }
}

/// The key of an absolute Host path inside one of a project's folders, the most specific folder first.
pub fn key_in(workspace: &IdeWorkspace, path: &str) -> Option<String> {
    let candidate = path.replace('\\', "/");
    let windows = candidate.as_bytes().get(1) == Some(&b':') || candidate.starts_with("//");
    let fold = |value: &str| if windows { value.to_lowercase() } else { value.to_owned() };
    let mut roots = workspace.roots();
    roots.sort_by_key(|root| std::cmp::Reverse(root.path.len()));
    roots.into_iter().find_map(|root| {
        let prefix = format!("{}/", root.path.replace('\\', "/").trim_end_matches('/'));
        let (head, rest) = candidate.split_at_checked(prefix.len())?;
        (fold(head) == fold(&prefix)).then(|| file_key(rest, Some(&root)))
    })
}

/// The project whose folder holds a path most closely, with the path's key there.
pub fn holding_workspace(workspaces: &[IdeWorkspace], path: &str) -> Option<(IdeWorkspace, String)> {
    workspaces
        .iter()
        .filter_map(|workspace| Some((workspace.clone(), key_in(workspace, path)?)))
        .min_by_key(|(_, key)| split_key(key).1.len())
}
