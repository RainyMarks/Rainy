//! Registry planning, synchronization, Win11 24H2 `UserChoiceLatest\ProgId` cleanup, preference storage, and fast-skip caching.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::catalog::{
    APPLICATION, APPLICATIONS, ASSOCIATION_TOASTS, CAPABILITIES, CLASSES, CONTEXT_KEYS, FILE_TYPES, FRIENDLY_APP_NAME,
    Language, PRODUCT, REGISTERED_APPLICATION, REGISTERED_APPLICATIONS, RETIRED_EXTENSIONS, ShellIntegration, VENDOR,
    catalog_signature, context_menu_label, extensions, file_exts, file_type_description, launch_command, prog_id,
};
use crate::fs_util;
use crate::paths;
use crate::targets;
use crate::text::quoted_executable;

const SYNC_CACHE_FILE: &str = ".explorer-sync-cache.json";

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

/// Take back what 4.8 and earlier registered for [`RETIRED_EXTENSIONS`]: Rainy's ProgID and its place in "Open with",
/// and a default-app choice of Rainy for them (including Win11 24H2's `UserChoiceLatest\ProgId`), so Windows runs them
/// again on a double-click. A choice of any other app is left alone, and so are keys the user had (`memory.keep`).
pub fn retire_operations(memory: &Memory) -> Vec<Operation> {
    let keep: HashSet<String> = memory.keep.iter().map(|path| path.to_lowercase()).collect();
    let mut operations = Vec::new();
    for extension in RETIRED_EXTENSIONS {
        let id = prog_id(extension);
        let type_key = format!(r"{CLASSES}\.{extension}");
        let list = format!(r"{type_key}\OpenWithProgids");
        let ours = memory.recorded.file_types && !keep.contains(&list.to_lowercase());
        let parents = if ours && !keep.contains(&type_key.to_lowercase()) { vec![type_key.clone()] } else { Vec::new() };
        operations.extend([
            Operation::RemoveKey { key: format!(r"{CLASSES}\{id}"), parents: Vec::new(), marker: None },
            Operation::RemoveValue { key: list, name: id.clone(), parents, keep_key: !ours },
            Operation::RemoveValue { key: format!(r"{APPLICATION}\SupportedTypes"), name: format!(".{extension}"), parents: Vec::new(), keep_key: true },
            Operation::RemoveValue { key: format!(r"{CAPABILITIES}\FileAssociations"), name: format!(".{extension}"), parents: Vec::new(), keep_key: true },
            Operation::RemoveValue { key: format!(r"{}\OpenWithProgids", file_exts(extension)), name: id.clone(), parents: Vec::new(), keep_key: true },
        ]);
        for choice in ["UserChoice", "UserChoiceLatest"] {
            for chosen in [id.as_str(), r"Applications\RainyAgent.exe"] {
                operations.push(Operation::RemoveKey {
                    key: format!(r"{}\{choice}", file_exts(extension)),
                    parents: Vec::new(),
                    marker: Some(("ProgId".to_owned(), chosen.to_owned())),
                });
            }
        }
        for seen in [format!("{id}_.{extension}"), format!(r"Applications\RainyAgent.exe_.{extension}")] {
            operations.push(Operation::RemoveValue { key: ASSOCIATION_TOASTS.to_owned(), name: seen, parents: Vec::new(), keep_key: true });
        }
    }
    operations
}

/// Carry out [`retire_operations`]; a failure is logged, never in the way of the rest of a sync.
fn retire(registry: &mut impl Registry, memory: &Memory) -> bool {
    let mut changed = false;
    for operation in retire_operations(memory) {
        match carry_out(registry, &operation) {
            Ok(done) => changed |= done,
            Err(error) => log::warn!("Explorer integration: {error:#}"),
        }
    }
    if changed {
        registry.associations_changed();
    }
    changed
}

/// The registry changes that bring the current user's registration to `wanted` (`integrationOperations`).
pub fn operations(executable: &str, language: Language, wanted: ShellIntegration, memory: &Memory, present: ShellIntegration) -> Vec<Operation> {
    let keep: HashSet<String> = memory.keep.iter().map(|path| path.to_lowercase()).collect();
    let kept = |path: &str| keep.contains(&path.to_lowercase());
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
        let description = file_type_description(language);
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
            operations.extend([
                remove_value(types_recorded, APPLICATION, FRIENDLY_APP_NAME, &[]),
                remove_key(&format!(r"{APPLICATION}\SupportedTypes"), Vec::new()),
            ]);
        } else {
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
            let id = prog_id(extension);
            let type_key = format!(r"{CLASSES}\.{extension}");
            operations.extend([
                remove_key(&format!(r"{CLASSES}\{id}"), Vec::new()),
                remove_value(types_recorded, &format!(r"{type_key}\OpenWithProgids"), &id, &[&type_key]),
                Operation::RemoveValue { key: format!(r"{}\OpenWithProgids", file_exts(extension)), name: id.clone(), parents: Vec::new(), keep_key: true },
                Operation::RemoveValue { key: ASSOCIATION_TOASTS.to_owned(), name: format!("{id}_.{extension}"), parents: Vec::new(), keep_key: true },
            ]);
        }
    }
    operations
}

/// Keys above Rainy's own that turning a part off may delete once they are empty.
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

/// What the registry holds now.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Status {
    pub context_menu_label: Option<String>,
    pub context_menu_command: Option<String>,
    pub file_type_command: Option<String>,
    pub file_type_name: Option<String>,
    pub context_menu_traces: bool,
    pub file_type_traces: bool,
    pub context_menu_complete: bool,
    pub file_types_complete: bool,
    pub missing_extensions: Vec<&'static str>,
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

    let context_menu_label = registry.read(menu, "")?;
    let context_menu_command = registry.read(&format!(r"{menu}\command"), "")?;
    let file_type_command = registry.read(&format!(r"{markdown}\shell\open\command"), "")?;
    let file_type_name = registry.read(&markdown, "")?;

    let context_menu_complete = if let (Some(label), Some(cmd)) = (context_menu_label.as_deref(), context_menu_command.as_deref())
        && let Some(exe) = quoted_executable(cmd)
    {
        let icon = format!("\"{exe}\",0");
        let mut all_ok = true;
        for (index, (verb, _)) in CONTEXT_KEYS.iter().enumerate() {
            let placeholder = if index == 0 { "%1" } else { "%V" };
            let expected_cmd = launch_command(exe, placeholder);
            let ok = registry.read(verb, "")?.as_deref() == Some(label)
                && registry.read(verb, "Icon")?.as_deref() == Some(icon.as_str())
                && registry.read(&format!(r"{verb}\command"), "")?.as_deref() == Some(expected_cmd.as_str());
            if !ok {
                all_ok = false;
                break;
            }
        }
        all_ok
    } else {
        false
    };

    let inferred_language = match file_type_name.as_deref() {
        Some(name) if name == FILE_TYPES[0].zh => Some(Language::Chinese),
        Some(name) if name == FILE_TYPES[0].en => Some(Language::English),
        _ => None,
    };

    let mut missing_extensions = Vec::new();
    let file_types_complete = if let (Some(cmd), Some(language)) = (file_type_command.as_deref(), inferred_language)
        && let Some(exe) = quoted_executable(cmd)
    {
        let icon = format!("\"{exe}\",0");
        let mut all_ok = registry.read(APPLICATION, FRIENDLY_APP_NAME)?.as_deref() == Some("Rainy")
            && registry.read(&format!(r"{APPLICATION}\shell\open\command"), "")?.as_deref() == Some(cmd)
            && registry.read(CAPABILITIES, "ApplicationName")?.as_deref() == Some("Rainy")
            && registry.read(CAPABILITIES, "ApplicationDescription")?.as_deref() == Some(file_type_description(language))
            && registry.read(CAPABILITIES, "ApplicationIcon")?.as_deref() == Some(icon.as_str())
            && registry.read(REGISTERED_APPLICATIONS, REGISTERED_APPLICATION)?.as_deref() == Some(CAPABILITIES);
        for entry in &FILE_TYPES {
            let expected_name = entry.name(language);
            for &extension in entry.extensions {
                let id = prog_id(extension);
                let has_open_with = registry.has_value(&format!(r"{CLASSES}\.{extension}\OpenWithProgids"), &id)?;
                if !has_open_with {
                    missing_extensions.push(extension);
                }
                if all_ok {
                    let ext_ok = has_open_with
                        && registry.read(&format!(r"{CLASSES}\{id}"), "")?.as_deref() == Some(expected_name)
                        && registry.read(&format!(r"{CLASSES}\{id}\DefaultIcon"), "")?.as_deref() == Some(icon.as_str())
                        && registry.read(&format!(r"{CLASSES}\{id}\shell\open\command"), "")?.as_deref() == Some(cmd)
                        && registry.has_value(&format!(r"{APPLICATION}\SupportedTypes"), &format!(".{extension}"))?
                        && registry.read(&format!(r"{CAPABILITIES}\FileAssociations"), &format!(".{extension}"))?.as_deref() == Some(id.as_str());
                    if !ext_ok {
                        all_ok = false;
                    }
                }
            }
        }
        all_ok
    } else {
        // Even if `Rainy.md` was damaged or missing, still identify missing extensions when file-type traces exist
        // so upgrades/repairs record any pre-existing user keys into `memory.keep` before backfilling.
        if file_type_traces {
            for extension in extensions() {
                let id = prog_id(extension);
                if !registry.has_value(&format!(r"{CLASSES}\.{extension}\OpenWithProgids"), &id)? {
                    missing_extensions.push(extension);
                }
            }
        }
        false
    };

    Ok(Status {
        context_menu_label,
        context_menu_command,
        file_type_command,
        file_type_name,
        context_menu_traces,
        file_type_traces,
        context_menu_complete,
        file_types_complete,
        missing_extensions,
    })
}

/// Whether the registry already matches, so nothing needs to be written (`shellIntegrationCurrent`).
pub fn current(status: &Status, executable: &str, language: Language, wanted: ShellIntegration) -> bool {
    let command = launch_command(executable, "%1");
    let menu = if wanted.context_menu {
        status.context_menu_complete
            && status.context_menu_command.as_deref() == Some(command.as_str())
            && status.context_menu_label.as_deref() == Some(context_menu_label(language))
    } else {
        status.context_menu_command.is_none() && !status.context_menu_traces
    };
    let types = if wanted.file_types {
        status.file_types_complete
            && status.file_type_command.as_deref() == Some(command.as_str())
            && status.file_type_name.as_deref() == Some(FILE_TYPES[0].name(language))
    } else {
        status.file_type_command.is_none() && !status.file_type_traces
    };
    menu && types
}

fn normalize_exe_path(path: &str) -> String {
    paths::without_verbatim(PathBuf::from(path.trim()))
        .to_string_lossy()
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_ascii_lowercase()
}

/// Whether an entry's command starts another Rainy installation that is still there.
fn elsewhere(command: Option<&str>, executable: &str) -> bool {
    let other = command.and_then(quoted_executable);
    other.is_some_and(|other| normalize_exe_path(other) != normalize_exe_path(executable) && Path::new(other).is_file())
}

/// Carry out `operations` (the 3.x registry script) and tell Explorer when anything changed.
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

pub(super) fn carry_out(registry: &mut impl Registry, operation: &Operation) -> Result<bool> {
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
            if let Some((name, value)) = marker {
                let direct = registry.read(key, name)?;
                // Windows 11 24H2 stores the ProgId under `UserChoiceLatest\ProgId` (value `ProgId`) instead of directly
                // under `UserChoiceLatest`.
                let matches_24h2 = direct.is_none()
                    && key.to_ascii_lowercase().ends_with(r"\userchoicelatest")
                    && name.eq_ignore_ascii_case("ProgId")
                    && registry.read(&format!(r"{key}\ProgId"), name)?.as_deref() == Some(value.as_str());
                if direct.as_deref() != Some(value.as_str()) && !matches_24h2 {
                    return Ok(false);
                }
                if matches_24h2 {
                    // Best-effort removal of protected 24H2 subkeys before removing the parent key.
                    for sub in ["ProgId", "Hash"] {
                        let subkey = format!(r"{key}\{sub}");
                        if registry.exists(&subkey)? && registry.delete_tree(&subkey).is_err() {
                            let _ = registry.delete_key(&subkey);
                        }
                    }
                }
            }
            // Windows denies setting values on a default-app choice (`UserChoice`), which deleting a tree needs; a key
            // without subkeys can still be deleted whole.
            if let Err(error) = registry.delete_tree(key) {
                registry.delete_key(key).map_err(|_| error)?;
            }
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

/// Bring the registry to `wanted`, remembering the keys the user already had for parts being turned on now.
pub fn sync(
    registry: &mut impl Registry,
    executable: &str,
    language: Language,
    wanted: ShellIntegration,
    memory: &Memory,
    mut remember: impl FnMut(&Memory) -> Result<()>,
) -> Result<Synced> {
    let retired = retire(registry, memory);
    let status = read_status(&*registry)?;
    if current(&status, executable, language, wanted) {
        return Ok(Synced { changed: retired, memory: memory.clone() });
    }
    let registered = ShellIntegration { context_menu: status.context_menu_command.is_some(), file_types: status.file_type_command.is_some() };
    let turning_on = ShellIntegration {
        context_menu: wanted.context_menu && !registered.context_menu && !(status.context_menu_traces && memory.recorded.context_menu),
        file_types: wanted.file_types && !registered.file_types && !(status.file_type_traces && memory.recorded.file_types),
    };
    let mut before = memory.clone();
    let mut backfill_keys: Vec<String> = Vec::new();
    if wanted.file_types && (registered.file_types || status.file_type_traces) && memory.recorded.file_types && !status.missing_extensions.is_empty() {
        for &extension in &status.missing_extensions {
            backfill_keys.push(format!(r"{CLASSES}\.{extension}"));
            backfill_keys.push(format!(r"{CLASSES}\.{extension}\OpenWithProgids"));
        }
    }
    if turning_on.any() || !backfill_keys.is_empty() {
        let mut keys_to_check = removable_keys(turning_on);
        keys_to_check.extend(backfill_keys);
        let mut keep: Vec<String> = Vec::new();
        for path in memory.keep.iter().map(|path| path.to_lowercase()).chain(existing_keys(&*registry, &keys_to_check)?) {
            if !keep.contains(&path) {
                keep.push(path);
            }
        }
        let recorded = ShellIntegration {
            context_menu: memory.recorded.context_menu || turning_on.context_menu,
            file_types: memory.recorded.file_types || turning_on.file_types,
        };
        let next = Memory { keep, recorded };
        if next != before || turning_on.any() {
            before = next;
            remember(&before)?;
        }
    }
    let present = ShellIntegration {
        context_menu: (registered.context_menu || status.context_menu_traces) && !elsewhere(status.context_menu_command.as_deref(), executable),
        file_types: (registered.file_types || status.file_type_traces) && !elsewhere(status.file_type_command.as_deref(), executable),
    };
    let changed = apply(registry, &operations(executable, language, wanted, &before, present))? || retired;
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

/// Remove both parts for the uninstaller (`--rainy-unregister-shell`), keeping what `memory` names as the user's.
pub fn unregister(registry: &mut impl Registry, executable: &str, memory: &Memory) -> Result<bool> {
    let status = read_status(&*registry)?;
    let present = ShellIntegration {
        context_menu: !elsewhere(status.context_menu_command.as_deref(), executable),
        file_types: !elsewhere(status.file_type_command.as_deref(), executable),
    };
    let retired = retire(registry, memory);
    Ok(apply(registry, &operations(executable, Language::Chinese, ShellIntegration::default(), memory, present))? || retired)
}

/// The user's choice in desktop.json (`shellIntegrationPreference`).
pub fn preference(preferences: &Map<String, Value>) -> ShellIntegration {
    let saved = preferences.get("explorerIntegration").and_then(Value::as_object);
    let flag = |name: &str| saved.and_then(|saved| saved.get(name)) == Some(&Value::Bool(true));
    ShellIntegration { context_menu: flag("contextMenu"), file_types: flag("fileTypes") }
}

/// What desktop.json remembers about the registry (`shellMemory`).
pub fn memory(preferences: &Map<String, Value>) -> Memory {
    let saved = preferences.get("explorerIntegration").and_then(Value::as_object);
    let keep = saved.and_then(|saved| saved.get("keep")).and_then(Value::as_array);
    Memory {
        keep: keep.map(|keep| keep.iter().filter_map(Value::as_str).map(str::to_owned).collect()).unwrap_or_default(),
        recorded: parts(saved.and_then(|saved| saved.get("recorded"))),
    }
}

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct SyncSignature {
    catalog: String,
    executable: String,
    language: String,
    context_menu: bool,
    file_types: bool,
}

fn sync_cache_path(settings: &Path) -> Option<PathBuf> {
    Some(settings.parent()?.join(SYNC_CACHE_FILE))
}

fn clear_sync_cache(settings: &Path) {
    if let Some(cache) = sync_cache_path(settings) {
        let _ = std::fs::remove_file(cache);
    }
}

fn read_sync_cache(settings: &Path) -> Option<SyncSignature> {
    let cache = sync_cache_path(settings)?;
    let text = std::fs::read_to_string(cache).ok()?;
    serde_json::from_str(&text).ok()
}

fn write_sync_cache(settings: &Path, sig: &SyncSignature) {
    if let Some(cache) = sync_cache_path(settings)
        && let Ok(text) = serde_json::to_string(sig)
    {
        let _ = fs_util::write_atomic(&cache, (text + "\n").as_bytes(), false);
    }
}

/// Cheap O(1) sentinel verification (<= 6 registry queries) used when the persisted sync signature matches
/// the current executable, language, switches and catalog, avoiding ~520 startup registry queries.
fn sentinels_match(registry: &impl Registry, executable: &str, language: Language, wanted: ShellIntegration) -> Result<bool> {
    let command = launch_command(executable, "%1");
    let (menu, _) = CONTEXT_KEYS[0];
    let menu_ok = if wanted.context_menu {
        registry.read(menu, "")?.as_deref() == Some(context_menu_label(language))
            && registry.read(&format!(r"{menu}\command"), "")?.as_deref() == Some(command.as_str())
    } else {
        !registry.exists(menu)?
    };
    if !menu_ok {
        return Ok(false);
    }
    let markdown = format!(r"{CLASSES}\{}", prog_id("md"));
    let types_ok = if wanted.file_types {
        registry.read(&markdown, "")?.as_deref() == Some(FILE_TYPES[0].name(language))
            && registry.read(&format!(r"{markdown}\shell\open\command"), "")?.as_deref() == Some(command.as_str())
            && registry.read(REGISTERED_APPLICATIONS, REGISTERED_APPLICATION)?.as_deref() == Some(CAPABILITIES)
    } else {
        registry.read(APPLICATION, FRIENDLY_APP_NAME)?.as_deref() != Some("Rainy")
            && !registry.exists(PRODUCT)?
            && !registry.has_value(REGISTERED_APPLICATIONS, REGISTERED_APPLICATION)?
    };
    Ok(types_ok)
}

/// Save a change of the choice in desktop.json (`saveShellIntegration`), keeping 3.9.2's exact JSON shape.
pub fn save(settings: &Path, change: Change) -> Result<()> {
    let switch_changed = change.context_menu.is_some() || change.file_types.is_some();
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
        preferences.retain(|name, _| name != "shellIntegration");
    })?;
    if switch_changed {
        clear_sync_cache(settings);
    }
    Ok(())
}

/// Bring the registry to the choice saved in `settings` (3.x main.ts `syncShellIntegration`): the choice when the
/// registry changed. Uses a persisted sync signature + sentinel probe to skip 500+ registry queries on steady startups.
pub fn sync_saved(registry: &mut impl Registry, settings: &Path, executable: &str, language: Language) -> Result<Option<ShellIntegration>> {
    let preferences = targets::read_preferences(settings)?;
    let (wanted, memory) = (preference(&preferences), memory(&preferences));
    let expected_sig = SyncSignature {
        catalog: catalog_signature(),
        executable: normalize_exe_path(executable),
        language: language.code().to_owned(),
        context_menu: wanted.context_menu,
        file_types: wanted.file_types,
    };
    if read_sync_cache(settings).as_ref() == Some(&expected_sig)
        && sentinels_match(&*registry, executable, language, wanted)?
    {
        return Ok(None);
    }
    let remember = |memory: &Memory| save(settings, Change::memory(memory));
    let synced = sync(registry, executable, language, wanted, &memory, remember)?;
    if synced.changed || synced.memory != memory {
        remember(&synced.memory)?;
    }
    write_sync_cache(settings, &expected_sig);
    Ok(synced.changed.then_some(wanted))
}

/// Save a switch and bring the registry to it with `sync` (`rainy:shell-integration-set`).
pub fn change_saved(settings: &Path, change: Change, mut sync: impl FnMut() -> Result<()>) -> Result<()> {
    let previous = preference(&targets::read_preferences(settings)?);
    let (context_menu, file_types) = (change.context_menu, change.file_types);
    save(settings, change)?;
    sync().inspect_err(|_| {
        clear_sync_cache(settings);
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

/// `--rainy-unregister-shell`: remove both parts, keeping what desktop.json remembers.
pub fn unregister_saved(registry: &mut impl Registry, settings: &Path, executable: &str) -> Result<bool> {
    clear_sync_cache(settings);
    let preferences = targets::read_preferences(settings).unwrap_or_default();
    unregister(registry, executable, &memory(&preferences))
}
