//! Project text search and replace (`src/shared/text-search.ts`) and Git status (`src/shared/git-protocol.ts`).

use serde::{Deserialize, Serialize};

/// One match; searches are line by line.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TextSearchMatch {
    /// One-based line.
    pub line: u32,
    /// Zero-based UTF-16 column in the line.
    pub column: u32,
    pub length: u32,
    /// The line, or the part of a long line around the match.
    pub preview: String,
    /// Where the match starts in `preview`, in UTF-16 units.
    pub preview_column: u32,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TextSearchFile {
    pub root_id: String,
    pub path: String,
    pub matches: Vec<TextSearchMatch>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct TextSearchResult {
    pub files: Vec<TextSearchFile>,
    pub matches: u32,
    pub truncated: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ReplacedFile {
    pub root_id: String,
    pub path: String,
    #[serde(default)]
    pub replaced: u32,
    #[serde(default)]
    pub message: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct TextReplaceResult {
    pub files: Vec<ReplacedFile>,
    pub failed: Vec<ReplacedFile>,
}

/// A file's change in one repository; `staged` and `unstaged` are a change kind such as `modified` or `untracked`.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct GitFileChange {
    pub path: String,
    #[serde(default)]
    pub from: Option<String>,
    #[serde(default)]
    pub staged: Option<String>,
    #[serde(default)]
    pub unstaged: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GitRepository {
    pub root_id: String,
    pub title: String,
    pub branch: Option<String>,
    pub head: Option<String>,
    #[serde(default)]
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    pub files: Vec<GitFileChange>,
    pub truncated: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PlainRoot {
    pub root_id: String,
    pub title: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct GitStatus {
    pub available: bool,
    pub repositories: Vec<GitRepository>,
    pub plain: Vec<PlainRoot>,
}

/// A file against its Git base (`files.diff`).
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct IdeFileDiff {
    pub path: String,
    pub base: Option<String>,
    pub current: Option<String>,
    pub version: Option<String>,
    /// `modified`, `added`, `deleted`, `unchanged` or `unavailable`.
    pub status: String,
    #[serde(default)]
    pub reason: Option<String>,
}
