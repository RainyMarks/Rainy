//! Explorer's "Open with Rainy" entries and the file types Rainy offers to open, kept in the current user's registry
//! (`shell-integration.ts` in 3.x). Nothing here needs administrator rights, and Windows never lets an application make
//! itself the default app: the user picks Rainy in "Open with" or in the Default apps settings. Both parts stay off, and
//! nothing is written, until the user turns them on.

use std::collections::HashSet;
use std::path::Path;

use anyhow::Result;
use serde_json::{Map, Value};

use crate::targets;

/// What the user turned on in Settings.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ShellIntegration {
    /// "Open with Rainy" on files, folders and the background of a folder in Explorer.
    pub context_menu: bool,
    /// Rainy listed in "Open with" and in the Default apps settings for the file types below.
    pub file_types: bool,
}

impl ShellIntegration {
    fn any(self) -> bool {
        self.context_menu || self.file_types
    }
}

/// The language of the menu label and the file type names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    Chinese,
    English,
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
/// to the applications made for them.
pub const FILE_TYPES: [FileType; 48] = [
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
    FileType::new(&["sh", "bash", "zsh"], "Shell 脚本", "Shell Script"),
    FileType::new(&["ps1", "psm1"], "PowerShell 脚本", "PowerShell Script"),
    FileType::new(&["bat", "cmd"], "批处理文件", "Batch File"),
    FileType::new(&["cmake"], "CMake 文件", "CMake File"),
    FileType::new(&["tex"], "LaTeX 文档", "LaTeX Document"),
    FileType::new(&["bib"], "BibTeX 文献库", "BibTeX Library"),
    FileType::new(&["rst"], "reStructuredText 文档", "reStructuredText Document"),
    FileType::new(&["adoc"], "AsciiDoc 文档", "AsciiDoc Document"),
    FileType::new(&["diff", "patch"], "补丁文件", "Patch File"),
];

/// Every registered extension, without its dot.
pub fn extensions() -> impl Iterator<Item = &'static str> {
    FILE_TYPES.iter().flat_map(|entry| entry.extensions.iter().copied())
}

const CLASSES: &str = r"Software\Classes";
/// The verb's key under each shell key, with the keys above it that writing it may have created, children first; each
/// goes only once nothing else uses it.
const CONTEXT_KEYS: [(&str, &[&str]); 3] = [
    (r"Software\Classes\*\shell\Rainy", &[r"Software\Classes\*\shell", r"Software\Classes\*"]),
    (r"Software\Classes\Directory\shell\Rainy", &[r"Software\Classes\Directory\shell", r"Software\Classes\Directory"]),
    (
        r"Software\Classes\Directory\Background\shell\Rainy",
        &[r"Software\Classes\Directory\Background\shell", r"Software\Classes\Directory\Background", r"Software\Classes\Directory"],
    ),
];
const APPLICATIONS: &str = r"Software\Classes\Applications";
/// Windows' "Open with" entry for the executable, under the name it is installed with.
const APPLICATION: &str = r"Software\Classes\Applications\RainyAgent.exe";
/// The value only Rainy writes on [`APPLICATION`]: Windows makes that key without it when the user picks RainyAgent.exe
/// under "Open with → Choose another app".
const FRIENDLY_APP_NAME: &str = "FriendlyAppName";
const VENDOR: &str = r"Software\RainyMarks";
const PRODUCT: &str = r"Software\RainyMarks\Rainy";
const CAPABILITIES: &str = r"Software\RainyMarks\Rainy\Capabilities";
const REGISTERED_APPLICATIONS: &str = r"Software\RegisteredApplications";
/// The name Rainy is listed under in Default apps; `ms-settings:defaultapps?registeredAppUser=` takes the same name.
pub const REGISTERED_APPLICATION: &str = "Rainy";
/// Windows' Default apps settings on Rainy's page, and the list for Windows builds without that page.
pub const DEFAULT_APPS: &str = "ms-settings:defaultapps?registeredAppUser=Rainy";
pub const DEFAULT_APPS_LIST: &str = "ms-settings:defaultapps";

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

/// One change under HKEY_CURRENT_USER; the name "" is the key's default value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Operation {
    /// Write a string value unless it already holds that text.
    Set { key: String, name: String, value: String },
    /// Delete the key when it exists, and with `marker` only when that value holds what Rainy writes there; then each
    /// parent, in order, while removing it left the parent empty.
    RemoveKey { key: String, parents: Vec<String>, marker: Option<(String, String)> },
    /// Delete Rainy's value when it is there; then the key and its parents, in order, while that left them empty. A key
    /// the user already had (`keep_key`), empty or not, is never deleted for lacking Rainy's value.
    RemoveValue { key: String, name: String, parents: Vec<String>, keep_key: bool },
}

/// What Rainy remembers about the registry it wrote to (`ShellMemory`, desktop.json `explorerIntegration.keep` and
/// `.recorded`): the keys the user had before a part was turned on ([`removable_keys`]), which stay even when left
/// empty, and the parts that looked them up. For a part without that record (written by 3.9.0, or a desktop.json that
/// cannot be read) only Rainy's own keys and values go, never the keys above them. The default remembers nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Memory {
    pub keep: Vec<String>,
    pub recorded: ShellIntegration,
}

/// The registry changes that bring the current user's registration to `wanted` (`integrationOperations`): parts turned
/// on are (re)written for this executable and language, and the parts in `present` that are off are removed. A part
/// that is not present is not removed at all, so keys Windows or the user made under the same names
/// (`Applications\RainyAgent.exe` from "Choose another app") stay. The values [`read_status`] compares are each part's
/// last writes (the `*` verb, then `Rainy.md` with its command last), so a write cut short never reads as current.
pub fn operations(executable: &str, language: Language, wanted: ShellIntegration, memory: &Memory, present: ShellIntegration) -> Vec<Operation> {
    let keep: HashSet<String> = memory.keep.iter().map(|path| path.to_lowercase()).collect();
    let kept = |path: &str| keep.contains(&path.to_lowercase());
    // The walk up stops below the first key the user had: nothing above it can be left empty by Rainy.
    let above = |recorded: bool, parents: &[&str]| {
        if !recorded {
            return Vec::new();
        }
        parents.iter().take_while(|parent| !kept(parent)).map(|parent| (*parent).to_owned()).collect::<Vec<_>>()
    };
    let remove_value = |recorded: bool, key: &str, name: &str, parents: &[&str]| {
        let keep_key = kept(key) || !recorded;
        Operation::RemoveValue {
            key: key.to_owned(),
            name: name.to_owned(),
            parents: if keep_key { Vec::new() } else { above(recorded, parents) },
            keep_key,
        }
    };
    let (menu_recorded, types_recorded) = (memory.recorded.context_menu, memory.recorded.file_types);
    let remove_key = |key: &str, parents: Vec<String>| Operation::RemoveKey { key: key.to_owned(), parents, marker: None };
    let set = |key: &str, name: &str, value: &str| Operation::Set { key: key.to_owned(), name: name.to_owned(), value: value.to_owned() };
    let mut operations = Vec::new();
    let icon = format!("\"{executable}\",0");
    if wanted.context_menu {
        let label = context_menu_label(language);
        for (index, (verb, _)) in CONTEXT_KEYS.iter().enumerate().rev() {
            let placeholder = if index == 0 { "%1" } else { "%V" };
            operations.extend([set(verb, "", label), set(verb, "Icon", &icon), set(&format!(r"{verb}\command"), "", &launch_command(executable, placeholder))]);
        }
    } else if present.context_menu {
        operations.extend(CONTEXT_KEYS.iter().map(|(verb, parents)| remove_key(verb, above(menu_recorded, parents))));
    }
    if wanted.file_types {
        let description = match language {
            Language::Chinese => "AI 编程、Markdown 写作与科研办公工作台",
            Language::English => "Workbench for AI coding, Markdown writing and research",
        };
        let command = launch_command(executable, "%1");
        operations.extend([
            set(APPLICATION, FRIENDLY_APP_NAME, "Rainy"),
            set(&format!(r"{APPLICATION}\shell\open\command"), "", &command),
            set(CAPABILITIES, "ApplicationName", "Rainy"),
            set(CAPABILITIES, "ApplicationDescription", description),
            set(CAPABILITIES, "ApplicationIcon", &icon),
            set(REGISTERED_APPLICATIONS, REGISTERED_APPLICATION, CAPABILITIES),
        ]);
        for entry in FILE_TYPES.iter().rev() {
            for extension in entry.extensions.iter().rev() {
                let id = prog_id(extension);
                operations.extend([
                    set(&format!(r"{CLASSES}\{id}"), "", entry.name(language)),
                    set(&format!(r"{CLASSES}\{id}\DefaultIcon"), "", &icon),
                    set(&format!(r"{CLASSES}\.{extension}\OpenWithProgids"), &id, ""),
                    set(&format!(r"{APPLICATION}\SupportedTypes"), &format!(".{extension}"), ""),
                    set(&format!(r"{CAPABILITIES}\FileAssociations"), &format!(".{extension}"), &id),
                    set(&format!(r"{CLASSES}\{id}\shell\open\command"), "", &command),
                ]);
            }
        }
    } else if present.file_types {
        if kept(APPLICATION) {
            // Windows made the key for "Open with" before Rainy's file types were turned on: only Rainy's additions go.
            operations.extend([
                remove_value(types_recorded, APPLICATION, FRIENDLY_APP_NAME, &[]),
                remove_key(&format!(r"{APPLICATION}\SupportedTypes"), Vec::new()),
            ]);
        } else {
            // Only the key Rainy wrote, which carries its name, goes; one Windows made later stays.
            operations.push(Operation::RemoveKey {
                key: APPLICATION.to_owned(),
                parents: above(types_recorded, &[APPLICATIONS]),
                marker: Some((FRIENDLY_APP_NAME.to_owned(), "Rainy".to_owned())),
            });
        }
        operations.extend([
            remove_value(types_recorded, REGISTERED_APPLICATIONS, REGISTERED_APPLICATION, &[]),
            remove_key(PRODUCT, above(types_recorded, &[VENDOR])),
        ]);
        for extension in extensions() {
            let type_key = format!(r"{CLASSES}\.{extension}");
            operations.extend([
                remove_key(&format!(r"{CLASSES}\{}", prog_id(extension)), Vec::new()),
                remove_value(types_recorded, &format!(r"{type_key}\OpenWithProgids"), &prog_id(extension), &[&type_key]),
            ]);
        }
    }
    operations
}

/// Keys above Rainy's own that turning a part off may delete once they are empty: those writing it may have created, and
/// `Applications\RainyAgent.exe`, which Windows may have made first.
pub fn removable_keys(parts: ShellIntegration) -> Vec<String> {
    let mut keys: Vec<String> = Vec::new();
    if parts.context_menu {
        for parent in CONTEXT_KEYS.iter().flat_map(|(_, parents)| parents.iter()) {
            if !keys.iter().any(|key| key == parent) {
                keys.push((*parent).to_owned());
            }
        }
    }
    if parts.file_types {
        keys.extend([APPLICATIONS, APPLICATION, REGISTERED_APPLICATIONS, VENDOR].map(str::to_owned));
        for extension in extensions() {
            keys.extend([format!(r"{CLASSES}\.{extension}"), format!(r"{CLASSES}\.{extension}\OpenWithProgids")]);
        }
    }
    keys
}

/// The keys that exist now, lowercased, so they can be kept when Rainy's entries are removed later.
pub fn existing_keys(registry: &impl Registry, keys: &[String]) -> Result<Vec<String>> {
    let mut found = Vec::new();
    for key in keys {
        if registry.exists(key)? {
            found.push(key.to_lowercase());
        }
    }
    Ok(found)
}

/// What the registry holds now: the context menu's label and command, the command and name of Rainy's Markdown type,
/// and for each part whether anything of it is there, also without those values, as a write cut short leaves it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Status {
    pub context_menu_label: Option<String>,
    pub context_menu_command: Option<String>,
    pub file_type_command: Option<String>,
    pub file_type_name: Option<String>,
    /// One of the menu's verb keys.
    pub context_menu_traces: bool,
    /// Rainy's name on `Applications\RainyAgent.exe`, its capabilities or its Default apps entry: the first file-type
    /// values written.
    pub file_type_traces: bool,
}

pub fn read_status(registry: &impl Registry) -> Result<Status> {
    let (menu, _) = CONTEXT_KEYS[0];
    let markdown = format!(r"{CLASSES}\{}", prog_id("md"));
    let mut context_menu_traces = false;
    for (verb, _) in CONTEXT_KEYS {
        context_menu_traces = context_menu_traces || registry.exists(verb)?;
    }
    let file_type_traces = registry.read(APPLICATION, FRIENDLY_APP_NAME)?.as_deref() == Some("Rainy")
        || registry.exists(PRODUCT)?
        || registry.has_value(REGISTERED_APPLICATIONS, REGISTERED_APPLICATION)?;
    Ok(Status {
        context_menu_label: registry.read(menu, "")?,
        context_menu_command: registry.read(&format!(r"{menu}\command"), "")?,
        file_type_command: registry.read(&format!(r"{markdown}\shell\open\command"), "")?,
        file_type_name: registry.read(&markdown, "")?,
        context_menu_traces,
        file_type_traces,
    })
}

/// Whether the registry already matches, so nothing needs to be written (`shellIntegrationCurrent`). A moved
/// installation or another language needs a rewrite; unlike 3.x the type name is compared too, so the names follow the
/// language when only the file types are on, and a part that is off must have left no trace.
pub fn current(status: &Status, executable: &str, language: Language, wanted: ShellIntegration) -> bool {
    let command = launch_command(executable, "%1");
    let menu = if wanted.context_menu {
        status.context_menu_command.as_deref() == Some(command.as_str()) && status.context_menu_label.as_deref() == Some(context_menu_label(language))
    } else {
        status.context_menu_command.is_none() && !status.context_menu_traces
    };
    let types = if wanted.file_types {
        status.file_type_command.as_deref() == Some(command.as_str()) && status.file_type_name.as_deref() == Some(FILE_TYPES[0].name(language))
    } else {
        status.file_type_command.is_none() && !status.file_type_traces
    };
    menu && types
}

/// Whether an entry's command starts another Rainy installation that is still there. Its entries are left to it, so a
/// second installation (a QA build, a copy) never takes away the first one's; those of a Rainy that is gone are stale.
fn elsewhere(command: Option<&str>, executable: &str) -> bool {
    let other = command.and_then(|command| command.strip_prefix('"')).and_then(|rest| rest.split_once('"')).map(|(other, _)| other);
    other.is_some_and(|other| other.to_lowercase() != executable.to_lowercase() && Path::new(other).is_file())
}

/// Carry out `operations` (the 3.x registry script) and tell Explorer, which caches menus, icons and "Open with" lists
/// per type, when anything changed, also when a later operation failed.
pub fn apply(registry: &mut impl Registry, operations: &[Operation]) -> Result<bool> {
    let mut changed = false;
    let mut outcome = Ok(());
    for operation in operations {
        match carry_out(registry, operation) {
            Ok(done) => changed |= done,
            Err(error) => {
                outcome = Err(error);
                break;
            }
        }
    }
    if changed {
        registry.associations_changed();
    }
    outcome.map(|()| changed)
}

fn carry_out(registry: &mut impl Registry, operation: &Operation) -> Result<bool> {
    match operation {
        Operation::Set { key, name, value } => {
            if registry.read(key, name)?.as_deref() == Some(value.as_str()) {
                return Ok(false);
            }
            registry.set(key, name, value)?;
        }
        Operation::RemoveKey { key, parents, marker } => {
            if !registry.exists(key)? {
                return Ok(false);
            }
            if let Some((name, value)) = marker
                && registry.read(key, name)?.as_deref() != Some(value.as_str())
            {
                return Ok(false);
            }
            registry.delete_tree(key)?;
            remove_empty_parents(registry, parents)?;
        }
        Operation::RemoveValue { key, name, parents, keep_key } => {
            if !registry.has_value(key, name)? {
                return Ok(false);
            }
            registry.delete_value(key, name)?;
            if !keep_key && registry.empty(key)? {
                registry.delete_key(key)?;
                remove_empty_parents(registry, parents)?;
            }
        }
    }
    Ok(true)
}

/// Parents go one by one while each is left empty; the first one still in use stops the walk.
fn remove_empty_parents(registry: &mut impl Registry, parents: &[String]) -> Result<()> {
    for parent in parents {
        if !registry.empty(parent)? {
            break;
        }
        registry.delete_key(parent)?;
    }
    Ok(())
}

/// What a sync did, and the memory to keep from now on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Synced {
    pub changed: bool,
    pub memory: Memory,
}

/// Bring the registry to `wanted`, remembering the keys the user already had for parts being turned on now, so turning
/// them off later removes only what Rainy added (`syncShellIntegration`). `memory` is what was remembered when parts were
/// turned on earlier; `remember` saves the memory of a part being turned on before anything is written, so a write cut
/// short cannot lose it.
pub fn sync(
    registry: &mut impl Registry,
    executable: &str,
    language: Language,
    wanted: ShellIntegration,
    memory: &Memory,
    mut remember: impl FnMut(&Memory) -> Result<()>,
) -> Result<Synced> {
    let status = read_status(&*registry)?;
    if current(&status, executable, language, wanted) {
        return Ok(Synced { changed: false, memory: memory.clone() });
    }
    let registered = ShellIntegration { context_menu: status.context_menu_command.is_some(), file_types: status.file_type_command.is_some() };
    // A part whose write was cut short keeps the memory saved before that write: the keys it created since are Rainy's.
    let turning_on = ShellIntegration {
        context_menu: wanted.context_menu && !registered.context_menu && !(status.context_menu_traces && memory.recorded.context_menu),
        file_types: wanted.file_types && !registered.file_types && !(status.file_type_traces && memory.recorded.file_types),
    };
    let mut before = memory.clone();
    if turning_on.any() {
        let mut keep: Vec<String> = Vec::new();
        for path in memory.keep.iter().map(|path| path.to_lowercase()).chain(existing_keys(&*registry, &removable_keys(turning_on))?) {
            if !keep.contains(&path) {
                keep.push(path);
            }
        }
        let recorded = ShellIntegration {
            context_menu: memory.recorded.context_menu || turning_on.context_menu,
            file_types: memory.recorded.file_types || turning_on.file_types,
        };
        before = Memory { keep, recorded };
        remember(&before)?;
    }
    // A part that is off is removed only while Rainy's registration for it, whole or cut short, is there and is this
    // installation's, or that of a Rainy that is gone.
    let present = ShellIntegration {
        context_menu: (registered.context_menu || status.context_menu_traces) && !elsewhere(status.context_menu_command.as_deref(), executable),
        file_types: (registered.file_types || status.file_type_traces) && !elsewhere(status.file_type_command.as_deref(), executable),
    };
    let changed = apply(registry, &operations(executable, language, wanted, &before, present))?;
    // A part that is off now has nothing of Rainy's left; the next time it is turned on, it looks again.
    let off: Vec<String> = removable_keys(ShellIntegration { context_menu: !wanted.context_menu, file_types: !wanted.file_types })
        .iter()
        .map(|path| path.to_lowercase())
        .collect();
    let keep = before.keep.into_iter().filter(|path| !off.contains(path)).collect();
    let recorded = ShellIntegration {
        context_menu: wanted.context_menu && before.recorded.context_menu,
        file_types: wanted.file_types && before.recorded.file_types,
    };
    Ok(Synced { changed, memory: Memory { keep, recorded } })
}

/// Remove both parts for the uninstaller (`--rainy-unregister-shell`), keeping what `memory` names as the user's. Unlike a
/// sync it also clears Rainy's file types when Rainy.md is gone, such as the ProgIDs and OpenWithProgids values 3.x's
/// uninstaller left behind; `Applications\RainyAgent.exe` goes only when Rainy wrote it. Whether anything changed.
pub fn unregister(registry: &mut impl Registry, executable: &str, memory: &Memory) -> Result<bool> {
    let status = read_status(&*registry)?;
    let present = ShellIntegration {
        context_menu: !elsewhere(status.context_menu_command.as_deref(), executable),
        file_types: !elsewhere(status.file_type_command.as_deref(), executable),
    };
    apply(registry, &operations(executable, Language::Chinese, ShellIntegration::default(), memory, present))
}

/// The user's choice in desktop.json (`shellIntegrationPreference`): each part is on only when it is `true` under
/// `explorerIntegration`. 3.9.0's `shellIntegration`, which turned both on without asking, is not read, so an upgrade
/// removes what 3.9.0 wrote until the user opts in again.
pub fn preference(preferences: &Map<String, Value>) -> ShellIntegration {
    let saved = preferences.get("explorerIntegration").and_then(Value::as_object);
    let flag = |name: &str| saved.and_then(|saved| saved.get(name)) == Some(&Value::Bool(true));
    ShellIntegration { context_menu: flag("contextMenu"), file_types: flag("fileTypes") }
}

/// What desktop.json remembers about the registry (`shellMemory`): the keys kept as the user's own when a part was
/// turned on, and which parts looked them up.
pub fn memory(preferences: &Map<String, Value>) -> Memory {
    let saved = preferences.get("explorerIntegration").and_then(Value::as_object);
    let keep = saved.and_then(|saved| saved.get("keep")).and_then(Value::as_array);
    Memory {
        keep: keep.map(|keep| keep.iter().filter_map(Value::as_str).map(str::to_owned).collect()).unwrap_or_default(),
        recorded: parts(saved.and_then(|saved| saved.get("recorded"))),
    }
}

/// Both parts of a saved `{contextMenu, fileTypes}`, each on only when it is `true`.
fn parts(saved: Option<&Value>) -> ShellIntegration {
    let saved = saved.and_then(Value::as_object);
    let flag = |name: &str| saved.and_then(|saved| saved.get(name)) == Some(&Value::Bool(true));
    ShellIntegration { context_menu: flag("contextMenu"), file_types: flag("fileTypes") }
}

/// A change of the saved choice and memory; `None` keeps what is saved.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Change {
    pub context_menu: Option<bool>,
    pub file_types: Option<bool>,
    pub keep: Option<Vec<String>>,
    pub recorded: Option<ShellIntegration>,
}

impl Change {
    /// Save the memory only, never the switches.
    pub fn memory(memory: &Memory) -> Self {
        Self { keep: Some(memory.keep.clone()), recorded: Some(memory.recorded), ..Self::default() }
    }
}

/// Save a change of the choice in desktop.json (`saveShellIntegration`), as `{contextMenu, fileTypes, keep?, recorded?}`:
/// `keep` is left out when empty and `recorded` when no part looked, and 3.9.0's `shellIntegration` is dropped: it held
/// parts that were on without the user choosing them.
pub fn save(settings: &Path, change: Change) -> Result<()> {
    targets::edit_preferences(settings, |preferences| {
        let saved = preferences.get("explorerIntegration").and_then(Value::as_object).cloned().unwrap_or_default();
        let flag = |name: &str, value: Option<bool>| Value::Bool(value.unwrap_or(saved.get(name) == Some(&Value::Bool(true))));
        let keep = match change.keep {
            Some(keep) => keep.into_iter().map(Value::String).collect(),
            None => saved.get("keep").and_then(Value::as_array).cloned().unwrap_or_default(),
        };
        let recorded = change.recorded.unwrap_or_else(|| parts(saved.get("recorded")));
        let mut value = Map::new();
        value.insert("contextMenu".to_owned(), flag("contextMenu", change.context_menu));
        value.insert("fileTypes".to_owned(), flag("fileTypes", change.file_types));
        if !keep.is_empty() {
            value.insert("keep".to_owned(), Value::Array(keep));
        }
        if recorded.any() {
            let mut looked = Map::new();
            looked.insert("contextMenu".to_owned(), Value::Bool(recorded.context_menu));
            looked.insert("fileTypes".to_owned(), Value::Bool(recorded.file_types));
            value.insert("recorded".to_owned(), Value::Object(looked));
        }
        preferences.insert("explorerIntegration".to_owned(), Value::Object(value));
        // The other fields keep their order, as 3.x writes them.
        preferences.retain(|name, _| name != "shellIntegration");
    })
}

/// Bring the registry to the choice saved in `settings` (3.x main.ts `syncShellIntegration`): the choice when the
/// registry changed. Only the memory is saved, never the switches, so a switch the user flipped meanwhile stays as they
/// set it. It is saved before the registry is written and again after, when parts that ended off forget theirs.
pub fn sync_saved(registry: &mut impl Registry, settings: &Path, executable: &str, language: Language) -> Result<Option<ShellIntegration>> {
    let preferences = targets::read_preferences(settings)?;
    let (wanted, memory) = (preference(&preferences), memory(&preferences));
    let remember = |memory: &Memory| save(settings, Change::memory(memory));
    let synced = sync(registry, executable, language, wanted, &memory, remember)?;
    if synced.changed || synced.memory != memory {
        remember(&synced.memory)?;
    }
    Ok(synced.changed.then_some(wanted))
}

/// Save a switch and bring the registry to it with `sync` (`rainy:shell-integration-set`). When that fails, the switches
/// the change set go back to what they were: the settings page keeps showing them, and the next start would otherwise
/// apply the choice unasked. A second `sync` then takes back what the failed one wrote, rather than leaving a part
/// half written until the next start.
pub fn change_saved(settings: &Path, change: Change, mut sync: impl FnMut() -> Result<()>) -> Result<()> {
    let previous = preference(&targets::read_preferences(settings)?);
    let (context_menu, file_types) = (change.context_menu, change.file_types);
    save(settings, change)?;
    sync().inspect_err(|_| {
        let back = Change {
            context_menu: context_menu.map(|_| previous.context_menu),
            file_types: file_types.map(|_| previous.file_types),
            ..Change::default()
        };
        match save(settings, back) {
            Ok(()) => {
                if let Err(error) = sync() {
                    log::warn!("Explorer integration was not put back: {error:#}");
                }
            }
            Err(error) => log::warn!("The Explorer integration switch was not put back: {error:#}"),
        }
    })
}

/// `--rainy-unregister-shell`: remove both parts, keeping what desktop.json remembers; an unreadable desktop.json
/// remembers nothing, so only Rainy's own keys and values go. Whether anything changed.
pub fn unregister_saved(registry: &mut impl Registry, settings: &Path, executable: &str) -> Result<bool> {
    let preferences = targets::read_preferences(settings).unwrap_or_default();
    unregister(registry, executable, &memory(&preferences))
}

/// The current user's registry as the integration uses it. Keys are paths below HKEY_CURRENT_USER, and as in Windows,
/// paths and value names are compared without regard to case.
pub trait Registry {
    /// The string value `name` of `key` ("" is the default value); `None` when the key or the value is missing or holds
    /// something other than text.
    fn read(&self, key: &str, name: &str) -> Result<Option<String>>;
    fn exists(&self, key: &str) -> Result<bool>;
    /// Whether `key` holds the value `name`, of any type.
    fn has_value(&self, key: &str, name: &str) -> Result<bool>;
    /// Whether the key exists and holds neither values nor keys.
    fn empty(&self, key: &str) -> Result<bool>;
    /// Write a text value (REG_SZ), creating the key and those above it.
    fn set(&mut self, key: &str, name: &str, value: &str) -> Result<()>;
    /// Delete the key with everything below it; a missing key is not an error.
    fn delete_tree(&mut self, key: &str) -> Result<()>;
    /// Delete an empty key; a missing key is not an error.
    fn delete_key(&mut self, key: &str) -> Result<()>;
    /// Delete one value; a missing value is not an error.
    fn delete_value(&mut self, key: &str, name: &str) -> Result<()>;
    /// Tell Explorer the file associations changed (`SHChangeNotify(SHCNE_ASSOCCHANGED)`).
    fn associations_changed(&mut self);
}

#[cfg(windows)]
pub use win32::{CurrentUser, open_default_apps};

#[cfg(windows)]
mod win32 {
    use anyhow::{Result, bail};
    use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_MORE_DATA, ERROR_PATH_NOT_FOUND, ERROR_SUCCESS, ERROR_UNSUPPORTED_TYPE, WIN32_ERROR};
    use windows_sys::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_READ, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SAM_FLAGS, REG_SZ, RRF_RT_REG_SZ,
        RegCloseKey, RegCreateKeyExW, RegDeleteKeyW, RegDeleteTreeW, RegDeleteValueW, RegGetValueW, RegOpenKeyExW, RegQueryInfoKeyW,
        RegQueryValueExW, RegSetValueExW,
    };
    use windows_sys::Win32::UI::Shell::{SHCNE_ASSOCCHANGED, SHCNF_IDLIST, SHChangeNotify, ShellExecuteW};
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    use super::{DEFAULT_APPS, DEFAULT_APPS_LIST, Registry};
    use crate::locale::localized;

    /// HKEY_CURRENT_USER through the Win32 registry API.
    pub struct CurrentUser;

    use crate::text::wide;

    fn missing(status: WIN32_ERROR) -> bool {
        matches!(status, ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND)
    }

    fn failed(status: WIN32_ERROR, doing: &str, key: &str) -> anyhow::Error {
        let chinese = match doing {
            "Opening" => format!(r"无法打开 HKCU\{key}"),
            "Reading" => format!(r"无法读取 HKCU\{key}"),
            "Creating" => format!(r"无法创建 HKCU\{key}"),
            "Writing" => format!(r"无法写入 HKCU\{key}"),
            "Deleting" => format!(r"无法删除 HKCU\{key}"),
            _ => format!(r"无法删除 HKCU\{key} 中的值"),
        };
        anyhow::Error::new(std::io::Error::from_raw_os_error(status as i32)).context(localized(&chinese, &format!(r"{doing} HKCU\{key}")))
    }

    /// An open key, closed when dropped.
    struct Key(HKEY);

    impl Key {
        /// `HKCU\<path>` opened with `access`, or `None` when it does not exist.
        fn open(path: &str, access: REG_SAM_FLAGS) -> Result<Option<Self>> {
            let name = wide(path);
            let mut handle = std::ptr::null_mut();
            // SAFETY: `name` ends with NUL; the handle is closed by `Key`.
            match unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, name.as_ptr(), 0, access, &mut handle) } {
                ERROR_SUCCESS => Ok(Some(Self(handle))),
                status if missing(status) => Ok(None),
                status => Err(failed(status, "Opening", path)),
            }
        }
    }

    impl Drop for Key {
        fn drop(&mut self) {
            // SAFETY: the handle was opened for this `Key` and is closed once.
            unsafe { RegCloseKey(self.0) };
        }
    }

    impl Registry for CurrentUser {
        fn read(&self, key: &str, name: &str) -> Result<Option<String>> {
            let (path, value) = (wide(key), wide(name));
            let mut buffer = vec![0u16; 256];
            loop {
                let mut size = u32::try_from(buffer.len() * 2)?;
                // SAFETY: both names end with NUL, and `size` is the buffer's length in bytes.
                let status = unsafe {
                    RegGetValueW(HKEY_CURRENT_USER, path.as_ptr(), value.as_ptr(), RRF_RT_REG_SZ, std::ptr::null_mut(), buffer.as_mut_ptr().cast(), &mut size)
                };
                match status {
                    ERROR_SUCCESS => {
                        let units = &buffer[..(size as usize / 2).min(buffer.len())];
                        let end = units.iter().position(|unit| *unit == 0).unwrap_or(units.len());
                        return Ok(Some(String::from_utf16_lossy(&units[..end])));
                    }
                    ERROR_MORE_DATA => buffer.resize(size as usize / 2 + 1, 0),
                    ERROR_UNSUPPORTED_TYPE => return Ok(None),
                    status if missing(status) => return Ok(None),
                    status => return Err(failed(status, "Reading", key)),
                }
            }
        }

        fn exists(&self, key: &str) -> Result<bool> {
            Ok(Key::open(key, KEY_READ)?.is_some())
        }

        fn has_value(&self, key: &str, name: &str) -> Result<bool> {
            let Some(handle) = Key::open(key, KEY_QUERY_VALUE)? else { return Ok(false) };
            let value = wide(name);
            let null = std::ptr::null_mut();
            // SAFETY: asks only whether the value exists; nothing is read into memory.
            match unsafe { RegQueryValueExW(handle.0, value.as_ptr(), std::ptr::null(), null, null.cast(), null) } {
                ERROR_SUCCESS => Ok(true),
                status if missing(status) => Ok(false),
                status => Err(failed(status, "Reading", key)),
            }
        }

        fn empty(&self, key: &str) -> Result<bool> {
            let Some(handle) = Key::open(key, KEY_READ)? else { return Ok(false) };
            let (mut keys, mut values) = (0u32, 0u32);
            let null = std::ptr::null_mut::<u32>();
            // SAFETY: only the two counts are asked for.
            let status =
                unsafe { RegQueryInfoKeyW(handle.0, null.cast(), null, std::ptr::null(), &mut keys, null, null, &mut values, null, null, null, null.cast()) };
            if status != ERROR_SUCCESS {
                return Err(failed(status, "Reading", key));
            }
            Ok(keys == 0 && values == 0)
        }

        fn set(&mut self, key: &str, name: &str, value: &str) -> Result<()> {
            let (path, name, data) = (wide(key), wide(name), wide(value));
            let mut handle = std::ptr::null_mut();
            // SAFETY: `path` ends with NUL; the handle is closed by `Key`.
            let status = unsafe {
                RegCreateKeyExW(HKEY_CURRENT_USER, path.as_ptr(), 0, std::ptr::null(), REG_OPTION_NON_VOLATILE, KEY_SET_VALUE, std::ptr::null(), &mut handle, std::ptr::null_mut())
            };
            if status != ERROR_SUCCESS {
                return Err(failed(status, "Creating", key));
            }
            let handle = Key(handle);
            // SAFETY: `data` is the text with its NUL, passed with its length in bytes.
            let status = unsafe { RegSetValueExW(handle.0, name.as_ptr(), 0, REG_SZ, data.as_ptr().cast(), u32::try_from(data.len() * 2)?) };
            if status != ERROR_SUCCESS {
                return Err(failed(status, "Writing", key));
            }
            Ok(())
        }

        fn delete_tree(&mut self, key: &str) -> Result<()> {
            let path = wide(key);
            // SAFETY: `path` ends with NUL.
            match unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, path.as_ptr()) } {
                ERROR_SUCCESS => Ok(()),
                status if missing(status) => Ok(()),
                status => Err(failed(status, "Deleting", key)),
            }
        }

        fn delete_key(&mut self, key: &str) -> Result<()> {
            let path = wide(key);
            // SAFETY: `path` ends with NUL.
            match unsafe { RegDeleteKeyW(HKEY_CURRENT_USER, path.as_ptr()) } {
                ERROR_SUCCESS => Ok(()),
                status if missing(status) => Ok(()),
                status => Err(failed(status, "Deleting", key)),
            }
        }

        fn delete_value(&mut self, key: &str, name: &str) -> Result<()> {
            let Some(handle) = Key::open(key, KEY_SET_VALUE)? else { return Ok(()) };
            let value = wide(name);
            // SAFETY: `value` ends with NUL.
            match unsafe { RegDeleteValueW(handle.0, value.as_ptr()) } {
                ERROR_SUCCESS => Ok(()),
                status if missing(status) => Ok(()),
                status => Err(failed(status, "Deleting a value of", key)),
            }
        }

        fn associations_changed(&mut self) {
            // SAFETY: SHCNE_ASSOCCHANGED takes no items.
            unsafe { SHChangeNotify(SHCNE_ASSOCCHANGED as i32, SHCNF_IDLIST, std::ptr::null(), std::ptr::null()) };
        }
    }

    /// Open Windows' Default apps settings on Rainy's page (`rainy:default-apps`); Windows 10 and older builds ignore the
    /// parameter and show the list, and the list is opened when the page cannot be.
    pub fn open_default_apps() -> Result<()> {
        let verb = wide("open");
        for target in [DEFAULT_APPS, DEFAULT_APPS_LIST] {
            let file = wide(target);
            // SAFETY: both strings end with NUL; there is no owner window, parameter or folder.
            let instance = unsafe { ShellExecuteW(std::ptr::null_mut(), verb.as_ptr(), file.as_ptr(), std::ptr::null(), std::ptr::null(), SW_SHOWNORMAL) };
            if instance as isize > 32 {
                return Ok(());
            }
        }
        let error = std::io::Error::last_os_error();
        bail!(localized(&format!("Windows 无法打开“默认应用”设置：{error}"), &format!("Windows could not open the Default apps settings: {error}")))
    }
}

#[cfg(test)]
mod tests {
    use ::core::prelude::v1::test;
    use std::collections::BTreeMap;

    use anyhow::bail;
    use serde_json::json;

    use super::*;

    const EXE: &str = r"C:\Program Files\Rainy\RainyAgent.exe";
    const ALL: ShellIntegration = ShellIntegration { context_menu: true, file_types: true };
    const OFF: ShellIntegration = ShellIntegration { context_menu: false, file_types: false };
    const APP: &str = r"Software\Classes\Applications\RainyAgent.exe";

    fn on(context_menu: bool, file_types: bool) -> ShellIntegration {
        ShellIntegration { context_menu, file_types }
    }

    fn set(key: &str, name: &str, value: &str) -> Operation {
        Operation::Set { key: key.into(), name: name.into(), value: value.into() }
    }

    fn key(operation: &Operation) -> &str {
        match operation {
            Operation::Set { key, .. } | Operation::RemoveKey { key, .. } | Operation::RemoveValue { key, .. } => key,
        }
    }

    fn map(value: Value) -> Map<String, Value> {
        value.as_object().cloned().unwrap()
    }

    /// Both parts recorded as turned on, with these keys kept.
    fn recorded(keep: &[&str]) -> Memory {
        Memory { keep: keep.iter().map(|key| (*key).to_owned()).collect(), recorded: ALL }
    }

    /// A sync whose memory is not saved.
    fn quietly(registry: &mut Hive, executable: &str, language: Language, wanted: ShellIntegration, memory: &Memory) -> Synced {
        sync(registry, executable, language, wanted, memory, |_| Ok(())).unwrap()
    }

    /// The registry in memory; key paths and value names are kept lowercased, as Windows compares them.
    #[derive(Default)]
    struct Hive {
        keys: BTreeMap<String, BTreeMap<String, String>>,
        /// Values written.
        writes: usize,
        notified: usize,
        /// A key whose values cannot be written.
        refused: Option<String>,
    }

    impl Hive {
        fn create(&mut self, key: &str) {
            let mut path = String::new();
            for part in key.split('\\') {
                if !path.is_empty() {
                    path.push('\\');
                }
                path.push_str(&part.to_lowercase());
                self.keys.entry(path.clone()).or_default();
            }
        }

        fn has_subkeys(&self, key: &str) -> bool {
            let below = format!("{}\\", key.to_lowercase());
            self.keys.keys().any(|path| path.starts_with(&below))
        }

        fn paths(&self) -> Vec<&str> {
            self.keys.keys().map(String::as_str).collect()
        }

        fn text(&self, key: &str, name: &str) -> Option<String> {
            self.read(key, name).unwrap()
        }

        fn has(&self, key: &str) -> bool {
            self.exists(key).unwrap()
        }
    }

    impl Registry for Hive {
        fn read(&self, key: &str, name: &str) -> Result<Option<String>> {
            Ok(self.keys.get(&key.to_lowercase()).and_then(|values| values.get(&name.to_lowercase())).cloned())
        }

        fn exists(&self, key: &str) -> Result<bool> {
            Ok(self.keys.contains_key(&key.to_lowercase()))
        }

        fn has_value(&self, key: &str, name: &str) -> Result<bool> {
            Ok(self.read(key, name)?.is_some())
        }

        fn empty(&self, key: &str) -> Result<bool> {
            Ok(self.keys.get(&key.to_lowercase()).is_some_and(BTreeMap::is_empty) && !self.has_subkeys(key))
        }

        fn set(&mut self, key: &str, name: &str, value: &str) -> Result<()> {
            if self.refused.as_deref().is_some_and(|refused| refused.eq_ignore_ascii_case(key)) {
                bail!("Access is denied.");
            }
            self.create(key);
            self.keys.get_mut(&key.to_lowercase()).unwrap().insert(name.to_lowercase(), value.to_owned());
            self.writes += 1;
            Ok(())
        }

        fn delete_tree(&mut self, key: &str) -> Result<()> {
            let (key, below) = (key.to_lowercase(), format!("{}\\", key.to_lowercase()));
            self.keys.retain(|path, _| *path != key && !path.starts_with(&below));
            Ok(())
        }

        fn delete_key(&mut self, key: &str) -> Result<()> {
            if self.has_subkeys(key) {
                bail!("{key} has subkeys");
            }
            self.keys.remove(&key.to_lowercase());
            Ok(())
        }

        fn delete_value(&mut self, key: &str, name: &str) -> Result<()> {
            if let Some(values) = self.keys.get_mut(&key.to_lowercase()) {
                values.remove(&name.to_lowercase());
            }
            Ok(())
        }

        fn associations_changed(&mut self) {
            self.notified += 1;
        }
    }

    #[test]
    fn registers_the_context_menu_on_files_folders_and_backgrounds() {
        let planned = operations(EXE, Language::Chinese, on(true, false), &recorded(&[]), ALL);
        let commands: Vec<&Operation> = planned.iter().filter(|operation| matches!(operation, Operation::Set { key, .. } if key.ends_with(r"\command"))).collect();
        // The command of files, which tells a registration that is there, is written last.
        assert_eq!(commands, [
            &set(r"Software\Classes\Directory\Background\shell\Rainy\command", "", &format!("\"{EXE}\" \"%V\"")),
            &set(r"Software\Classes\Directory\shell\Rainy\command", "", &format!("\"{EXE}\" \"%V\"")),
            &set(r"Software\Classes\*\shell\Rainy\command", "", &format!("\"{EXE}\" \"%1\"")),
        ]);
        assert_eq!(menu_last(&planned), Some(&set(r"Software\Classes\*\shell\Rainy\command", "", &format!("\"{EXE}\" \"%1\""))));
        assert!(planned.contains(&set(r"Software\Classes\*\shell\Rainy", "", "通过 Rainy 打开")));
        assert!(planned.contains(&set(r"Software\Classes\Directory\shell\Rainy", "Icon", &format!("\"{EXE}\",0"))));
        // File types that are turned off lose only Rainy's value; the keys above it go only when that left them empty.
        assert!(planned.contains(&Operation::RemoveValue {
            key: r"Software\Classes\.md\OpenWithProgids".into(),
            name: "Rainy.md".into(),
            parents: vec![r"Software\Classes\.md".into()],
            keep_key: false,
        }));
        assert!(!planned.iter().any(|operation| matches!(operation, Operation::RemoveKey { key, .. } if key == r"Software\Classes\.md")));
        // File types Rainy has not registered are not removed at all.
        let menu_only = operations(EXE, Language::Chinese, on(true, false), &recorded(&[]), on(true, false));
        assert_eq!(menu_only.len(), 9);
        assert!(menu_only.iter().all(|operation| matches!(operation, Operation::Set { key, .. } if key.contains(r"\shell\Rainy"))));
    }

    #[test]
    fn offers_rainy_for_every_text_type() {
        let planned = operations(EXE, Language::English, on(false, true), &recorded(&[]), ALL);
        assert!(planned.contains(&Operation::RemoveKey {
            key: r"Software\Classes\*\shell\Rainy".into(),
            parents: vec![r"Software\Classes\*\shell".into(), r"Software\Classes\*".into()],
            marker: None,
        }));
        assert!(planned.contains(&Operation::RemoveKey {
            key: r"Software\Classes\Directory\Background\shell\Rainy".into(),
            parents: vec![
                r"Software\Classes\Directory\Background\shell".into(),
                r"Software\Classes\Directory\Background".into(),
                r"Software\Classes\Directory".into(),
            ],
            marker: None,
        }));
        for extension in ["md", "txt", "py", "tsx"] {
            assert!(planned.contains(&set(&format!(r"Software\Classes\.{extension}\OpenWithProgids"), &format!("Rainy.{extension}"), "")));
            assert!(planned.contains(&set(r"Software\RainyMarks\Rainy\Capabilities\FileAssociations", &format!(".{extension}"), &format!("Rainy.{extension}"))));
            assert!(planned.contains(&set(r"Software\Classes\Applications\RainyAgent.exe\SupportedTypes", &format!(".{extension}"), "")));
        }
        assert!(planned.contains(&set(r"Software\Classes\Rainy.md", "", "Markdown Document")));
        assert!(planned.contains(&set(r"Software\Classes\Rainy.md\shell\open\command", "", &format!("\"{EXE}\" \"%1\""))));
        assert!(planned.contains(&set(r"Software\RegisteredApplications", "Rainy", r"Software\RainyMarks\Rainy\Capabilities")));
        assert!(planned.contains(&set(APP, "FriendlyAppName", "Rainy")));
        assert!(planned.contains(&set(r"Software\RainyMarks\Rainy\Capabilities", "ApplicationDescription", "Workbench for AI coding, Markdown writing and research")));
        assert_eq!(planned.iter().filter(|operation| matches!(operation, Operation::Set { .. })).count(), 6 + 6 * 80);
        // Never the extension's own default: Windows keeps the user's choice.
        assert!(!planned.iter().any(|operation| matches!(operation, Operation::Set { key, name, .. }
            if name.is_empty() && key.rsplit('\\').next().is_some_and(|last| last.starts_with('.')))));
    }

    #[test]
    fn lists_text_formats_only_each_extension_once() {
        let all: Vec<&str> = extensions().collect();
        assert_eq!(all.len(), 80);
        assert_eq!(all.iter().collect::<HashSet<_>>().len(), 80);
        for extension in ["md", "markdown", "txt", "json", "py", "c", "cpp"] {
            assert!(all.contains(&extension));
        }
        for extension in ["docx", "pptx", "pdf", "exe", "lnk", "png"] {
            assert!(!all.contains(&extension));
        }
        assert!(FILE_TYPES.iter().all(|entry| !entry.zh.is_empty() && !entry.en.is_empty() && !entry.extensions.is_empty()));
    }

    #[test]
    fn keeps_both_parts_off_until_turned_on_and_compares_with_this_installation() {
        assert_eq!(preference(&Map::new()), OFF);
        // 3.9.0 turned both on without asking; its key is ignored, so an upgrade removes what it wrote.
        assert_eq!(preference(&map(json!({"shellIntegration": {"contextMenu": true, "fileTypes": true}}))), OFF);
        assert_eq!(preference(&map(json!({"explorerIntegration": {"contextMenu": true}}))), on(true, false));
        assert_eq!(preference(&map(json!({"explorerIntegration": {"fileTypes": "yes", "contextMenu": null}}))), OFF);
        let command = launch_command(EXE, "%1");
        let registered = Status {
            context_menu_label: Some("通过 Rainy 打开".into()),
            context_menu_command: Some(command.clone()),
            file_type_command: Some(command),
            file_type_name: Some("Markdown 文档".into()),
            context_menu_traces: true,
            file_type_traces: true,
        };
        assert!(current(&registered, EXE, Language::Chinese, ALL));
        // Another language, a moved installation or a part turned off all need a rewrite.
        assert!(!current(&registered, EXE, Language::English, ALL));
        assert!(!current(&registered, r"D:\Rainy\RainyAgent.exe", Language::Chinese, ALL));
        assert!(!current(&registered, EXE, Language::Chinese, on(false, true)));
        assert!(current(&Status::default(), EXE, Language::Chinese, OFF));
        // The type names follow the language also when only the file types are on.
        let types_only = Status { context_menu_label: None, context_menu_command: None, context_menu_traces: false, ..registered };
        assert!(current(&types_only, EXE, Language::Chinese, on(false, true)));
        assert!(!current(&types_only, EXE, Language::English, on(false, true)));
        // A part that is off but left something behind, as a write cut short does, is not current either.
        assert!(!current(&Status { context_menu_traces: true, ..Status::default() }, EXE, Language::Chinese, OFF));
        assert!(!current(&Status { file_type_traces: true, ..Status::default() }, EXE, Language::Chinese, OFF));
    }

    #[test]
    fn writes_reads_back_and_removes_what_it_made_with_unicode_intact() {
        let unicode = r"C:\Users\雨天\Rainy\RainyAgent.exe";
        let mut registry = Hive::default();
        registry.create(r"Software\Classes\.unrelated");
        assert!(apply(&mut registry, &operations(unicode, Language::Chinese, ALL, &Memory::default(), ALL)).unwrap());
        assert_eq!(registry.notified, 1);
        let status = read_status(&registry).unwrap();
        assert_eq!(status, Status {
            context_menu_label: Some("通过 Rainy 打开".into()),
            context_menu_command: Some(launch_command(unicode, "%1")),
            file_type_command: Some(launch_command(unicode, "%1")),
            file_type_name: Some("Markdown 文档".into()),
            context_menu_traces: true,
            file_type_traces: true,
        });
        assert!(current(&status, unicode, Language::Chinese, ALL));
        // Both parts were recorded as turned on into an empty registry: everything Rainy created goes.
        assert!(apply(&mut registry, &operations(unicode, Language::Chinese, OFF, &recorded(&[]), ALL)).unwrap());
        assert_eq!(read_status(&registry).unwrap(), Status::default());
        // Nothing the registration created is left, not even the empty keys above its values.
        assert_eq!(registry.paths(), ["software", r"software\classes", r"software\classes\.unrelated"]);
        assert_eq!(registry.notified, 2);
    }

    #[test]
    fn leaves_what_other_applications_and_the_user_had() {
        let mut registry = Hive::default();
        registry.set(r"Software\Classes\.md\OpenWithProgids", "Other.md", "").unwrap();
        registry.set(r"Software\Classes\*\shell\Other", "", "Other").unwrap();
        // Removing an integration that was never written touches nothing, not even an empty key of the user.
        registry.create(r"Software\Classes\.text\OpenWithProgids");
        assert!(!apply(&mut registry, &operations(EXE, Language::Chinese, OFF, &recorded(&[]), ALL)).unwrap());
        assert!(registry.has(r"Software\Classes\.text\OpenWithProgids"));
        assert_eq!(registry.notified, 0);
        apply(&mut registry, &operations(EXE, Language::Chinese, ALL, &Memory::default(), ALL)).unwrap();
        assert_eq!(registry.text(r"Software\Classes\.md\OpenWithProgids", "Rainy.md").as_deref(), Some(""));
        apply(&mut registry, &operations(EXE, Language::Chinese, OFF, &recorded(&[]), ALL)).unwrap();
        assert_eq!(registry.text(r"Software\Classes\.md\OpenWithProgids", "Other.md").as_deref(), Some(""));
        assert_eq!(registry.text(r"Software\Classes\.md\OpenWithProgids", "Rainy.md"), None);
        assert_eq!(registry.text(r"Software\Classes\*\shell\Other", "").as_deref(), Some("Other"));
        assert!(!registry.has(r"Software\Classes\*\shell\Rainy"));
    }

    #[test]
    fn remembers_the_keys_the_user_had_when_a_part_is_turned_on() {
        let mut registry = Hive::default();
        registry.create(r"Software\Classes\.text\OpenWithProgids");
        registry.create(r"Software\Classes\Directory\shell");
        // The memory is saved before anything is written, so a write cut short cannot lose it.
        let mut saved = Vec::new();
        let synced = sync(&mut registry, EXE, Language::Chinese, ALL, &Memory::default(), |memory| {
            saved.push(memory.clone());
            Ok(())
        })
        .unwrap();
        assert!(synced.changed);
        assert_eq!(saved, [synced.memory.clone()]);
        assert_eq!(synced.memory.recorded, ALL);
        assert!(synced.memory.keep.contains(&r"software\classes\.text\openwithprogids".to_owned()));
        assert!(synced.memory.keep.contains(&r"software\classes\directory\shell".to_owned()));
        assert!(!synced.memory.keep.contains(&r"software\classes\.md".to_owned()));
        // Already current: nothing is written or looked up again, Explorer is not told, and the memory stays.
        let (writes, notified) = (registry.writes, registry.notified);
        assert_eq!(quietly(&mut registry, EXE, Language::Chinese, ALL, &synced.memory), Synced { changed: false, memory: synced.memory.clone() });
        assert_eq!((registry.writes, registry.notified), (writes, notified));
        // Turning the menu off forgets only the menu's keys.
        let menu_off = quietly(&mut registry, EXE, Language::Chinese, on(false, true), &synced.memory);
        assert_eq!(menu_off.memory.recorded, on(false, true));
        assert!(menu_off.memory.keep.contains(&r"software\classes\.text\openwithprogids".to_owned()));
        assert!(!menu_off.memory.keep.contains(&r"software\classes\directory\shell".to_owned()));
        assert_eq!(quietly(&mut registry, EXE, Language::Chinese, OFF, &menu_off.memory), Synced { changed: true, memory: Memory::default() });
        assert!(registry.has(r"Software\Classes\.text\OpenWithProgids"));
        assert!(registry.has(r"Software\Classes\Directory\shell"));
        assert!(!registry.has(r"Software\Classes\Directory\shell\Rainy"));
        // What Rainy created goes: the .md type key, the Directory\Background branch and the * branch.
        assert!(!registry.has(r"Software\Classes\.md"));
        assert!(!registry.has(r"Software\Classes\Directory\Background"));
        assert!(!registry.has(r"Software\Classes\*"));
    }

    #[test]
    fn saves_the_memory_before_a_write_that_fails() {
        let mut registry = Hive { refused: Some(r"Software\Classes\Directory\Background\shell\Rainy".into()), ..Hive::default() };
        registry.create(r"Software\Classes\Directory\shell");
        let mut saved = Vec::new();
        let failed = sync(&mut registry, EXE, Language::Chinese, on(true, false), &Memory::default(), |memory| {
            saved.push(memory.clone());
            Ok(())
        });
        assert!(failed.is_err());
        assert_eq!(registry.writes, 0);
        assert_eq!(saved, [Memory {
            keep: vec![r"software\classes\directory\shell".into(), r"software\classes\directory".into()],
            recorded: on(true, false),
        }]);
        // A memory that cannot be saved stops the sync before anything is written.
        let mut registry = Hive::default();
        assert!(sync(&mut registry, EXE, Language::Chinese, ALL, &Memory::default(), |_| Err(anyhow::anyhow!("The disk is full"))).is_err());
        assert!(registry.keys.is_empty());
    }

    #[test]
    fn leaves_the_application_key_windows_wrote_for_choose_another_app() {
        let command = format!(r"{APP}\shell\open\command");
        let windows = launch_command(EXE, "%1");
        let mut registry = Hive::default();
        // Picking RainyAgent.exe under "Open with → Choose another app" makes Windows write this key.
        registry.set(&command, "", &windows).unwrap();
        // Only the menu, a language change, both off, then the uninstaller: the file-type part is not Rainy's, so its keys
        // are not touched.
        let menu = quietly(&mut registry, EXE, Language::Chinese, on(true, false), &Memory::default());
        assert_eq!(registry.text(&command, ""), Some(windows.clone()));
        let menu = quietly(&mut registry, EXE, Language::English, on(true, false), &menu.memory);
        let off = quietly(&mut registry, EXE, Language::English, OFF, &menu.memory);
        assert_eq!(off.memory, Memory::default());
        unregister(&mut registry, EXE, &off.memory).unwrap();
        assert_eq!(registry.text(&command, ""), Some(windows.clone()));
        // The menu, then the file types: the key was there first, so only Rainy's additions go.
        let menu = quietly(&mut registry, EXE, Language::Chinese, on(true, false), &Memory::default());
        let types = quietly(&mut registry, EXE, Language::Chinese, ALL, &menu.memory);
        assert!(types.memory.keep.contains(&APP.to_lowercase()));
        assert!(registry.has(&format!(r"{APP}\SupportedTypes")));
        assert_eq!(registry.text(APP, "FriendlyAppName").as_deref(), Some("Rainy"));
        quietly(&mut registry, EXE, Language::Chinese, OFF, &types.memory);
        assert_eq!(registry.text(&command, ""), Some(windows));
        assert_eq!(registry.text(APP, "FriendlyAppName"), None);
        assert!(!registry.has(&format!(r"{APP}\SupportedTypes")));
        assert!(!registry.has(r"Software\Classes\Rainy.md"));
        // Made by Rainy: the whole key goes, and Applications with it when Rainy made that too.
        let mut registry = Hive::default();
        let synced = quietly(&mut registry, EXE, Language::Chinese, on(false, true), &Memory::default());
        quietly(&mut registry, EXE, Language::Chinese, OFF, &synced.memory);
        assert!(!registry.has(r"Software\Classes\Applications"));
        assert!(!registry.has(r"Software\RainyMarks"));
        assert!(!registry.has(r"Software\RegisteredApplications"));
    }

    #[test]
    fn removes_only_rainys_own_keys_and_values_for_a_part_without_a_record_as_3_9_0_left_them() {
        let planned = operations(EXE, Language::Chinese, OFF, &Memory::default(), ALL);
        assert!(planned.contains(&Operation::RemoveKey { key: r"Software\Classes\*\shell\Rainy".into(), parents: Vec::new(), marker: None }));
        assert!(planned.contains(&Operation::RemoveValue {
            key: r"Software\Classes\.md\OpenWithProgids".into(),
            name: "Rainy.md".into(),
            parents: Vec::new(),
            keep_key: true,
        }));
        assert!(planned.contains(&Operation::RemoveKey {
            key: APP.into(),
            parents: Vec::new(),
            marker: Some(("FriendlyAppName".into(), "Rainy".into())),
        }));
        // A part that is not present is not removed at all.
        let menu_only = operations(EXE, Language::Chinese, OFF, &recorded(&[]), on(true, false));
        assert!(!menu_only.iter().any(|operation| key(operation).contains("Applications") || key(operation).contains("OpenWithProgids")));
        assert!(menu_only.iter().any(|operation| key(operation).ends_with(r"\shell\Rainy")));
        // Both parts as 3.9.0 wrote them, with nothing remembered: Rainy's keys and values go, the keys above them stay.
        let mut registry = Hive::default();
        apply(&mut registry, &operations(EXE, Language::Chinese, ALL, &Memory::default(), ALL)).unwrap();
        assert!(quietly(&mut registry, EXE, Language::Chinese, OFF, &Memory::default()).changed);
        assert_eq!(read_status(&registry).unwrap(), Status::default());
        for gone in [r"Software\Classes\*\shell\Rainy", r"Software\Classes\Rainy.md", APP, r"Software\RainyMarks\Rainy"] {
            assert!(!registry.has(gone), "{gone}");
        }
        for left in [
            r"Software\Classes\*\shell",
            r"Software\Classes\Directory\Background\shell",
            r"Software\Classes\.md\OpenWithProgids",
            r"Software\Classes\Applications",
            r"Software\RainyMarks",
            r"Software\RegisteredApplications",
        ] {
            assert!(registry.has(left), "{left}");
        }
        assert_eq!(registry.text(r"Software\Classes\.md\OpenWithProgids", "Rainy.md"), None);
        assert_eq!(registry.text(r"Software\RegisteredApplications", "Rainy"), None);
    }

    #[test]
    fn stops_removing_parents_at_a_kept_key_and_keeps_a_kept_key_that_held_only_rainys_value() {
        let planned = operations(EXE, Language::Chinese, OFF, &recorded(&[r"software\classes\directory", r"Software\Classes\.txt\OpenWithProgids"]), ALL);
        assert!(planned.contains(&Operation::RemoveKey {
            key: r"Software\Classes\Directory\Background\shell\Rainy".into(),
            parents: vec![r"Software\Classes\Directory\Background\shell".into(), r"Software\Classes\Directory\Background".into()],
            marker: None,
        }));
        assert!(planned.contains(&Operation::RemoveValue {
            key: r"Software\Classes\.txt\OpenWithProgids".into(),
            name: "Rainy.txt".into(),
            parents: Vec::new(),
            keep_key: true,
        }));
        assert!(removable_keys(on(true, false)).contains(&r"Software\Classes\Directory".to_owned()));
        assert_eq!(removable_keys(on(true, false)).len(), 6);
        assert_eq!(removable_keys(on(false, true)).len(), 4 + 2 * 80);
        assert!(removable_keys(OFF).is_empty());
        assert_eq!(memory(&map(json!({"explorerIntegration": {"keep": ["a", 1, "b"], "recorded": {"fileTypes": true}}}))), Memory {
            keep: vec!["a".into(), "b".into()],
            recorded: on(false, true),
        });
        assert_eq!(memory(&map(json!({"explorerIntegration": true}))), Memory::default());
        assert_eq!(memory(&map(json!({"explorerIntegration": {"recorded": {"contextMenu": "yes"}, "keep": "a"}}))), Memory::default());
    }

    #[test]
    fn rewrites_only_what_differs_when_the_language_changes() {
        let mut registry = Hive::default();
        let none = Memory::default();
        quietly(&mut registry, EXE, Language::Chinese, ALL, &none);
        let writes = registry.writes;
        assert!(quietly(&mut registry, EXE, Language::English, ALL, &none).changed);
        // The three menu labels, the 80 type names and the description.
        assert_eq!(registry.writes - writes, 3 + 80 + 1);
        assert_eq!(registry.text(r"Software\Classes\Directory\shell\Rainy", "").as_deref(), Some("Open with Rainy"));
        assert_eq!(registry.text(r"Software\Classes\Rainy.rs", "").as_deref(), Some("Rust Source File"));
        // With only the file types on, the names follow the language too.
        quietly(&mut registry, EXE, Language::English, on(false, true), &none);
        assert!(!registry.has(r"Software\Classes\*\shell\Rainy"));
        assert!(quietly(&mut registry, EXE, Language::Chinese, on(false, true), &none).changed);
        assert_eq!(registry.text(r"Software\Classes\Rainy.md", "").as_deref(), Some("Markdown 文档"));
        assert_eq!(registry.text(r"Software\RainyMarks\Rainy\Capabilities", "ApplicationDescription").as_deref(), Some("AI 编程、Markdown 写作与科研办公工作台"));
        assert!(!quietly(&mut registry, EXE, Language::Chinese, on(false, true), &none).changed);
    }

    #[test]
    fn unregistering_removes_what_the_3x_uninstaller_left_behind() {
        let left_behind = || {
            let mut registry = Hive::default();
            apply(&mut registry, &operations(EXE, Language::Chinese, on(false, true), &Memory::default(), ALL)).unwrap();
            registry.set(r"Software\Classes\.txt\OpenWithProgids", "Other.txt", "").unwrap();
            // 3.x's uninstaller deleted the Applications key and the capabilities itself; say Rainy.md went too.
            for key in [r"Software\Classes\Rainy.md", APP, r"Software\RainyMarks"] {
                registry.delete_tree(key).unwrap();
            }
            registry.delete_value(r"Software\RegisteredApplications", "Rainy").unwrap();
            registry
        };
        let mut registry = left_behind();
        // A sync sees no Rainy.md and leaves the rest alone; the uninstaller clears it.
        assert!(!quietly(&mut registry, EXE, Language::Chinese, OFF, &Memory::default()).changed);
        assert!(registry.has(r"Software\Classes\Rainy.txt"));
        // 3.x remembered nothing: Rainy's ProgIDs and values go, the keys that held the values stay.
        assert!(unregister(&mut registry, EXE, &Memory::default()).unwrap());
        assert!(!registry.has(r"Software\Classes\Rainy.txt"));
        assert!(registry.has(r"Software\Classes\.md\OpenWithProgids"));
        assert_eq!(registry.text(r"Software\Classes\.md\OpenWithProgids", "Rainy.md"), None);
        assert_eq!(registry.text(r"Software\Classes\.txt\OpenWithProgids", "Other.txt").as_deref(), Some(""));
        assert_eq!(registry.text(r"Software\Classes\.txt\OpenWithProgids", "Rainy.txt"), None);
        assert!(!registry.keys.keys().any(|key| key.contains("rainy.")));
        assert_eq!(registry.notified, 2);
        // Recorded when turned on: the keys Rainy created go too.
        let mut registry = left_behind();
        assert!(unregister(&mut registry, EXE, &recorded(&[])).unwrap());
        assert!(!registry.has(r"Software\Classes\.md"));
        assert_eq!(registry.text(r"Software\Classes\.txt\OpenWithProgids", "Other.txt").as_deref(), Some(""));
    }

    #[test]
    fn leaves_the_entries_of_another_installation_that_is_still_there() {
        let folder = tempfile::tempdir().unwrap();
        let other = folder.path().join("RainyAgent.exe");
        std::fs::write(&other, b"").unwrap();
        let other = other.to_str().unwrap().to_owned();
        let mut registry = Hive::default();
        let theirs = quietly(&mut registry, &other, Language::Chinese, ALL, &Memory::default()).memory;
        let (writes, notified) = (registry.writes, registry.notified);
        // This installation has both parts off: the other one's entries stay, at a start and at this one's uninstall.
        assert!(!quietly(&mut registry, EXE, Language::Chinese, OFF, &theirs).changed);
        assert!(!unregister(&mut registry, EXE, &theirs).unwrap());
        assert_eq!((registry.writes, registry.notified), (writes, notified));
        assert_eq!(read_status(&registry).unwrap().file_type_command, Some(launch_command(&other, "%1")));
        // Turning a part on here takes it over.
        quietly(&mut registry, EXE, Language::Chinese, on(true, false), &theirs);
        assert_eq!(read_status(&registry).unwrap().context_menu_command, Some(launch_command(EXE, "%1")));
        // Once that Rainy is gone its entries are stale and go.
        drop(folder);
        assert!(unregister(&mut registry, EXE, &theirs).unwrap());
        assert_eq!(read_status(&registry).unwrap(), Status::default());
        assert!(!registry.keys.keys().any(|key| key.contains("rainy")));
    }

    #[test]
    fn tells_explorer_about_what_changed_before_a_failure() {
        let mut registry = Hive { refused: Some(r"Software\Classes\Directory\shell\Rainy".into()), ..Hive::default() };
        assert!(apply(&mut registry, &operations(EXE, Language::Chinese, on(true, false), &Memory::default(), ALL)).is_err());
        assert!(registry.has(r"Software\Classes\Directory\Background\shell\Rainy\command"));
        assert_eq!(registry.notified, 1);
        // The verb written last, which tells a registration that is there, was not reached.
        assert!(!registry.has(r"Software\Classes\*\shell\Rainy"));
        let status = read_status(&registry).unwrap();
        assert!(status.context_menu_command.is_none() && status.context_menu_traces);
    }

    /// The last `Set` of the menu part.
    fn menu_last(planned: &[Operation]) -> Option<&Operation> {
        planned.iter().filter(|operation| matches!(operation, Operation::Set { key, .. } if key.contains(r"\shell\Rainy"))).last()
    }

    #[test]
    fn writes_the_values_a_sync_compares_last() {
        let planned = operations(EXE, Language::Chinese, on(false, true), &recorded(&[]), OFF);
        let markdown = r"Software\Classes\Rainy.md";
        assert_eq!(planned.last(), Some(&set(&format!(r"{markdown}\shell\open\command"), "", &launch_command(EXE, "%1"))));
        // The type name a sync compares comes after every other type's.
        let names: Vec<usize> = planned
            .iter()
            .enumerate()
            .filter(|(_, operation)| matches!(operation, Operation::Set { key, name, .. } if name.is_empty() && key.rsplit('\\').next().is_some_and(|last| last.starts_with("Rainy."))))
            .map(|(index, _)| index)
            .collect();
        assert_eq!(names.len(), 80);
        assert_eq!(planned.iter().position(|operation| *operation == set(markdown, "", "Markdown 文档")), names.last().copied());
    }

    /// Every key and value of a registration written in one go.
    fn whole(wanted: ShellIntegration) -> BTreeMap<String, BTreeMap<String, String>> {
        let mut registry = Hive::default();
        quietly(&mut registry, EXE, Language::Chinese, wanted, &Memory::default());
        registry.keys
    }

    #[test]
    fn a_write_cut_short_is_finished_or_taken_back_by_the_next_sync() {
        for (refused, wanted) in [
            (r"Software\Classes\Directory\shell\Rainy", on(true, false)),
            (r"Software\Classes\Rainy.py", on(false, true)),
            (r"Software\Classes\Rainy.md", on(false, true)),
            (r"Software\RegisteredApplications", on(false, true)),
        ] {
            let mut registry = Hive { refused: Some(refused.into()), ..Hive::default() };
            let mut saved = Memory::default();
            let failed = sync(&mut registry, EXE, Language::Chinese, wanted, &Memory::default(), |memory| {
                saved = memory.clone();
                Ok(())
            });
            assert!(failed.is_err(), "{refused}");
            let status = read_status(&registry).unwrap();
            assert!(!current(&status, EXE, Language::Chinese, wanted), "{refused}");
            assert!(!current(&status, EXE, Language::Chinese, OFF), "{refused}");
            // Turned on still: the next sync finishes the registration, keeping the memory saved before the first.
            let mut finished = Hive { keys: registry.keys.clone(), ..Hive::default() };
            let synced = quietly(&mut finished, EXE, Language::Chinese, wanted, &saved);
            assert!(synced.changed, "{refused}");
            assert_eq!(synced.memory, saved, "{refused}");
            assert_eq!(finished.keys, whole(wanted), "{refused}");
            // Turned off meanwhile: everything the cut write made goes.
            registry.refused = None;
            assert!(quietly(&mut registry, EXE, Language::Chinese, OFF, &saved).changed, "{refused}");
            assert_eq!(registry.paths(), ["software", r"software\classes"], "{refused}");
        }
    }

    #[test]
    fn a_switch_that_fails_takes_back_what_it_wrote() {
        let folder = tempfile::tempdir().unwrap();
        let settings = folder.path().join("desktop.json");
        std::fs::write(&settings, r#"{"explorerIntegration": {"contextMenu": true}}"#).unwrap();
        let mut registry = Hive::default();
        registry.create(r"Software\Classes\.txt\OpenWithProgids");
        sync_saved(&mut registry, &settings, EXE, Language::Chinese).unwrap();
        let before = registry.keys.clone();
        // The file types break off half way, after Rainy's capabilities and some of the types are written.
        registry.refused = Some(r"Software\Classes\Rainy.py".into());
        let failed = change_saved(&settings, Change { file_types: Some(true), ..Change::default() }, || {
            sync_saved(&mut registry, &settings, EXE, Language::Chinese).map(drop)
        });
        assert!(failed.is_err());
        let saved = targets::read_preferences(&settings).unwrap();
        assert_eq!(preference(&saved), on(true, false));
        assert_eq!(registry.keys, before);
        assert_eq!(memory(&saved).recorded, on(true, false));
    }

    #[test]
    fn saves_the_choice_and_drops_the_key_3_9_0_used() {
        let folder = tempfile::tempdir().unwrap();
        let settings = folder.path().join("desktop.json");
        std::fs::write(&settings, r#"{"locale": "en", "shellIntegration": {"contextMenu": true, "fileTypes": true}, "explorerIntegration": {"fileTypes": true, "keep": ["a"]}, "sshHosts": []}"#).unwrap();
        save(&settings, Change { context_menu: Some(true), ..Change::default() }).unwrap();
        let saved = targets::read_preferences(&settings).unwrap();
        assert_eq!(saved["explorerIntegration"], json!({"contextMenu": true, "fileTypes": true, "keep": ["a"]}));
        assert!(!saved.contains_key("shellIntegration"));
        assert_eq!(saved["locale"], "en");
        // Where the map keeps the file's order (the application's build), the other fields stay where 3.x leaves them.
        let ordered = serde_json::from_str::<Map<String, Value>>(r#"{"b": 0, "a": 0}"#).unwrap().keys().next().is_some_and(|first| first == "b");
        if ordered {
            assert_eq!(saved.keys().map(String::as_str).collect::<Vec<_>>(), ["locale", "explorerIntegration", "sshHosts"]);
        }
        save(&settings, Change { file_types: Some(false), keep: Some(Vec::new()), ..Change::default() }).unwrap();
        let saved = targets::read_preferences(&settings).unwrap();
        assert_eq!(saved["explorerIntegration"], json!({"contextMenu": true, "fileTypes": false}));
        assert_eq!(preference(&saved), on(true, false));
        assert!(std::fs::read_to_string(&settings).unwrap().ends_with("}\n"));
    }

    /// `saveShellIntegration` in 3.9.2, byte for byte: each switch and the memory are saved apart.
    #[test]
    fn saves_each_switch_and_the_memory_apart_as_3_9_2_writes_them() {
        let folder = tempfile::tempdir().unwrap();
        let settings = folder.path().join("desktop.json");
        std::fs::write(&settings, r#"{"explorerIntegration": {"recorded": {"contextMenu": "yes"}}, "locale": "en"}"#).unwrap();
        save(&settings, Change { file_types: Some(true), ..Change::default() }).unwrap();
        save(&settings, Change { keep: Some(vec![r"software\classes\.txt".into()]), ..Change::default() }).unwrap();
        save(&settings, Change { context_menu: Some(false), ..Change::default() }).unwrap();
        let saved = targets::read_preferences(&settings).unwrap();
        assert_eq!(saved["explorerIntegration"], json!({"contextMenu": false, "fileTypes": true, "keep": [r"software\classes\.txt"]}));
        save(&settings, Change { recorded: Some(on(false, true)), ..Change::default() }).unwrap();
        let expected = r#"{
  "explorerIntegration": {
    "contextMenu": false,
    "fileTypes": true,
    "keep": [
      "software\\classes\\.txt"
    ],
    "recorded": {
      "contextMenu": false,
      "fileTypes": true
    }
  },
  "locale": "en"
}
"#;
        assert_eq!(std::fs::read_to_string(&settings).unwrap(), expected);
        assert_eq!(memory(&targets::read_preferences(&settings).unwrap()), Memory { keep: vec![r"software\classes\.txt".into()], recorded: on(false, true) });
        save(&settings, Change { file_types: Some(false), keep: Some(Vec::new()), recorded: Some(OFF), ..Change::default() }).unwrap();
        assert_eq!(targets::read_preferences(&settings).unwrap()["explorerIntegration"], json!({"contextMenu": false, "fileTypes": false}));
    }

    #[test]
    fn a_sync_saves_only_the_memory_and_the_uninstaller_reads_it() {
        let folder = tempfile::tempdir().unwrap();
        let settings = folder.path().join("desktop.json");
        std::fs::write(&settings, r#"{"explorerIntegration": {"contextMenu": true}}"#).unwrap();
        let mut registry = Hive::default();
        registry.create(r"Software\Classes\Directory\shell");
        assert_eq!(sync_saved(&mut registry, &settings, EXE, Language::Chinese).unwrap(), Some(on(true, false)));
        let saved = targets::read_preferences(&settings).unwrap();
        assert_eq!(saved["explorerIntegration"], json!({
            "contextMenu": true,
            "fileTypes": false,
            "keep": [r"software\classes\directory\shell", r"software\classes\directory"],
            "recorded": {"contextMenu": true, "fileTypes": false},
        }));
        assert_eq!(sync_saved(&mut registry, &settings, EXE, Language::Chinese).unwrap(), None);
        // A switch turned off forgets the memory of its part.
        save(&settings, Change { context_menu: Some(false), ..Change::default() }).unwrap();
        assert_eq!(sync_saved(&mut registry, &settings, EXE, Language::Chinese).unwrap(), Some(OFF));
        assert_eq!(targets::read_preferences(&settings).unwrap()["explorerIntegration"], json!({"contextMenu": false, "fileTypes": false}));
        assert!(registry.has(r"Software\Classes\Directory\shell"));
        assert!(!registry.has(r"Software\Classes\*"));
        // On again, then the uninstaller with what desktop.json remembers.
        save(&settings, Change { context_menu: Some(true), ..Change::default() }).unwrap();
        sync_saved(&mut registry, &settings, EXE, Language::Chinese).unwrap();
        assert!(unregister_saved(&mut registry, &settings, EXE).unwrap());
        assert!(registry.has(r"Software\Classes\Directory\shell"));
        assert!(!registry.has(r"Software\Classes\Directory\shell\Rainy"));
        assert!(!registry.has(r"Software\Classes\*"));
        // The uninstaller does not need desktop.json.
        assert!(!unregister_saved(&mut registry, &folder.path().join("missing.json"), EXE).unwrap());
    }

    #[test]
    fn a_failed_write_puts_the_switch_back() {
        let folder = tempfile::tempdir().unwrap();
        let settings = folder.path().join("desktop.json");
        std::fs::write(&settings, r#"{"explorerIntegration": {"contextMenu": true}}"#).unwrap();
        let mut registry = Hive::default();
        sync_saved(&mut registry, &settings, EXE, Language::Chinese).unwrap();
        let turn_on = || Change { file_types: Some(true), ..Change::default() };
        registry.refused = Some(APP.into());
        let failed = change_saved(&settings, turn_on(), || sync_saved(&mut registry, &settings, EXE, Language::Chinese).map(drop));
        assert!(failed.is_err());
        let saved = targets::read_preferences(&settings).unwrap();
        assert_eq!(preference(&saved), on(true, false));
        // The memory of the part being turned on was saved before the write and stays.
        assert_eq!(memory(&saved).recorded, ALL);
        assert_eq!(read_status(&registry).unwrap().file_type_command, None);
        // Once the registry takes it, the switch stays on.
        registry.refused = None;
        change_saved(&settings, turn_on(), || sync_saved(&mut registry, &settings, EXE, Language::Chinese).map(drop)).unwrap();
        assert_eq!(preference(&targets::read_preferences(&settings).unwrap()), ALL);
        assert_eq!(read_status(&registry).unwrap().file_type_command, Some(launch_command(EXE, "%1")));
        // A switch that cannot be saved is not written.
        let damaged = folder.path().join("damaged.json");
        std::fs::write(&damaged, "{").unwrap();
        let mut called = false;
        let refused = change_saved(&damaged, turn_on(), || {
            called = true;
            Ok(())
        });
        assert!(refused.is_err() && !called);
        assert_eq!(std::fs::read_to_string(&damaged).unwrap(), "{");
    }

    /// The Win32 registry under a scratch key of the current user, removed afterwards. Nothing under Software\Classes is
    /// touched and Explorer is not told anything.
    #[cfg(windows)]
    #[test]
    fn the_current_user_registry_reads_writes_and_deletes() {
        struct Scratch(String);
        impl Drop for Scratch {
            fn drop(&mut self) {
                let _ = CurrentUser.delete_tree(&self.0);
            }
        }
        let scratch = Scratch(format!(r"Software\RainyTest-shell-{}", std::process::id()));
        let key = |path: &str| format!(r"{}\{path}", scratch.0);
        let mut registry = CurrentUser;
        assert!(!registry.exists(&scratch.0).unwrap());
        let icon = r#""C:\Users\雨天\Rainy\RainyAgent.exe",0"#;
        registry.set(&key(r"a\b"), "", "通过 Rainy 打开").unwrap();
        registry.set(&key(r"a\b"), "Icon", icon).unwrap();
        assert_eq!(registry.read(&key(r"A\B"), "").unwrap().as_deref(), Some("通过 Rainy 打开"));
        assert_eq!(registry.read(&key(r"a\b"), "icon").unwrap().as_deref(), Some(icon));
        let long = "雨".repeat(1000);
        registry.set(&key(r"a\b"), "Long", &long).unwrap();
        assert_eq!(registry.read(&key(r"a\b"), "Long").unwrap(), Some(long));
        assert_eq!(registry.read(&key(r"a\b"), "missing").unwrap(), None);
        assert_eq!(registry.read(&key("a"), "").unwrap(), None);
        assert_eq!(registry.read(&key("none"), "").unwrap(), None);
        assert!(registry.has_value(&key(r"a\b"), "").unwrap());
        assert!(!registry.has_value(&key("a"), "").unwrap());
        assert!(!registry.has_value(&key("none"), "").unwrap());
        assert!(!registry.empty(&key("a")).unwrap());
        assert!(!registry.empty(&key("none")).unwrap());
        registry.set(&key("c"), "Rainy.md", "").unwrap();
        assert_eq!(registry.read(&key("c"), "Rainy.md").unwrap().as_deref(), Some(""));
        registry.delete_value(&key("c"), "Rainy.md").unwrap();
        assert!(registry.empty(&key("c")).unwrap());
        registry.delete_key(&key("c")).unwrap();
        assert!(!registry.exists(&key("c")).unwrap());
        // The tree goes with the key itself.
        registry.delete_tree(&key("a")).unwrap();
        assert!(!registry.exists(&key("a")).unwrap());
        assert!(registry.empty(&scratch.0).unwrap());
        // What is not there is not an error.
        registry.delete_tree(&key("a")).unwrap();
        registry.delete_key(&key("a")).unwrap();
        registry.delete_value(&key("a"), "x").unwrap();
        registry.delete_value(&scratch.0, "x").unwrap();
    }
}
