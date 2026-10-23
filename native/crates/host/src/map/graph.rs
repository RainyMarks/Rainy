//! Architecture of a project map (3.x `shared/map-graph.ts`): the boxes one directory (the scope) is split into, and the
//! imports between them. The page draws the same boxes; the Host builds them to ask the AI to describe them.

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use regex::Regex;

use super::{MapFile, ProjectMap};

/// Boxes the automatic split aims for at most.
pub const MAX_NODES: usize = 16;
/// A scope with at most this many files shows every file as a box.
pub const FILE_LEVEL_MAX: usize = 24;
/// Boxes outside the scope kept, by import count.
const MAX_OUTSIDE: usize = 8;
/// A directory holding more than this share of the scope's code is split into its parts.
const SPLIT_SHARE: f64 = 0.35;

static TEST_DIRECTORY: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^(?:tests?|__tests__|specs?|e2e|testing)$").unwrap());
static DOC_DIRECTORY: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^(?:docs?|documentation)$").unwrap());

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum NodeType {
    Directory,
    Loose,
    File,
}

/// A box: a directory, a single file, or the loose files directly inside a directory.
#[derive(Clone, Debug)]
pub struct MapNode {
    /// A directory or file path; the loose files of directory `d` are `d/*` (`*` at the top).
    pub id: String,
    pub node_type: NodeType,
    /// Outside the scope: a directory or file the scope's files import or are imported by.
    pub outside: bool,
    /// The kind most of its lines are.
    pub kind: String,
    /// Indexes into the map's files.
    pub files: Vec<usize>,
    pub lines: u64,
    /// Lines per language, largest first.
    pub languages: Vec<(String, u64)>,
    /// A directory with more than one file can be opened as the next scope.
    pub openable: bool,
}

/// Imports from the files of one box to the files of another, counted in importing file pairs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MapEdge {
    pub from: String,
    pub to: String,
    pub weight: u64,
}

/// The boxes and imports of one scope ('' for the whole project).
pub struct MapView {
    pub nodes: Vec<MapNode>,
    pub edges: Vec<MapEdge>,
}

/// Whether `path` lies inside directory `scope` ('' contains everything).
pub fn in_scope(scope: &str, path: &str) -> bool {
    scope.is_empty() || path.strip_prefix(scope).is_some_and(|rest| rest.starts_with('/'))
}

fn loose_id(directory: &str) -> String {
    if directory.is_empty() { "*".into() } else { format!("{directory}/*") }
}

/// How much a file counts when sizing and splitting: its lines, and at least 1.
fn file_weight(file: &MapFile) -> u64 {
    file.lines.max(1)
}

fn code_weight(file: &MapFile) -> u64 {
    if file.kind == "code" || file.kind == "test" { file_weight(file) } else { 0 }
}

/// The box a path belongs to just outside a scope: the first directory or file where the path leaves the scope's
/// ancestors. For scope `src/app`, `src/lib/x.ts` belongs to `src/lib` and `package.json` to itself.
pub fn outside_id(scope: &str, path: &str) -> String {
    let scope_parts: Vec<&str> = if scope.is_empty() { Vec::new() } else { scope.split('/').collect() };
    let parts: Vec<&str> = path.split('/').collect();
    let mut common = 0;
    while common < scope_parts.len() && common + 1 < parts.len() && scope_parts[common] == parts[common] {
        common += 1;
    }
    parts[..common + 1].join("/")
}

#[derive(Clone, Debug, PartialEq)]
struct Group {
    id: String,
    node_type: NodeType,
    files: Vec<usize>,
}

/// Split the files of `directory` into its subdirectories and its loose files.
fn split(map: &ProjectMap, directory: &str, files: &[usize], loose_limit: usize) -> Vec<Group> {
    let mut directories: Vec<(String, Vec<usize>)> = Vec::new();
    let mut positions: HashMap<String, usize> = HashMap::new();
    let mut loose = Vec::new();
    let offset = if directory.is_empty() { 0 } else { directory.len() + 1 };
    for &index in files {
        let rest = &map.files[index].path[offset..];
        let Some(slash) = rest.find('/') else {
            loose.push(index);
            continue;
        };
        let id = format!("{}{}", if directory.is_empty() { String::new() } else { format!("{directory}/") }, &rest[..slash]);
        match positions.get(&id) {
            Some(&position) => directories[position].1.push(index),
            None => {
                positions.insert(id.clone(), directories.len());
                directories.push((id, vec![index]));
            }
        }
    }
    let mut groups: Vec<Group> = directories.into_iter().map(|(id, files)| Group { id, node_type: NodeType::Directory, files }).collect();
    if loose.len() > loose_limit {
        groups.push(Group { id: loose_id(directory), node_type: NodeType::Loose, files: loose });
    } else {
        groups.extend(loose.into_iter().map(|index| Group { id: map.files[index].path.clone(), node_type: NodeType::File, files: vec![index] }));
    }
    groups
}

fn weight_of(map: &ProjectMap, group: &Group) -> u64 {
    group.files.iter().map(|&index| code_weight(&map.files[index])).sum()
}

fn describe(map: &ProjectMap, group: Group, outside: bool) -> MapNode {
    let mut kinds: Vec<(&str, u64)> = Vec::new();
    let mut languages: Vec<(String, u64)> = Vec::new();
    let mut lines = 0;
    for &index in &group.files {
        let file = &map.files[index];
        lines += file.lines;
        let weight = if file.kind == "asset" || file.kind == "other" { 1 } else { file_weight(file) };
        match kinds.iter_mut().find(|(kind, _)| *kind == file.kind) {
            Some(entry) => entry.1 += weight,
            None => kinds.push((&file.kind, weight)),
        }
        if !file.language.is_empty() && file.kind != "asset" {
            match languages.iter_mut().find(|(language, _)| *language == file.language) {
                Some(entry) => entry.1 += file.lines,
                None => languages.push((file.language.clone(), file.lines)),
            }
        }
    }
    let name = &group.id[group.id.rfind('/').map_or(0, |slash| slash + 1)..];
    kinds.sort_by(|left, right| right.1.cmp(&left.1));
    let mut kind = kinds.first().map_or("other", |entry| entry.0).to_owned();
    if group.node_type == NodeType::Directory && TEST_DIRECTORY.is_match(name) {
        kind = "test".into();
    } else if group.node_type == NodeType::Directory && DOC_DIRECTORY.is_match(name) && kind != "code" {
        kind = "doc".into();
    }
    languages.retain(|entry| entry.1 > 0);
    languages.sort_by(|left, right| right.1.cmp(&left.1));
    let openable = group.node_type == NodeType::Directory && group.files.len() > 1;
    MapNode { id: group.id, node_type: group.node_type, outside, kind, files: group.files, lines, languages, openable }
}

fn files_under(map: &ProjectMap, directory: &str) -> Vec<usize> {
    map.files.iter().enumerate().filter(|(_, file)| in_scope(directory, &file.path)).map(|(index, _)| index).collect()
}

/// The boxes of one scope and the imports between them, with the directories outside it that its files import or that
/// import them. Directories holding most of the scope's code are split into their parts until about [`MAX_NODES`]
/// boxes remain; a small scope shows each file.
pub fn map_view(map: &ProjectMap, scope: &str) -> MapView {
    let files = files_under(map, scope);
    let mut groups: Vec<Group>;
    if files.len() <= FILE_LEVEL_MAX {
        groups = files.iter().map(|&index| Group { id: map.files[index].path.clone(), node_type: NodeType::File, files: vec![index] }).collect();
    } else {
        groups = split(map, scope, &files, 6);
        let total: u64 = groups.iter().map(|group| weight_of(map, group)).sum();
        let mut settled: HashSet<String> = HashSet::new();
        loop {
            let mut largest: Option<(&Group, u64)> = None;
            for group in &groups {
                let weight = weight_of(map, group);
                let candidate = group.node_type == NodeType::Directory && !settled.contains(&group.id) && group.files.len() > 1 && weight as f64 > total as f64 * SPLIT_SHARE;
                if candidate && largest.is_none_or(|(_, best)| weight > best) {
                    largest = Some((group, weight));
                }
            }
            let Some(largest) = largest.map(|(group, _)| group.clone()) else { break };
            let parts = split(map, &largest.id, &largest.files, 3);
            // Splitting must show more structure: a lone box of loose files does not, and too many boxes crowd the view.
            // A directory with a single subdirectory is replaced by it, whatever the box count.
            let useful = parts.len() > 1 || parts.first().is_some_and(|part| part.node_type == NodeType::Directory);
            if !useful || (parts.len() > 1 && groups.len() - 1 + parts.len() > MAX_NODES + 2) {
                settled.insert(largest.id);
                continue;
            }
            groups = groups.into_iter().flat_map(|group| if group.id == largest.id { parts.clone() } else { vec![group] }).collect();
        }
    }
    groups.sort_by(|left, right| left.node_type.cmp(&right.node_type).then_with(|| left.id.cmp(&right.id)));
    let mut node_of: HashMap<usize, String> = HashMap::new();
    for group in &groups {
        for &index in &group.files {
            node_of.insert(index, group.id.clone());
        }
    }
    let mut weights: Vec<((String, String), u64)> = Vec::new();
    let mut positions: HashMap<(String, String), usize> = HashMap::new();
    let mut outside_weights: HashMap<String, u64> = HashMap::new();
    let mut add = |from: String, to: String| {
        let key = (from, to);
        match positions.get(&key) {
            Some(&position) => weights[position].1 += 1,
            None => {
                positions.insert(key.clone(), weights.len());
                weights.push((key, 1));
            }
        }
    };
    for pair in map.imports.chunks_exact(2) {
        let (from, to) = (pair[0], pair[1]);
        match (node_of.get(&from), node_of.get(&to)) {
            (Some(source), Some(target)) => {
                if source != target {
                    add(source.clone(), target.clone());
                }
            }
            (Some(source), None) => {
                let ghost = outside_id(scope, &map.files[to].path);
                *outside_weights.entry(ghost.clone()).or_default() += 1;
                add(source.clone(), ghost);
            }
            (None, Some(target)) => {
                let ghost = outside_id(scope, &map.files[from].path);
                *outside_weights.entry(ghost.clone()).or_default() += 1;
                add(ghost, target.clone());
            }
            (None, None) => {}
        }
    }
    let mut outside: Vec<(String, u64)> = outside_weights.into_iter().collect();
    outside.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
    let mut kept: Vec<String> = outside.into_iter().take(MAX_OUTSIDE).map(|entry| entry.0).collect();
    kept.sort();
    let mut nodes: Vec<MapNode> = groups.into_iter().map(|group| describe(map, group, false)).collect();
    for id in kept {
        let group = match map.files.iter().position(|file| file.path == id) {
            Some(single) => Group { id, node_type: NodeType::File, files: vec![single] },
            None => Group { files: files_under(map, &id), id, node_type: NodeType::Directory },
        };
        nodes.push(describe(map, group, true));
    }
    let present: HashSet<&str> = nodes.iter().map(|node| node.id.as_str()).collect();
    let mut edges: Vec<MapEdge> = weights.into_iter().filter(|((from, to), _)| present.contains(from.as_str()) && present.contains(to.as_str())).map(|((from, to), weight)| MapEdge { from, to, weight }).collect();
    edges.sort_by(|left, right| left.from.cmp(&right.from).then_with(|| left.to.cmp(&right.to)));
    MapView { nodes, edges }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_outside_boxes() {
        assert_eq!(outside_id("src/app", "src/lib/x.ts"), "src/lib");
        assert_eq!(outside_id("src/app", "package.json"), "package.json");
        assert!(in_scope("src", "src/a.ts"));
        assert!(!in_scope("src", "srcx/a.ts"));
    }
}
