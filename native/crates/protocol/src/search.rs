//! Project text search and replace (`search.text`, `search.replace`) and Git (`git.*`, `files.diff`).

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
    #[serde(default)]
    pub files: Vec<TextSearchFile>,
    #[serde(default)]
    pub matches: u32,
    #[serde(default)]
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
    #[serde(default)]
    pub files: Vec<ReplacedFile>,
    #[serde(default)]
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
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub head: Option<String>,
    #[serde(default)]
    pub upstream: Option<String>,
    #[serde(default)]
    pub ahead: u32,
    #[serde(default)]
    pub behind: u32,
    #[serde(default)]
    pub files: Vec<GitFileChange>,
    #[serde(default)]
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
    #[serde(default)]
    pub repositories: Vec<GitRepository>,
    #[serde(default)]
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

/// Short commit hash returned by `git.commit`.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct GitCommitResult {
    pub commit: String,
}
