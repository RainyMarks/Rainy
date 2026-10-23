//! Skills (3.x `skills.ts`): `SKILL.md` files the model reads when relevant; the system prompt lists only their
//! descriptions.

use std::path::Path;

use serde::Serialize;

use super::budget::estimate_text;

/// Largest SKILL.md that is listed.
const MAX_SKILL_BYTES: u64 = 256 * 1024;
/// Bytes of a description kept in the prompt.
const MAX_DESCRIPTION_BYTES: usize = 512;
/// Tokens all skill descriptors together may use.
pub const PROMPT_TOKENS: u64 = 4096;

/// A discovered skill, as Settings lists it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Skill {
    pub id: String,
    /// `project` or `user`.
    pub scope: &'static str,
    pub name: String,
    pub description: String,
    pub path: String,
}

fn truncate_bytes(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

/// A front-matter scalar: plain, quoted, or a `|` / `>` block of the indented lines that follow.
fn front_value(lines: &[&str], index: usize, raw: &str) -> String {
    let raw = raw.trim();
    if raw == "|" || raw == ">" || raw.starts_with("|-") || raw.starts_with(">-") || raw.starts_with("|+") || raw.starts_with(">+") {
        let block: Vec<&str> = lines[index + 1..].iter().take_while(|line| line.is_empty() || line.starts_with(' ') || line.starts_with('\t')).map(|line| line.trim()).collect();
        return if raw.starts_with('|') { block.join("\n").trim().to_owned() } else { block.join(" ").split_whitespace().collect::<Vec<_>>().join(" ") };
    }
    if raw.len() >= 2 && ((raw.starts_with('"') && raw.ends_with('"')) || (raw.starts_with('\'') && raw.ends_with('\''))) {
        let inner = &raw[1..raw.len() - 1];
        return if raw.starts_with('"') { serde_json::from_str::<String>(raw).unwrap_or_else(|_| inner.to_owned()) } else { inner.replace("''", "'") };
    }
    raw.to_owned()
}

/// Name and description of a SKILL.md: front-matter fields, else the first heading and the first paragraph.
pub fn describe(text: &str, fallback_name: &str) -> (String, String) {
    let text = text.replace("\r\n", "\n");
    let mut body = text.as_str();
    let (mut name, mut description) = (fallback_name.to_owned(), String::new());
    if let Some(rest) = text.strip_prefix("---\n")
        && let Some(end) = rest.find("\n---")
    {
        let lines: Vec<&str> = rest[..end].split('\n').collect();
        for (index, line) in lines.iter().enumerate() {
            if let Some(value) = line.strip_prefix("name:") {
                let value = front_value(&lines, index, value);
                if !value.trim().is_empty() {
                    name = value.trim().to_owned();
                }
            } else if let Some(value) = line.strip_prefix("description:") {
                description = front_value(&lines, index, value).trim().to_owned();
            }
        }
        let after = &rest[end + 4..];
        body = after.strip_prefix('\n').unwrap_or(after);
    }
    if description.is_empty() {
        description = body.split("\n\n").map(str::trim).find(|part| !part.is_empty() && !part.starts_with('#')).map(|part| part.split_whitespace().collect::<Vec<_>>().join(" ")).unwrap_or_default();
    }
    (name, truncate_bytes(&description, MAX_DESCRIPTION_BYTES))
}

fn scan(root: &Path, scope: &'static str) -> Vec<Skill> {
    let Ok(entries) = std::fs::read_dir(root) else { return Vec::new() };
    let mut names: Vec<String> = entries.flatten().map(|entry| entry.file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    let mut skills = Vec::new();
    for directory in names {
        let path = root.join(&directory).join("SKILL.md");
        let Ok(info) = std::fs::metadata(&path) else { continue };
        if !info.is_file() || info.len() > MAX_SKILL_BYTES {
            continue;
        }
        match std::fs::read(&path) {
            Ok(bytes) => {
                let (name, description) = describe(&String::from_utf8_lossy(&bytes), &directory);
                skills.push(Skill { id: format!("{scope}/{directory}"), scope, name, description, path: path.to_string_lossy().into_owned() });
            }
            Err(error) => log::warn!("skill {}: {error}", path.display()),
        }
    }
    skills
}

/// Skills for a working directory: the project's `<cwd>/.rainy/skills`, then the user's `~/.rainy-agent/skills`.
pub fn discover(cwd: Option<&Path>) -> Vec<Skill> {
    let mut skills = cwd.map(|cwd| scan(&cwd.join(".rainy").join("skills"), "project")).unwrap_or_default();
    skills.extend(scan(&crate::env::home_dir().join(".rainy-agent").join("skills"), "user"));
    skills
}

/// The system-prompt text listing skills, within [`PROMPT_TOKENS`].
pub fn section(skills: &[Skill]) -> String {
    let mut lines = Vec::new();
    let mut tokens = 0;
    for skill in skills {
        let text = format!("Skill \"{}\": {}\nRead \"{}\" for the full instructions when this skill is relevant.", skill.id, if skill.description.is_empty() { &skill.name } else { &skill.description }, skill.path);
        let cost = estimate_text(&text);
        if tokens + cost > PROMPT_TOKENS {
            break;
        }
        tokens += cost;
        lines.push(text);
    }
    lines.join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_front_matter_and_bodies() {
        assert_eq!(describe("---\nname: \"PDF tools\"\ndescription: >\n  Fill forms\n  and merge files.\n---\n# x\n", "pdf"), ("PDF tools".into(), "Fill forms and merge files.".into()));
        assert_eq!(describe("# Title\n\nFirst   paragraph\nhere.\n\nSecond.", "dir"), ("dir".into(), "First paragraph here.".into()));
        assert_eq!(describe("---\nname: x\n---\nBody text.", "d"), ("x".into(), "Body text.".into()));
    }
}
