//! The external Qwen3.8 Flash Next files Strata runs (3.x `main/strata-model.ts`): the main GGUF (first shard), its
//! native pack and tokenizer, and the matching MTP weights, with the offline preparation steps still needed. Weights
//! are never changed, and an imported Strata profile contributes only paths and allocation settings, never commands.

use std::collections::BTreeSet;
use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use anyhow::{Result, anyhow, bail};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest as _, Sha256};

use super::Settings;
use crate::locale::localized;

/// The model files the engine needs.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Model {
    pub source_path: String,
    pub model: String,
    pub gguf_path: String,
    pub pack_path: Option<String>,
    pub tokenizer_path: Option<String>,
    pub mtp_path: Option<String>,
    pub needs_preparation: bool,
}

/// One offline converter that writes only inside Rainy's private model cache.
#[derive(Debug, Clone, PartialEq)]
pub struct Preparation {
    /// `iq_pack.py` or `mtp_rt.py`.
    pub tool: &'static str,
    pub args: Vec<String>,
    pub directory: PathBuf,
    /// The MTP runtime files rather than the main model's pack.
    pub mtp: bool,
}

/// What a selection resolves to.
#[derive(Debug, Clone, PartialEq)]
pub struct Resolved {
    pub model: Model,
    pub settings: Settings,
    pub preparations: Vec<Preparation>,
    pub missing: Vec<String>,
}

fn is_file(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|info| info.is_file() && info.len() > 0)
}

fn has_files(root: &Path, names: &[&str]) -> bool {
    names.iter().all(|name| is_file(&root.join(name)))
}

fn text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn name_of(path: &Path) -> String {
    path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default()
}

fn read_json(path: &Path) -> Result<Value> {
    if fs::metadata(path)?.len() > 65536 {
        bail!(localized("Strata 配置文件超过 64 KiB。", "The Strata profile is larger than 64 KiB."));
    }
    let text = fs::read_to_string(path)?;
    Ok(serde_json::from_str(text.trim_start_matches('\u{feff}'))?)
}

/// `path` as Node's `path.resolve` would give it: absolute, with `.` and `..` worked out.
fn absolute(path: &Path) -> PathBuf {
    let joined = if path.is_absolute() { path.to_path_buf() } else { std::env::current_dir().unwrap_or_default().join(path) };
    let mut result = PathBuf::new();
    for part in joined.components() {
        match part {
            std::path::Component::ParentDir => {
                result.pop();
            }
            std::path::Component::CurDir => {}
            other => result.push(other),
        }
    }
    result
}

fn absolute_from(base: &Path, value: &str) -> PathBuf {
    absolute(&base.join(value))
}

fn argument<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.iter().position(|arg| arg == flag).and_then(|at| args.get(at + 1)).map(String::as_str)
}

static SPLIT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^(.*)-(\d{5})-of-(\d{5})\.gguf$").unwrap());

/// The first shard of a GGUF model and all its shards; every shard must be present and the first must be a GGUF.
fn first_gguf(path: &Path) -> Result<(PathBuf, Vec<PathBuf>)> {
    let folder = path.parent().unwrap_or(Path::new("")).to_path_buf();
    let (first, shards) = match SPLIT.captures(&name_of(path)) {
        Some(split) => {
            let total: usize = split[3].parse().unwrap_or(0);
            if !(1..=128).contains(&total) {
                bail!(localized("GGUF 分片数量无效。", "The number of GGUF shards is invalid."));
            }
            let shard = |index: usize| folder.join(format!("{}-{index:05}-of-{}.gguf", &split[1], &split[3]));
            (shard(1), (1..=total).map(shard).collect())
        }
        None => (path.to_path_buf(), vec![path.to_path_buf()]),
    };
    for shard in &shards {
        if !is_file(shard) {
            bail!(localized(&format!("缺少模型分片：{}", name_of(shard)), &format!("A model shard is missing: {}", name_of(shard))));
        }
    }
    let mut magic = [0u8; 4];
    let ok = fs::File::open(&first).and_then(|mut file| file.read_exact(&mut magic)).is_ok() && &magic == b"GGUF";
    if !ok {
        bail!(localized("所选文件不是 GGUF 模型。", "The selected file is not a GGUF model."));
    }
    Ok((first, shards))
}

fn files_in(root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(root) else { return Vec::new() };
    let mut files: Vec<PathBuf> = entries.flatten().filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file())).map(|entry| entry.path()).collect();
    files.sort();
    files
}

fn lower_name(path: &Path) -> String {
    name_of(path).to_lowercase()
}

fn is_profile(path: &Path) -> bool {
    let name = lower_name(path);
    name.starts_with("strata-") && name.ends_with(".json") && !name.ends_with(".shared-settings.json")
}

fn is_gguf(path: &Path) -> bool {
    lower_name(path).ends_with(".gguf")
}

/// The model of a folder: its one Strata profile, else its one main GGUF (in it or under `models/`).
fn model_in_directory(root: &Path) -> Result<PathBuf> {
    let direct = files_in(root);
    let profiles: Vec<&PathBuf> = direct.iter().filter(|path| is_profile(path)).collect();
    if profiles.len() == 1 {
        return Ok(profiles[0].clone());
    }
    let mut candidates: Vec<PathBuf> = direct.iter().filter(|path| is_gguf(path) && !lower_name(path).contains("mtp")).cloned().collect();
    if candidates.is_empty() {
        let models = root.join("models");
        let entries: Vec<fs::DirEntry> = fs::read_dir(&models).map(|entries| entries.flatten().collect()).unwrap_or_default();
        if entries.len() > 128 {
            bail!(localized("模型目录包含过多项目，请直接选择主模型的 GGUF 文件。", "The model folder contains too many items. Select the GGUF file of the main model directly."));
        }
        for entry in entries {
            let path = entry.path();
            if path.is_dir() {
                candidates.extend(files_in(&path).into_iter().filter(|path| is_gguf(path)));
            } else if is_gguf(&path) {
                candidates.push(path);
            }
        }
    }
    static SHARD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)-\d{5}-of-\d{5}\.gguf$").unwrap());
    static FIRST_SHARD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)-00001-of-\d{5}\.gguf$").unwrap());
    let first: Vec<PathBuf> = candidates
        .into_iter()
        .filter(|path| !lower_name(path).contains("mtp") && (!SHARD.is_match(&text(path)) || FIRST_SHARD.is_match(&text(path))))
        .collect();
    match first.len() {
        1 => Ok(first[0].clone()),
        0 => Err(anyhow!(localized("目录中没有找到主模型 GGUF，请选择完整模型目录或首个分片。", "No main model GGUF was found in the folder. Select a complete model folder or the first shard."))),
        _ => Err(anyhow!(localized("目录包含多个模型，请直接选择需要的首个 GGUF 分片。", "The folder contains more than one model. Select the first GGUF shard of the model you want."))),
    }
}

/// A JavaScript number as `JSON.stringify` writes it.
fn js_number(value: f64) -> String {
    if value.fract() == 0.0 && value.abs() < 1e21 { format!("{}", value as i64) } else { format!("{value}") }
}

/// The cache folder name of derived files, as 3.x computed it, so packs it prepared are found again.
fn fingerprint(paths: &[PathBuf]) -> Result<String> {
    let mut facts = Vec::new();
    for path in paths {
        let info = fs::metadata(path)?;
        let modified = info.modified()?.duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
        let ms = modified.as_secs() as f64 * 1000.0 + f64::from(modified.subsec_nanos()) / 1e6;
        facts.push(format!("{{\"path\":{},\"size\":{},\"mtimeMs\":{}}}", serde_json::to_string(&text(&absolute(path)))?, info.len(), js_number(ms)));
    }
    let digest = Sha256::digest(format!("[{}]", facts.join(",")).as_bytes());
    Ok(digest.iter().take(12).map(|byte| format!("{byte:02x}")).collect())
}

fn pack_ready(pack: &Path, tokenizer: &Path) -> bool {
    has_files(pack, &["index.txt", "dense.bin"])
        && (is_file(&pack.join("native_experts.txt")) || is_file(&pack.join("experts.bin")))
        && has_files(tokenizer, &["vocab.json", "merges.txt", "token_type.json"])
}

const MTP_FILES: [&str; 3] = ["dense.bin", "dense.txt", "experts.bin"];

/// The MTP runtime files: given, found near the model, or prepared from an MTP GGUF.
fn resolve_mtp(source: &str, candidates: &[PathBuf], cache: &Path) -> Result<(Option<PathBuf>, Vec<Preparation>)> {
    let mut selected = if source.trim().is_empty() { None } else { Some(absolute(Path::new(source))) };
    if selected.is_none() {
        selected = candidates.iter().find(|candidate| has_files(candidate, &MTP_FILES) || (is_file(candidate) && is_gguf(candidate))).cloned();
    }
    let Some(mut selected) = selected else { return Ok((None, Vec::new())) };
    if has_files(&selected, &MTP_FILES) {
        return Ok((Some(selected), Vec::new()));
    }
    if fs::metadata(&selected)?.is_dir() {
        if has_files(&selected.join("rt"), &MTP_FILES) {
            return Ok((Some(selected.join("rt")), Vec::new()));
        }
        let packed = selected.join("mtp-q2_0.gguf");
        if !is_file(&packed) {
            bail!(localized("MTP 目录需要 dense.bin、dense.txt、experts.bin，或配套的 mtp-q2_0.gguf。", "An MTP folder needs dense.bin, dense.txt and experts.bin, or the matching mtp-q2_0.gguf."));
        }
        selected = packed;
    }
    let (first, shards) = first_gguf(&selected)?;
    let output = cache.join("mtp").join(fingerprint(&shards)?);
    if has_files(&output, &["dense.bin", "dense.txt", "experts.bin", ".rainy-complete.json"]) {
        return Ok((Some(output), Vec::new()));
    }
    let preparation = Preparation { tool: "mtp_rt.py", args: vec!["--gguf".into(), text(&first), "--out".into(), text(&output)], directory: output.clone(), mtp: true };
    Ok((Some(output), vec![preparation]))
}

#[derive(Deserialize)]
struct Profile {
    args: Vec<String>,
    cwd: Option<String>,
    tokenizer: Option<String>,
    model_name: Option<String>,
    port: Option<u16>,
}

/// Inspect the selected files; `import_defaults` takes a newly chosen profile's allocation settings. `None` when no model
/// is selected.
pub fn resolve(input: &Settings, cache: &Path, import_defaults: bool) -> Result<Option<Resolved>> {
    if input.model_path.trim().is_empty() {
        return Ok(None);
    }
    let source = absolute(Path::new(input.model_path.trim()));
    let selected = if fs::metadata(&source)?.is_dir() { model_in_directory(&source)? } else { source.clone() };
    let mut settings = Settings { model_path: text(&source), ..input.clone() };
    let (mut native, mut explicit_pack, mut explicit_tokenizer, mut explicit_mtp, mut model_name) = (selected.clone(), None, None, None, None);
    if lower_name(&selected).ends_with(".json") {
        let profile: Profile = serde_json::from_value(read_json(&selected)?)?;
        if profile.args.len() > 256 {
            bail!(localized("Strata 配置的参数过多。", "The Strata profile has too many arguments."));
        }
        let folder = selected.parent().unwrap_or(Path::new("")).to_path_buf();
        let base = profile.cwd.as_deref().map_or(folder.clone(), |cwd| absolute_from(&folder, cwd));
        let path = argument(&profile.args, "--native")
            .ok_or_else(|| anyhow!(localized("此 Strata 配置没有外部主模型 GGUF（--native）。", "This Strata profile has no external main model GGUF (--native).")))?;
        native = absolute_from(&base, path);
        explicit_pack = argument(&profile.args, "--pack").map(|pack| absolute_from(&base, pack));
        explicit_tokenizer = profile.tokenizer.as_deref().map(|tokenizer| absolute_from(&base, tokenizer));
        explicit_mtp = argument(&profile.args, "--mtp").map(|mtp| absolute_from(&base, mtp));
        model_name = profile.model_name.clone();
        if import_defaults {
            let number = |flag: &str| argument(&profile.args, flag).and_then(|value| value.parse::<f64>().ok());
            if let Some(context) = number("--max-context") {
                settings.context_window = context as u32;
            }
            if let Some(port) = profile.port {
                settings.port = port;
            }
            if let Some(kv) = argument(&profile.args, "--kv") {
                settings.kv_cache = kv.to_owned();
            }
            if let Some(reserve) = number("--vram-reserve-mib") {
                settings.vram_reserve_mib = reserve as u64;
            }
            if let Some(budget) = number("--resident-budget-gib") {
                settings.resident_budget_gib = Some(budget);
            }
            if let Some(mtp) = &explicit_mtp {
                settings.mtp_path = text(mtp);
            }
            settings.check()?;
        }
    } else if !is_gguf(&selected) {
        bail!(localized("请选择 Qwen3.8 Flash Next GGUF、模型目录或已有 Strata JSON 配置。", "Select a Qwen3.8 Flash Next GGUF, a model folder, or an existing Strata JSON profile."));
    }
    let (gguf, shards) = first_gguf(&native)?;
    let folder = gguf.parent().unwrap_or(Path::new("")).to_path_buf();
    let mut preparations = Vec::new();
    let mut pack_candidates: Vec<PathBuf> = explicit_pack.iter().cloned().collect();
    for candidate in [folder.join("pack"), absolute(&folder.join("../../packs").join(name_of(&folder)))] {
        if !pack_candidates.contains(&candidate) {
            pack_candidates.push(candidate);
        }
    }
    let mut found = None;
    for candidate in pack_candidates {
        let tokenizer = match (&explicit_pack, &explicit_tokenizer) {
            (Some(pack), Some(tokenizer)) if *pack == candidate => tokenizer.clone(),
            _ => candidate.join("tokenizer"),
        };
        if pack_ready(&candidate, &tokenizer) {
            found = Some((candidate, tokenizer));
            break;
        }
    }
    let (pack, tokenizer) = match found {
        Some(found) => found,
        None => {
            let pack = cache.join("packs").join(fingerprint(&shards)?);
            let tokenizer = pack.join("tokenizer");
            if !pack_ready(&pack, &tokenizer) || !is_file(&pack.join(".rainy-complete.json")) {
                preparations.push(Preparation {
                    tool: "iq_pack.py",
                    args: vec!["--gguf".into(), text(&gguf), "--out".into(), text(&pack), "--compat-bf16".into()],
                    directory: pack.clone(),
                    mtp: false,
                });
            }
            (pack, tokenizer)
        }
    };
    let parent = folder.parent().unwrap_or(&folder).to_path_buf();
    let grandparent = parent.parent().unwrap_or(&parent).to_path_buf();
    let mut mtp_candidates: Vec<PathBuf> = explicit_mtp.iter().cloned().collect();
    for base in [&folder, &parent, &grandparent] {
        mtp_candidates.extend([base.join("mtp/rt"), base.join("mtp"), base.join("mtp/mtp-q2_0.gguf"), base.join("mtp-q2_0.gguf")]);
    }
    let (mtp, mtp_preparations) = resolve_mtp(&settings.mtp_path, &mtp_candidates, cache)?;
    preparations.extend(mtp_preparations);
    let model = model_name.map(|name| name.trim().to_owned()).filter(|name| !name.is_empty()).unwrap_or_else(|| {
        static SUFFIX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"-\d{5}-of-\d{5}$").unwrap());
        let stem = name_of(&gguf);
        let stem = stem.strip_suffix(".gguf").or_else(|| stem.strip_suffix(".GGUF")).unwrap_or(&stem).to_owned();
        SUFFIX.replace(&stem, "").to_lowercase()
    });
    let missing = if mtp.is_some() {
        Vec::new()
    } else {
        vec![localized(
            "缺少配套 MTP 权重。请选择 MTP GGUF 或包含 dense.bin、dense.txt、experts.bin 的目录。",
            "The matching MTP weights are missing. Select an MTP GGUF or a folder that contains dense.bin, dense.txt and experts.bin.",
        )]
    };
    let needs_preparation = !preparations.is_empty();
    Ok(Some(Resolved {
        model: Model {
            source_path: text(&source),
            model,
            gguf_path: text(&gguf),
            pack_path: Some(text(&pack)),
            tokenizer_path: Some(text(&tokenizer)),
            mtp_path: mtp.as_deref().map(text),
            needs_preparation,
        },
        settings,
        preparations,
        missing,
    }))
}

/// An installed Strata profile the user may import.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProfileChoice {
    pub path: String,
    pub label: String,
}

/// The profiles Strata's own installer recorded (`%APPDATA%\Strata\settings.json` `installs`); nothing else is scanned.
pub fn discover_profiles(settings: Option<&Path>) -> Vec<ProfileChoice> {
    let Some(value) = settings.and_then(|path| read_json(path).ok()) else { return Vec::new() };
    let installs: Vec<String> = value.get("installs").and_then(Value::as_array).map(|items| items.iter().take(20).filter_map(|item| item.as_str().map(str::to_owned)).collect()).unwrap_or_default();
    let mut found = BTreeSet::new();
    for root in installs.iter().map(PathBuf::from).filter(|root| root.is_absolute()) {
        for path in files_in(&root).into_iter().filter(|path| is_profile(path)) {
            let stem = name_of(&path);
            let label = stem.strip_suffix(".json").unwrap_or(&stem).strip_prefix("strata-").unwrap_or(&stem).to_owned();
            found.insert((text(&path), label));
        }
    }
    found.into_iter().map(|(path, label)| ProfileChoice { path, label }).collect()
}

#[cfg(test)]
mod tests {
    use ::core::prelude::v1::test;

    use super::*;

    fn write(path: &Path, bytes: &[u8]) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }

    #[test]
    fn resolves_a_sharded_model_with_its_pack_and_mtp() {
        let dir = std::env::temp_dir().join(format!("rainy-strata-model-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let models = dir.join("Qwen3.8-Flash-Next");
        write(&models.join("Qwen3.8-Flash-Next-Q2-00001-of-00002.gguf"), b"GGUFdata");
        write(&models.join("Qwen3.8-Flash-Next-Q2-00002-of-00002.gguf"), b"more");
        for name in ["index.txt", "dense.bin", "experts.bin", "tokenizer/vocab.json", "tokenizer/merges.txt", "tokenizer/token_type.json"] {
            write(&models.join("pack").join(name), b"x");
        }
        for name in MTP_FILES {
            write(&models.join("mtp/rt").join(name), b"x");
        }
        let settings = Settings { model_path: text(&models.join("Qwen3.8-Flash-Next-Q2-00002-of-00002.gguf")), ..Settings::default() };
        let resolved = resolve(&settings, &dir.join("cache"), false).unwrap().unwrap();
        assert_eq!(resolved.model.model, "qwen3.8-flash-next-q2");
        assert!(resolved.model.gguf_path.ends_with("-00001-of-00002.gguf"));
        assert_eq!(resolved.model.pack_path.as_deref(), Some(text(&models.join("pack")).as_str()));
        assert_eq!(resolved.model.mtp_path.as_deref(), Some(text(&models.join("mtp/rt")).as_str()));
        assert!(resolved.preparations.is_empty() && resolved.missing.is_empty());
        // Without a pack, one is prepared into the cache; a missing shard is reported.
        fs::remove_dir_all(models.join("pack")).unwrap();
        let resolved = resolve(&settings, &dir.join("cache"), false).unwrap().unwrap();
        assert_eq!(resolved.preparations.len(), 1);
        assert_eq!(resolved.preparations[0].tool, "iq_pack.py");
        fs::remove_file(models.join("Qwen3.8-Flash-Next-Q2-00002-of-00002.gguf")).unwrap();
        assert!(resolve(&Settings { model_path: text(&models.join("Qwen3.8-Flash-Next-Q2-00001-of-00002.gguf")), ..Settings::default() }, &dir, false).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn fingerprints_as_javascript_did() {
        assert_eq!(js_number(1729584000123.0), "1729584000123");
        // Both print the shortest text that reads back as the same number (JavaScript gives "1729584000123.4568" too).
        assert_eq!(js_number(1729584000123.4567), "1729584000123.4568");
    }
}
