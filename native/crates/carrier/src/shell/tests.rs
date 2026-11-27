use ::core::prelude::v1::test;
use std::cell::Cell;
use std::collections::{BTreeMap, HashSet};

use anyhow::{Result, bail};
use serde_json::{Map, Value, json};

use super::catalog::{APPLICATION, ASSOCIATION_TOASTS, CAPABILITIES, CLASSES, file_exts};
use super::plan::carry_out;
use super::*;
use crate::targets;

const EXE: &str = r"C:\Program Files\Rainy\RainyAgent.exe";
const ALL: ShellIntegration = ShellIntegration { context_menu: true, file_types: true };
const OFF: ShellIntegration = ShellIntegration { context_menu: false, file_types: false };
const APP: &str = APPLICATION;

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

fn recorded(keep: &[&str]) -> Memory {
    Memory { keep: keep.iter().map(|key| (*key).to_owned()).collect(), recorded: ALL }
}

fn quietly(registry: &mut Hive, executable: &str, language: Language, wanted: ShellIntegration, memory: &Memory) -> Synced {
    sync(registry, executable, language, wanted, memory, |_| Ok(())).unwrap()
}

#[derive(Default)]
struct Hive {
    keys: BTreeMap<String, BTreeMap<String, String>>,
    reads: Cell<usize>,
    writes: usize,
    notified: usize,
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
        self.reads.set(self.reads.get() + 1);
        Ok(self.keys.get(&key.to_lowercase()).and_then(|values| values.get(&name.to_lowercase())).cloned())
    }

    fn exists(&self, key: &str) -> Result<bool> {
        self.reads.set(self.reads.get() + 1);
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
fn scripts_that_run_on_a_double_click_are_never_offered() {
    for retired in RETIRED_EXTENSIONS {
        assert!(!extensions().any(|extension| extension == retired), "{retired}");
    }
    let planned = operations(EXE, Language::Chinese, on(false, true), &recorded(&[]), ShellIntegration::default());
    assert!(!planned.iter().any(|operation| {
        let key = key(operation).to_lowercase();
        key.contains(r"\.cmd") || key.contains("rainy.cmd") || key.contains(r"\.bat")
    }));
}

fn hit_by_4_8(hive: &mut Hive) {
    for extension in ["bat", "cmd"] {
        let id = prog_id(extension);
        hive.set(&format!(r"{CLASSES}\{id}\shell\open\command"), "", &launch_command(EXE, "%1")).unwrap();
        hive.set(&format!(r"{CLASSES}\.{extension}\OpenWithProgids"), &id, "").unwrap();
        hive.set(&format!(r"{APPLICATION}\SupportedTypes"), &format!(".{extension}"), "").unwrap();
        hive.set(&format!(r"{CAPABILITIES}\FileAssociations"), &format!(".{extension}"), &id).unwrap();
        hive.set(&format!(r"{}\OpenWithProgids", file_exts(extension)), &id, "").unwrap();
    }
    hive.set(&format!(r"{}\UserChoice", file_exts("cmd")), "ProgId", "Rainy.cmd").unwrap();
    hive.set(&format!(r"{}\UserChoice", file_exts("cmd")), "Hash", "x").unwrap();
    hive.set(&format!(r"{}\UserChoice", file_exts("bat")), "ProgId", r"Applications\Code.exe").unwrap();
    for seen in ["Rainy.bat_.bat", r"Applications\RainyAgent.exe_.bat", "Rainy.cmd_.cmd", "batfile_.bat"] {
        hive.set(ASSOCIATION_TOASTS, seen, "0").unwrap();
    }
}

#[test]
fn a_sync_gives_batch_files_back_to_windows() {
    let mut hive = Hive::default();
    hit_by_4_8(&mut hive);
    let synced = quietly(&mut hive, EXE, Language::Chinese, on(false, true), &recorded(&[]));
    assert!(synced.changed && hive.notified > 0);
    assert!(!hive.has(&format!(r"{}\UserChoice", file_exts("cmd"))));
    assert_eq!(hive.text(&format!(r"{}\UserChoice", file_exts("bat")), "ProgId").as_deref(), Some(r"Applications\Code.exe"));
    for seen in ["Rainy.bat_.bat", r"Applications\RainyAgent.exe_.bat", "Rainy.cmd_.cmd"] {
        assert_eq!(hive.text(ASSOCIATION_TOASTS, seen), None, "{seen}");
    }
    assert_eq!(hive.text(ASSOCIATION_TOASTS, "batfile_.bat").as_deref(), Some("0"));
    for extension in ["bat", "cmd"] {
        assert!(!hive.has(&format!(r"{CLASSES}\{}", prog_id(extension))));
        assert!(!hive.has(&format!(r"{CLASSES}\.{extension}")), "{extension}");
        assert_eq!(hive.text(&format!(r"{APPLICATION}\SupportedTypes"), &format!(".{extension}")), None);
        assert_eq!(hive.text(&format!(r"{CAPABILITIES}\FileAssociations"), &format!(".{extension}")), None);
        assert!(hive.read(&format!(r"{}\OpenWithProgids", file_exts(extension)), &prog_id(extension)).unwrap().is_none());
    }
    assert!(hive.text(&format!(r"{CLASSES}\Rainy.md\shell\open\command"), "").is_some());
    assert!(!quietly(&mut hive, EXE, Language::Chinese, on(false, true), &recorded(&[])).changed);
}

#[test]
fn retires_win11_24h2_user_choice_latest_progid_subkey() {
    let mut hive = Hive::default();
    // Win11 24H2 layout: `UserChoiceLatest\ProgId` holds value `ProgId`, `UserChoiceLatest\Hash` holds `Hash`.
    hive.set(&format!(r"{}\UserChoiceLatest\ProgId", file_exts("cmd")), "ProgId", "Rainy.cmd").unwrap();
    hive.set(&format!(r"{}\UserChoiceLatest\Hash", file_exts("cmd")), "Hash", "abc").unwrap();
    // Another editor under `UserChoiceLatest\ProgId` for `.bat` must stay untouched.
    hive.set(&format!(r"{}\UserChoiceLatest\ProgId", file_exts("bat")), "ProgId", r"Applications\Code.exe").unwrap();

    let synced = quietly(&mut hive, EXE, Language::Chinese, OFF, &Memory::default());
    assert!(synced.changed);
    assert!(!hive.has(&format!(r"{}\UserChoiceLatest", file_exts("cmd"))));
    assert!(!hive.has(&format!(r"{}\UserChoiceLatest\ProgId", file_exts("cmd"))));
    assert_eq!(
        hive.text(&format!(r"{}\UserChoiceLatest\ProgId", file_exts("bat")), "ProgId").as_deref(),
        Some(r"Applications\Code.exe")
    );
}

#[test]
fn protects_memory_keep_even_when_rainy_md_is_damaged() {
    let mut hive = Hive::default();
    let synced = quietly(&mut hive, EXE, Language::Chinese, on(false, true), &Memory::default());
    // Corrupt/delete `Rainy.md` and remove `Rainy.rs`, while the user owns `.rs\OpenWithProgids`.
    hive.delete_tree(r"Software\Classes\Rainy.md").unwrap();
    hive.delete_value(r"Software\Classes\.rs\OpenWithProgids", "Rainy.rs").unwrap();
    hive.set(r"Software\Classes\.rs\OpenWithProgids", "User.rs", "").unwrap();

    let repaired = quietly(&mut hive, EXE, Language::Chinese, on(false, true), &synced.memory);
    assert!(repaired.changed);
    assert!(
        repaired.memory.keep.contains(&r"software\classes\.rs\openwithprogids".to_owned()),
        "user key must still be recorded in memory.keep when Rainy.md was damaged: {:?}",
        repaired.memory.keep
    );
}

#[test]
fn sync_saved_fast_skips_steady_startup_and_self_heals_when_sentinel_changes() {
    let folder = tempfile::tempdir().unwrap();
    let settings = folder.path().join("desktop.json");
    std::fs::write(&settings, r#"{"explorerIntegration": {"contextMenu": true, "fileTypes": true}}"#).unwrap();
    let mut registry = Hive::default();
    assert_eq!(sync_saved(&mut registry, &settings, EXE, Language::Chinese).unwrap(), Some(ALL));

    // Steady-state startup hits the fast-skip cache and performs only a handful of sentinel probes instead of 500+.
    registry.reads.set(0);
    assert_eq!(sync_saved(&mut registry, &settings, EXE, Language::Chinese).unwrap(), None);
    assert!(registry.reads.get() <= 10, "expected fast-skip <= 10 registry reads, got {}", registry.reads.get());

    // If a sentinel key is deleted externally, the next sync detects it and heals the full registration.
    registry.delete_tree(r"Software\Classes\Rainy.md").unwrap();
    assert_eq!(sync_saved(&mut registry, &settings, EXE, Language::Chinese).unwrap(), Some(ALL));
    assert!(registry.has(r"Software\Classes\Rainy.md\shell\open\command"));
}

#[test]
fn batch_files_are_given_back_with_the_integration_off_and_on_uninstall() {
    let mut hive = Hive::default();
    hit_by_4_8(&mut hive);
    quietly(&mut hive, EXE, Language::Chinese, ShellIntegration::default(), &Memory::default());
    assert!(!hive.has(&format!(r"{}\UserChoice", file_exts("cmd"))) && !hive.has(&format!(r"{CLASSES}\Rainy.cmd")));
    assert!(hive.has(&format!(r"{CLASSES}\.cmd\OpenWithProgids")));
    assert_eq!(hive.text(&format!(r"{CLASSES}\.cmd\OpenWithProgids"), "Rainy.cmd"), None);
    let mut hive = Hive::default();
    hit_by_4_8(&mut hive);
    assert!(unregister(&mut hive, EXE, &recorded(&[])).unwrap());
    assert!(!hive.has(&format!(r"{}\UserChoice", file_exts("cmd"))) && !hive.has(&format!(r"{CLASSES}\Rainy.bat")));
}

#[test]
fn registers_the_context_menu_on_files_folders_and_backgrounds() {
    let planned = operations(EXE, Language::Chinese, on(true, false), &recorded(&[]), ALL);
    let commands: Vec<&Operation> = planned.iter().filter(|operation| matches!(operation, Operation::Set { key, .. } if key.ends_with(r"\command"))).collect();
    assert_eq!(commands, [
        &set(r"Software\Classes\Directory\Background\shell\Rainy\command", "", &format!("\"{EXE}\" \"%V\"")),
        &set(r"Software\Classes\Directory\shell\Rainy\command", "", &format!("\"{EXE}\" \"%V\"")),
        &set(r"Software\Classes\*\shell\Rainy\command", "", &format!("\"{EXE}\" \"%1\"")),
    ]);
    assert_eq!(menu_last(&planned), Some(&set(r"Software\Classes\*\shell\Rainy\command", "", &format!("\"{EXE}\" \"%1\""))));
    assert!(planned.contains(&set(r"Software\Classes\*\shell\Rainy", "", "通过 Rainy 打开")));
    assert!(planned.contains(&set(r"Software\Classes\Directory\shell\Rainy", "Icon", &format!("\"{EXE}\",0"))));
    assert!(planned.contains(&Operation::RemoveValue {
        key: r"Software\Classes\.md\OpenWithProgids".into(),
        name: "Rainy.md".into(),
        parents: vec![r"Software\Classes\.md".into()],
        keep_key: false,
    }));
    assert!(!planned.iter().any(|operation| matches!(operation, Operation::RemoveKey { key, .. } if key == r"Software\Classes\.md")));
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
    assert_eq!(planned.iter().filter(|operation| matches!(operation, Operation::Set { .. })).count(), 6 + 6 * 75);
    assert!(!planned.iter().any(|operation| matches!(operation, Operation::Set { key, name, .. }
        if name.is_empty() && key.rsplit('\\').next().is_some_and(|last| last.starts_with('.')))));
}

#[test]
fn lists_text_formats_only_each_extension_once() {
    let all: Vec<&str> = extensions().collect();
    assert_eq!(all.len(), 75);
    assert_eq!(all.iter().collect::<HashSet<_>>().len(), 75);
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
        context_menu_complete: true,
        file_types_complete: true,
        missing_extensions: Vec::new(),
    };
    assert!(current(&registered, EXE, Language::Chinese, ALL));
    assert!(!current(&registered, EXE, Language::English, ALL));
    assert!(!current(&registered, r"D:\Rainy\RainyAgent.exe", Language::Chinese, ALL));
    assert!(!current(&registered, EXE, Language::Chinese, on(false, true)));
    assert!(!current(&Status { context_menu_complete: false, ..registered.clone() }, EXE, Language::Chinese, ALL));
    assert!(!current(&Status { file_types_complete: false, ..registered.clone() }, EXE, Language::Chinese, ALL));
    assert!(current(&Status::default(), EXE, Language::Chinese, OFF));
    let types_only = Status { context_menu_label: None, context_menu_command: None, context_menu_traces: false, context_menu_complete: false, ..registered };
    assert!(current(&types_only, EXE, Language::Chinese, on(false, true)));
    assert!(!current(&types_only, EXE, Language::English, on(false, true)));
    assert!(!current(&Status { context_menu_traces: true, ..Status::default() }, EXE, Language::Chinese, OFF));
    assert!(!current(&Status { file_type_traces: true, ..Status::default() }, EXE, Language::Chinese, OFF));
}

#[test]
fn heals_missing_context_verb_and_backfills_extensions_added_in_an_upgrade() {
    let mut registry = Hive::default();
    let synced = quietly(&mut registry, EXE, Language::Chinese, ALL, &Memory::default());
    assert!(synced.changed);
    registry.delete_tree(r"Software\Classes\Directory\shell\Rainy").unwrap();
    registry.delete_tree(r"Software\Classes\Rainy.rs").unwrap();
    registry.delete_value(r"Software\Classes\.rs\OpenWithProgids", "Rainy.rs").unwrap();
    registry.set(r"Software\Classes\.rs\OpenWithProgids", "Other.rs", "").unwrap();

    let status = read_status(&registry).unwrap();
    assert!(!status.context_menu_complete);
    assert!(!status.file_types_complete);
    assert_eq!(status.missing_extensions, ["rs"]);
    assert!(!current(&status, EXE, Language::Chinese, ALL));

    let mut remembered = Vec::new();
    let healed = sync(&mut registry, EXE, Language::Chinese, ALL, &synced.memory, |memory| {
        remembered.push(memory.clone());
        Ok(())
    })
    .unwrap();
    assert!(healed.changed);
    assert!(registry.has(r"Software\Classes\Directory\shell\Rainy\command"));
    assert!(registry.has(r"Software\Classes\Rainy.rs\shell\open\command"));
    assert_eq!(registry.text(r"Software\Classes\.rs\OpenWithProgids", "Rainy.rs").as_deref(), Some(""));
    assert!(healed.memory.keep.contains(&r"software\classes\.rs\openwithprogids".to_owned()));
    assert!(!remembered.is_empty());

    let after = read_status(&registry).unwrap();
    assert!(after.context_menu_complete && after.file_types_complete && after.missing_extensions.is_empty());
    assert!(current(&after, EXE, Language::Chinese, ALL));
    assert!(!quietly(&mut registry, EXE, Language::Chinese, ALL, &healed.memory).changed);
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
        context_menu_complete: true,
        file_types_complete: true,
        missing_extensions: Vec::new(),
    });
    assert!(current(&status, unicode, Language::Chinese, ALL));
    assert!(apply(&mut registry, &operations(unicode, Language::Chinese, OFF, &recorded(&[]), ALL)).unwrap());
    assert_eq!(read_status(&registry).unwrap(), Status::default());
    assert_eq!(registry.paths(), ["software", r"software\classes", r"software\classes\.unrelated"]);
    assert_eq!(registry.notified, 2);
}

#[test]
fn leaves_what_other_applications_and_the_user_had() {
    let mut registry = Hive::default();
    registry.set(r"Software\Classes\.md\OpenWithProgids", "Other.md", "").unwrap();
    registry.set(r"Software\Classes\*\shell\Other", "", "Other").unwrap();
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
    let mut saved = Vec::new();
    let synced = sync(&mut registry, EXE, Language::Chinese, ALL, &Memory::default(), |memory| {
        saved.push(memory.clone());
        Ok(())
    })
    .unwrap();
    assert!(synced.changed);
    assert_eq!(saved.as_slice(), std::slice::from_ref(&synced.memory));
    assert_eq!(synced.memory.recorded, ALL);
    assert!(synced.memory.keep.contains(&r"software\classes\.text\openwithprogids".to_owned()));
    assert!(synced.memory.keep.contains(&r"software\classes\directory\shell".to_owned()));
    assert!(!synced.memory.keep.contains(&r"software\classes\.md".to_owned()));
    let (writes, notified) = (registry.writes, registry.notified);
    assert_eq!(quietly(&mut registry, EXE, Language::Chinese, ALL, &synced.memory), Synced { changed: false, memory: synced.memory.clone() });
    assert_eq!((registry.writes, registry.notified), (writes, notified));
    let menu_off = quietly(&mut registry, EXE, Language::Chinese, on(false, true), &synced.memory);
    assert_eq!(menu_off.memory.recorded, on(false, true));
    assert!(menu_off.memory.keep.contains(&r"software\classes\.text\openwithprogids".to_owned()));
    assert!(!menu_off.memory.keep.contains(&r"software\classes\directory\shell".to_owned()));
    assert_eq!(quietly(&mut registry, EXE, Language::Chinese, OFF, &menu_off.memory), Synced { changed: true, memory: Memory::default() });
    assert!(registry.has(r"Software\Classes\.text\OpenWithProgids"));
    assert!(registry.has(r"Software\Classes\Directory\shell"));
    assert!(!registry.has(r"Software\Classes\Directory\shell\Rainy"));
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
    let mut registry = Hive::default();
    assert!(sync(&mut registry, EXE, Language::Chinese, ALL, &Memory::default(), |_| Err(anyhow::anyhow!("The disk is full"))).is_err());
    assert!(registry.keys.is_empty());
}

#[test]
fn leaves_the_application_key_windows_wrote_for_choose_another_app() {
    let command = format!(r"{APP}\shell\open\command");
    let windows = launch_command(EXE, "%1");
    let mut registry = Hive::default();
    registry.set(&command, "", &windows).unwrap();
    let menu = quietly(&mut registry, EXE, Language::Chinese, on(true, false), &Memory::default());
    assert_eq!(registry.text(&command, ""), Some(windows.clone()));
    let menu = quietly(&mut registry, EXE, Language::English, on(true, false), &menu.memory);
    let off = quietly(&mut registry, EXE, Language::English, OFF, &menu.memory);
    assert_eq!(off.memory, Memory::default());
    unregister(&mut registry, EXE, &off.memory).unwrap();
    assert_eq!(registry.text(&command, ""), Some(windows.clone()));
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
    let mut registry = Hive::default();
    apply(&mut registry, &operations(EXE, Language::Chinese, ALL, &Memory::default(), ALL)).unwrap();
    apply(&mut registry, &planned).unwrap();
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
    assert_eq!(removable_keys(on(false, true)).len(), 4 + 2 * 75);
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
    assert_eq!(registry.writes - writes, 3 + 75 + 1);
    assert_eq!(registry.text(r"Software\Classes\Directory\shell\Rainy", "").as_deref(), Some("Open with Rainy"));
    assert_eq!(registry.text(r"Software\Classes\Rainy.rs", "").as_deref(), Some("Rust Source File"));
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
        for key in [r"Software\Classes\Rainy.md", APP, r"Software\RainyMarks"] {
            registry.delete_tree(key).unwrap();
        }
        registry.delete_value(r"Software\RegisteredApplications", "Rainy").unwrap();
        registry
    };
    let mut registry = left_behind();
    assert!(!quietly(&mut registry, EXE, Language::Chinese, OFF, &Memory::default()).changed);
    assert!(registry.has(r"Software\Classes\Rainy.txt"));
    assert!(unregister(&mut registry, EXE, &Memory::default()).unwrap());
    assert!(!registry.has(r"Software\Classes\Rainy.txt"));
    assert!(registry.has(r"Software\Classes\.md\OpenWithProgids"));
    assert_eq!(registry.text(r"Software\Classes\.md\OpenWithProgids", "Rainy.md"), None);
    assert_eq!(registry.text(r"Software\Classes\.txt\OpenWithProgids", "Other.txt").as_deref(), Some(""));
    assert_eq!(registry.text(r"Software\Classes\.txt\OpenWithProgids", "Rainy.txt"), None);
    assert!(!registry.keys.keys().any(|key| key.contains("rainy.")));
    assert_eq!(registry.notified, 2);
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
    assert!(!quietly(&mut registry, EXE, Language::Chinese, OFF, &theirs).changed);
    assert!(!unregister(&mut registry, EXE, &theirs).unwrap());
    assert_eq!((registry.writes, registry.notified), (writes, notified));
    assert_eq!(read_status(&registry).unwrap().file_type_command, Some(launch_command(&other, "%1")));
    quietly(&mut registry, EXE, Language::Chinese, on(true, false), &theirs);
    assert_eq!(read_status(&registry).unwrap().context_menu_command, Some(launch_command(EXE, "%1")));
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
    assert!(!registry.has(r"Software\Classes\*\shell\Rainy"));
    let status = read_status(&registry).unwrap();
    assert!(status.context_menu_command.is_none() && status.context_menu_traces);
}

fn menu_last(planned: &[Operation]) -> Option<&Operation> {
    planned.iter().rfind(|operation| matches!(operation, Operation::Set { key, .. } if key.contains(r"\shell\Rainy")))
}

#[test]
fn writes_the_values_a_sync_compares_last() {
    let planned = operations(EXE, Language::Chinese, on(false, true), &recorded(&[]), OFF);
    let markdown = r"Software\Classes\Rainy.md";
    assert_eq!(planned.last(), Some(&set(&format!(r"{markdown}\shell\open\command"), "", &launch_command(EXE, "%1"))));
    let names: Vec<usize> = planned
        .iter()
        .enumerate()
        .filter(|(_, operation)| matches!(operation, Operation::Set { key, name, .. } if name.is_empty() && key.rsplit('\\').next().is_some_and(|last| last.starts_with("Rainy."))))
        .map(|(index, _)| index)
        .collect();
    assert_eq!(names.len(), 75);
    assert_eq!(planned.iter().position(|operation| *operation == set(markdown, "", "Markdown 文档")), names.last().copied());
}

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
        let mut finished = Hive { keys: registry.keys.clone(), ..Hive::default() };
        let synced = quietly(&mut finished, EXE, Language::Chinese, wanted, &saved);
        assert!(synced.changed, "{refused}");
        assert_eq!(synced.memory, saved, "{refused}");
        assert_eq!(finished.keys, whole(wanted), "{refused}");
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
    save(&settings, Change { context_menu: Some(false), ..Change::default() }).unwrap();
    assert_eq!(sync_saved(&mut registry, &settings, EXE, Language::Chinese).unwrap(), Some(OFF));
    assert_eq!(targets::read_preferences(&settings).unwrap()["explorerIntegration"], json!({"contextMenu": false, "fileTypes": false}));
    assert!(registry.has(r"Software\Classes\Directory\shell"));
    assert!(!registry.has(r"Software\Classes\*"));
    save(&settings, Change { context_menu: Some(true), ..Change::default() }).unwrap();
    sync_saved(&mut registry, &settings, EXE, Language::Chinese).unwrap();
    assert!(unregister_saved(&mut registry, &settings, EXE).unwrap());
    assert!(registry.has(r"Software\Classes\Directory\shell"));
    assert!(!registry.has(r"Software\Classes\Directory\shell\Rainy"));
    assert!(!registry.has(r"Software\Classes\*"));
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
    assert_eq!(memory(&saved).recorded, ALL);
    assert_eq!(read_status(&registry).unwrap().file_type_command, None);
    registry.refused = None;
    change_saved(&settings, turn_on(), || sync_saved(&mut registry, &settings, EXE, Language::Chinese).map(drop)).unwrap();
    assert_eq!(preference(&targets::read_preferences(&settings).unwrap()), ALL);
    assert_eq!(read_status(&registry).unwrap().file_type_command, Some(launch_command(EXE, "%1")));
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

#[cfg(windows)]
#[test]
fn a_default_app_choice_windows_protects_is_still_taken_back() {
    let scratch = format!(r"Software\RainyTest-choice-{}", std::process::id());
    let choice = format!(r"{scratch}\.cmd\UserChoice");
    let mut registry = CurrentUser;
    registry.set(&choice, "ProgId", "Rainy.cmd").unwrap();
    let script = format!(
        "$k=[Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('{choice}',$true);$a=$k.GetAccessControl();$me=[Security.Principal.WindowsIdentity]::GetCurrent().Name;$a.AddAccessRule((New-Object Security.AccessControl.RegistryAccessRule($me,'SetValue','Deny')));$k.SetAccessControl($a);$k.Close()"
    );
    let denied = std::process::Command::new("powershell").args(["-NoProfile", "-Command", &script]).status().unwrap();
    assert!(denied.success());
    assert!(registry.set(&choice, "ProgId", "x").is_err(), "the deny rule should be in place");
    let remove = Operation::RemoveKey { key: choice.clone(), parents: Vec::new(), marker: Some(("ProgId".into(), "Rainy.cmd".into())) };
    assert!(carry_out(&mut registry, &remove).unwrap());
    assert!(!registry.exists(&choice).unwrap());
    registry.delete_tree(&scratch).unwrap();
    assert!(!registry.exists(&scratch).unwrap());
}

#[cfg(windows)]
#[test]
fn a_redirected_registry_keeps_a_whole_registration_below_its_key() {
    let name = format!("test-{}", std::process::id());
    let root = format!(r"Software\RainyQA\{name}");
    struct Parent;
    impl Drop for Parent {
        fn drop(&mut self) {
            let _ = CurrentUser.delete_key(r"Software\RainyQA");
        }
    }
    let _parent = Parent;
    struct Scratch(String);
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = CurrentUser.delete_tree(&self.0);
        }
    }
    let _scratch = Scratch(root.clone());
    assert!(Redirected::new("a b").is_none() && Redirected::new(r"..\x").is_none() && Redirected::new("").is_none());
    let mut registry = Redirected::new(&name).unwrap();
    let executable = r"C:\QA\Rainy\RainyAgent.exe";
    let wanted = ShellIntegration { context_menu: true, file_types: true };
    let synced = sync(&mut registry, executable, Language::English, wanted, &Memory::default(), |_| Ok(())).unwrap();
    assert!(synced.changed);
    for key in [r"Software\Classes\*\shell\Rainy", r"Software\Classes\Rainy.md", r"Software\Classes\Applications\RainyAgent.exe"] {
        assert!(CurrentUser.exists(&format!(r"{root}\{key}")).unwrap(), "{key}");
    }
    assert_eq!(
        CurrentUser.read(&format!(r"{root}\Software\RegisteredApplications"), "Rainy").unwrap().as_deref(),
        Some(r"Software\RainyMarks\Rainy\Capabilities")
    );
    assert!(!sync(&mut registry, executable, Language::English, wanted, &synced.memory, |_| Ok(())).unwrap().changed);
    sync(&mut registry, executable, Language::English, ShellIntegration::default(), &synced.memory, |_| Ok(())).unwrap();
    assert!(!CurrentUser.exists(&format!(r"{root}\Software\Classes\Rainy.md")).unwrap());
}

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
    registry.delete_tree(&key("a")).unwrap();
    assert!(!registry.exists(&key("a")).unwrap());
    assert!(registry.empty(&scratch.0).unwrap());
    registry.delete_tree(&key("a")).unwrap();
    registry.delete_key(&key("a")).unwrap();
    registry.delete_value(&key("a"), "x").unwrap();
    registry.delete_value(&scratch.0, "x").unwrap();
}
