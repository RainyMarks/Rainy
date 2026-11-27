//! Static file-type catalog, retired script extensions, registry path constants, and naming helpers.

/// What the user turned on in Settings.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ShellIntegration {
    /// "Open with Rainy" on files, folders and the background of a folder in Explorer.
    pub context_menu: bool,
    /// Rainy listed in "Open with" and in the Default apps settings for the file types below.
    pub file_types: bool,
}

impl ShellIntegration {
    pub(super) fn any(self) -> bool {
        self.context_menu || self.file_types
    }
}

/// The language of the menu label and the file type names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    Chinese,
    English,
}

impl Language {
    pub(super) fn code(self) -> &'static str {
        match self {
            Self::Chinese => "zh",
            Self::English => "en",
        }
    }
}

/// A file type Rainy registers, with its name in both interface languages.
#[derive(Debug)]
pub struct FileType {
    pub extensions: &'static [&'static str],
    pub zh: &'static str,
    pub en: &'static str,
}

impl FileType {
    const fn new(extensions: &'static [&'static str], zh: &'static str, en: &'static str) -> Self {
        Self { extensions, zh, en }
    }

    pub fn name(&self, language: Language) -> &'static str {
        match language {
            Language::Chinese => self.zh,
            Language::English => self.en,
        }
    }
}

/// Text formats Rainy edits: documents, data and source code. Office files, PDFs and media are previews only and are left
/// to the applications made for them, and scripts Windows runs on a double-click ([`RETIRED_EXTENSIONS`]) to Windows.
pub const FILE_TYPES: [FileType; 46] = [
    FileType::new(&["md", "markdown", "mdown", "mkd"], "Markdown 文档", "Markdown Document"),
    FileType::new(&["mdx"], "MDX 文档", "MDX Document"),
    FileType::new(&["txt", "text"], "文本文档", "Text Document"),
    FileType::new(&["log"], "日志文件", "Log File"),
    FileType::new(&["json", "jsonc", "json5"], "JSON 文件", "JSON File"),
    FileType::new(&["yaml", "yml"], "YAML 文件", "YAML File"),
    FileType::new(&["toml"], "TOML 文件", "TOML File"),
    FileType::new(&["ini", "cfg", "conf", "properties"], "配置文件", "Configuration File"),
    FileType::new(&["env"], "环境变量文件", "Environment File"),
    FileType::new(&["editorconfig", "gitignore", "gitattributes"], "配置文件", "Configuration File"),
    FileType::new(&["csv"], "CSV 文件", "CSV File"),
    FileType::new(&["tsv"], "TSV 文件", "TSV File"),
    FileType::new(&["xml"], "XML 文件", "XML File"),
    FileType::new(&["svg"], "SVG 图像", "SVG Image"),
    FileType::new(&["html", "htm"], "HTML 文件", "HTML File"),
    FileType::new(&["css"], "CSS 文件", "CSS File"),
    FileType::new(&["scss", "sass", "less"], "样式表文件", "Stylesheet"),
    FileType::new(&["js", "mjs", "cjs", "jsx"], "JavaScript 文件", "JavaScript File"),
    FileType::new(&["ts", "mts", "cts", "tsx"], "TypeScript 文件", "TypeScript File"),
    FileType::new(&["vue"], "Vue 组件", "Vue Component"),
    FileType::new(&["svelte"], "Svelte 组件", "Svelte Component"),
    FileType::new(&["py", "pyi"], "Python 源文件", "Python Source File"),
    FileType::new(&["c"], "C 源文件", "C Source File"),
    FileType::new(&["h"], "C 头文件", "C Header File"),
    FileType::new(&["cpp", "cc", "cxx"], "C++ 源文件", "C++ Source File"),
    FileType::new(&["hpp", "hh", "hxx"], "C++ 头文件", "C++ Header File"),
    FileType::new(&["cs"], "C# 源文件", "C# Source File"),
    FileType::new(&["java"], "Java 源文件", "Java Source File"),
    FileType::new(&["kt", "kts"], "Kotlin 源文件", "Kotlin Source File"),
    FileType::new(&["go"], "Go 源文件", "Go Source File"),
    FileType::new(&["rs"], "Rust 源文件", "Rust Source File"),
    FileType::new(&["swift"], "Swift 源文件", "Swift Source File"),
    FileType::new(&["php"], "PHP 源文件", "PHP Source File"),
    FileType::new(&["rb"], "Ruby 源文件", "Ruby Source File"),
    FileType::new(&["lua"], "Lua 源文件", "Lua Source File"),
    FileType::new(&["r"], "R 源文件", "R Source File"),
    FileType::new(&["dart"], "Dart 源文件", "Dart Source File"),
    FileType::new(&["scala"], "Scala 源文件", "Scala Source File"),
    FileType::new(&["sql"], "SQL 文件", "SQL File"),
    FileType::new(&["ps1", "psm1"], "PowerShell 脚本", "PowerShell Script"),
    FileType::new(&["cmake"], "CMake 文件", "CMake File"),
    FileType::new(&["tex"], "LaTeX 文档", "LaTeX Document"),
    FileType::new(&["bib"], "BibTeX 文献库", "BibTeX Library"),
    FileType::new(&["rst"], "reStructuredText 文档", "reStructuredText Document"),
    FileType::new(&["adoc"], "AsciiDoc 文档", "AsciiDoc Document"),
    FileType::new(&["diff", "patch"], "补丁文件", "Patch File"),
];

/// Scripts a double-click runs (batch files, and shell scripts where Git for Windows is installed). Up to 4.8 Rainy was
/// offered for them too; Windows then asked which app should open them, and once Rainy was picked, or became the choice,
/// they opened as text in Rainy and stopped running. Every sync takes these entries back ([`super::retire_operations`]).
pub const RETIRED_EXTENSIONS: [&str; 5] = ["bat", "cmd", "sh", "bash", "zsh"];

/// Every registered extension, without its dot.
pub fn extensions() -> impl Iterator<Item = &'static str> {
    FILE_TYPES.iter().flat_map(|entry| entry.extensions.iter().copied())
}

pub(super) const CLASSES: &str = r"Software\Classes";
/// The verb's key under each shell key, with the keys above it that writing it may have created, children first; each
/// goes only once nothing else uses it.
pub(super) const CONTEXT_KEYS: [(&str, &[&str]); 3] = [
    (r"Software\Classes\*\shell\Rainy", &[r"Software\Classes\*\shell", r"Software\Classes\*"]),
    (r"Software\Classes\Directory\shell\Rainy", &[r"Software\Classes\Directory\shell", r"Software\Classes\Directory"]),
    (
        r"Software\Classes\Directory\Background\shell\Rainy",
        &[r"Software\Classes\Directory\Background\shell", r"Software\Classes\Directory\Background", r"Software\Classes\Directory"],
    ),
];
pub(super) const APPLICATIONS: &str = r"Software\Classes\Applications";
/// Windows' "Open with" entry for the executable, under the name it is installed with.
pub(super) const APPLICATION: &str = r"Software\Classes\Applications\RainyAgent.exe";
/// The value only Rainy writes on [`APPLICATION`]: Windows makes that key without it when the user picks RainyAgent.exe
/// under "Open with → Choose another app".
pub(super) const FRIENDLY_APP_NAME: &str = "FriendlyAppName";
pub(super) const VENDOR: &str = r"Software\RainyMarks";
pub(super) const PRODUCT: &str = r"Software\RainyMarks\Rainy";
pub(super) const CAPABILITIES: &str = r"Software\RainyMarks\Rainy\Capabilities";
pub(super) const REGISTERED_APPLICATIONS: &str = r"Software\RegisteredApplications";
/// Where Windows records the handlers it has seen appear for each file type (`<ProgID>_.<extension>`).
pub(super) const ASSOCIATION_TOASTS: &str = r"Software\Microsoft\Windows\CurrentVersion\ApplicationAssociationToasts";
/// The name Rainy is listed under in Default apps; `ms-settings:defaultapps?registeredAppUser=` takes the same name.
pub const REGISTERED_APPLICATION: &str = "Rainy";
/// Windows' Default apps settings on Rainy's page, and the list for Windows builds without that page.
pub const DEFAULT_APPS: &str = "ms-settings:defaultapps?registeredAppUser=Rainy";
pub const DEFAULT_APPS_LIST: &str = "ms-settings:defaultapps";

/// Explorer's own record of the user's choices for an extension.
pub(super) fn file_exts(extension: &str) -> String {
    format!(r"Software\Microsoft\Windows\CurrentVersion\Explorer\FileExts\.{extension}")
}

/// Rainy's ProgID for an extension without its dot.
pub fn prog_id(extension: &str) -> String {
    format!("Rainy.{extension}")
}

/// The command Explorer runs: `%1` is a file, `%V` a folder.
pub fn launch_command(executable: &str, placeholder: &str) -> String {
    format!("\"{executable}\" \"{placeholder}\"")
}

pub fn context_menu_label(language: Language) -> &'static str {
    match language {
        Language::Chinese => "通过 Rainy 打开",
        Language::English => "Open with Rainy",
    }
}

pub(super) fn file_type_description(language: Language) -> &'static str {
    match language {
        Language::Chinese => "AI 编程、Markdown 写作与科研办公工作台",
        Language::English => "Workbench for AI coding, Markdown writing and research",
    }
}

/// Fingerprint of the current `FILE_TYPES` and `RETIRED_EXTENSIONS` catalog so upgrades automatically invalidate
/// the startup fast-skip cache whenever extensions are added or retired.
pub(super) fn catalog_signature() -> String {
    let mut active: Vec<&str> = extensions().collect();
    active.sort_unstable();
    format!("v2:active={}:retired={}", active.join(","), RETIRED_EXTENSIONS.join(","))
}
