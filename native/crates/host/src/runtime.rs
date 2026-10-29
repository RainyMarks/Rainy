//! Interpreter discovery and per-project selection, served through the `runtime` method (3.x `runtime/`).
//! Probes run small computations with the interpreter itself and never install or change anything.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::Mutex as AsyncMutex;

use crate::activity::Activity;
use crate::env::HostEnv;
use crate::error::{HostError, HostResult};
use crate::locale::l;
use crate::paths::contains;
use crate::process::{resolve_powershell, scrubbed_env};
use crate::projects::Projects;
use crate::registry::Registry;
use crate::store::{read_json, write_json};
use crate::text::sha256_hex;

pub const LANGUAGES: [&str; 5] = ["python", "node", "php", "c", "cpp"];
const PROBE_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_CANDIDATES: usize = 16;
const PROBE_CONCURRENCY: usize = 4;

const PYTHON_PROBE: &str = r#"import sys,json,platform,importlib
checks=[]
def test(name,fn):
 try: fn(); checks.append({'name':name,'ready':True})
 except Exception as error: checks.append({'name':name,'ready':False,'detail':str(error)[:300]})
test('python',lambda:compile('x=1+1','<runtime-probe>','exec'))
test('pip',lambda:importlib.import_module('pip'))
test('numpy',lambda:importlib.import_module('numpy').dot([1,2],[3,4]))
test('pandas',lambda:importlib.import_module('pandas').DataFrame({'x':[1,2]}).sum())
test('scipy',lambda:importlib.import_module('scipy.linalg').det([[1,0],[0,1]]))
test('scikit-learn',lambda:importlib.import_module('sklearn.tree').DecisionTreeClassifier().fit([[0],[1]],[0,1]))
test('pillow',lambda:importlib.import_module('PIL.Image').new('RGB',(2,2)).getpixel((0,0)))
def cpu():
 torch=importlib.import_module('torch'); x=torch.ones(2,requires_grad=True); (x*x).sum().backward()
def cuda():
 torch=importlib.import_module('torch')
 if not torch.cuda.is_available(): raise RuntimeError('CUDA is not available in this environment')
 x=torch.ones((2,2),device='cuda',requires_grad=True); result=(x@x).sum(); result.backward(); value=result.item()
 if value!=8: raise RuntimeError('CUDA computation returned an unexpected result')
 if x.grad.sum().item()!=16: raise RuntimeError('CUDA backward computation returned an unexpected result')
def vision():
 torch=importlib.import_module('torch'); module=importlib.import_module('torchvision')
 module.ops.nms(torch.tensor([[0.,0.,2.,2.],[0.,0.,1.,1.]]),torch.tensor([0.9,0.8]),0.5)
def audio():
 torch=importlib.import_module('torch'); module=importlib.import_module('torchaudio')
 module.functional.resample(torch.ones(1,160),16000,8000)
test('torch-cpu',cpu)
test('torch-cuda',cuda)
test('torchvision',vision)
test('torchaudio',audio)
for name in ['sympy','matplotlib','cv2','transformers','datasets','accelerate','safetensors','tokenizers','sentencepiece']:
 test(name,lambda name=name:importlib.import_module(name))
print(json.dumps({'version':platform.python_version(),'platform':'windows' if sys.platform=='win32' else 'linux' if sys.platform.startswith('linux') else sys.platform,'executable':sys.executable,'prefix':sys.prefix,'capabilities':checks}))"#;
const NODE_PROBE: &str = "console.log(JSON.stringify({version:process.versions.node,platform:process.platform==='win32'?'windows':process.platform,executable:process.execPath,capabilities:[{name:'node',ready:2+2===4}]}))";
const PHP_PROBE: &str = "echo json_encode(['version'=>PHP_VERSION,'platform'=>PHP_OS_FAMILY==='Windows'?'windows':strtolower(PHP_OS_FAMILY),'executable'=>PHP_BINARY,'capabilities'=>array_map(fn($name)=>['name'=>$name,'ready'=>extension_loaded($name)],['json','openssl','mbstring','pdo','pdo_sqlite','curl','zip'])]);";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Capability {
    pub name: String,
    pub ready: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Candidate {
    pub id: String,
    pub language: String,
    pub path: String,
    pub source: String,
    pub platform: String,
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>,
    pub ready: bool,
    pub capabilities: Vec<Capability>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Selection {
    root: String,
    selected: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    python_prefix: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SelectionFile {
    version: u8,
    workspaces: BTreeMap<String, Selection>,
}

/// What a process in a project folder gets: PATH and friends, the chosen executables, and the component's PowerShell.
#[derive(Debug, Clone, Default)]
pub struct Resolved {
    pub environment: BTreeMap<String, String>,
    pub executables: BTreeMap<String, String>,
    pub powershell: Option<PathBuf>,
}

fn platform() -> &'static str {
    if cfg!(windows) { "windows" } else { "linux" }
}

fn command_name(language: &str) -> String {
    let name = match language {
        "python" if cfg!(windows) => "python",
        "python" => "python3",
        "node" => "node",
        "php" => "php",
        "c" => "gcc",
        _ => "g++",
    };
    if cfg!(windows) { format!("{name}.exe") } else { name.to_owned() }
}

fn identity(target: &str, language: &str, path: &str) -> String {
    sha256_hex(json!([target, language, path]).to_string())
}

async fn run_probe(executable: &str, args: &[&str], environment: &[(&str, String)]) -> Result<String, String> {
    let mut env = scrubbed_env();
    for (name, value) in [("PYTHONDONTWRITEBYTECODE", "1".to_owned()), ("HF_HUB_OFFLINE", "1".to_owned()), ("TRANSFORMERS_OFFLINE", "1".to_owned())].into_iter().chain(environment.iter().map(|(name, value)| (*name, value.clone()))) {
        env.insert(name.into(), value.into());
    }
    let cwd = std::env::temp_dir();
    match crate::process::run(executable, args, &cwd, &env, 256 * 1024, Some(PROBE_TIMEOUT)).await {
        Ok(output) if output.success => Ok(String::from_utf8_lossy(&output.stdout).into_owned()),
        Ok(output) => Err(crate::text::truncate_chars(String::from_utf8_lossy(&output.stderr).trim(), 1000).to_owned()).map_err(|error: String| if error.is_empty() { format!("exit code {:?}", output.code) } else { error }),
        Err(error) => Err(error.to_string()),
    }
}

/// Probe one interpreter or compiler: a separate observation for the program and each library.
pub async fn probe(target: &str, language: &str, path: &str, source: &str) -> Candidate {
    let mut candidate = Candidate {
        id: identity(target, language, path),
        language: language.to_owned(),
        path: path.to_owned(),
        source: source.to_owned(),
        platform: platform().to_owned(),
        version: None,
        prefix: None,
        ready: false,
        capabilities: Vec::new(),
        error: None,
    };
    let result: Result<(), String> = async {
        if !Path::new(path).is_absolute() || path.contains('\0') {
            return Err("Invalid path".into());
        }
        if language == "c" || language == "cpp" {
            let output = run_probe(path, &["--version"], &[]).await?;
            let version = output.lines().find(|line| !line.trim().is_empty()).unwrap_or_default().to_owned();
            let lower = version.to_lowercase();
            if !(lower.contains("gcc") || lower.contains("g++") || lower.contains("clang") || lower.contains("llvm")) {
                return Err(l("这个程序不是受支持的 C/C++ 编译器（GCC 或 Clang）。", "The executable did not identify a supported C/C++ compiler."));
            }
            let triple = run_probe(path, &["-dumpmachine"], &[]).await?.trim().to_lowercase();
            let compiler = if ["windows", "mingw", "cygwin", "msvc"].iter().any(|word| triple.contains(word)) {
                "windows"
            } else if triple.contains("linux") {
                "linux"
            } else {
                ""
            };
            if compiler != platform() {
                return Err(l("这个编译器生成的程序属于另一个执行平台。", "The compiler produces programs for another execution platform."));
            }
            candidate.version = Some(version);
            candidate.ready = true;
            candidate.capabilities = vec![Capability { name: "compiler-start".into(), ready: true, detail: None }];
            return Ok(());
        }
        let extension_dir = Path::new(path).parent().map(|folder| folder.join("ext").display().to_string()).unwrap_or_default();
        let output = match language {
            "python" => run_probe(path, &["-I", "-B", "-c", PYTHON_PROBE], &[]).await?,
            "node" => run_probe(path, &["-e", NODE_PROBE], &[]).await?,
            _ if cfg!(windows) => run_probe(path, &["-r", PHP_PROBE], &[("RAINY_PHP_EXTENSION_DIR", extension_dir)]).await?,
            _ => run_probe(path, &["-r", PHP_PROBE], &[]).await?,
        };
        let result: Value = serde_json::from_str(output.trim()).map_err(|_| l("检测解释器失败。", "Interpreter inspection failed."))?;
        let reported = result["platform"].as_str().unwrap_or_default();
        if reported != platform() {
            return Err(l(&format!("这个解释器运行在 {reported} 上，而当前项目运行在 {} 上。", platform()), &format!("The interpreter runs on {reported}, but this workspace runs on {}.", platform())));
        }
        candidate.path = result["executable"].as_str().filter(|path| !path.is_empty()).unwrap_or(path).to_owned();
        candidate.version = result["version"].as_str().map(str::to_owned);
        candidate.prefix = result["prefix"].as_str().map(str::to_owned);
        candidate.capabilities = serde_json::from_value(result["capabilities"].clone()).unwrap_or_default();
        candidate.ready = true;
        Ok(())
    }
    .await;
    if let Err(error) = result {
        candidate.error = Some(crate::text::truncate_chars(&error, 1000).to_owned());
    }
    candidate
}

/// The runtime service of one Host.
pub struct Runtime {
    env: HostEnv,
    projects: Arc<Projects>,
    registry: Arc<Registry>,
    activity: Arc<Activity>,
    state: Mutex<SelectionFile>,
    writes: AsyncMutex<()>,
    observations: Mutex<HashMap<String, Vec<Candidate>>>,
    components: Mutex<(Vec<(String, PathBuf)>, BTreeMap<String, String>)>,
    operations: Arc<std::sync::atomic::AtomicUsize>,
    listeners: Mutex<Vec<Box<dyn Fn(&str) + Send + Sync>>>,
}

impl Runtime {
    pub fn new(env: HostEnv, projects: Arc<Projects>, registry: Arc<Registry>, activity: Arc<Activity>) -> std::io::Result<Arc<Self>> {
        let state = read_json::<SelectionFile>(&env.home.join("runtime-environments.json"))?.unwrap_or(SelectionFile { version: 1, workspaces: BTreeMap::new() });
        let operations = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = operations.clone();
        activity.add(move || counter.load(std::sync::atomic::Ordering::Relaxed) > 0);
        let runtime = Arc::new(Self {
            env,
            projects,
            registry,
            activity,
            state: Mutex::new(state),
            writes: AsyncMutex::new(()),
            observations: Mutex::default(),
            components: Mutex::default(),
            operations,
            listeners: Mutex::default(),
        });
        runtime.refresh_components()?;
        Ok(runtime)
    }

    /// Called with the project whose selection was saved, so processes started with the old PATH can be replaced.
    pub fn on_selection_change(&self, listener: impl Fn(&str) + Send + Sync + 'static) {
        self.listeners.lock().unwrap().push(Box::new(listener));
    }

    /// The installed offline components (`active.json` of the toolchain root) and their default executables.
    fn refresh_components(&self) -> std::io::Result<()> {
        let root = &self.env.toolchain_root;
        let Some(active) = read_json::<BTreeMap<String, String>>(&root.join("active.json"))? else {
            *self.components.lock().unwrap() = Default::default();
            return Ok(());
        };
        let platform = platform();
        let mut entries = Vec::new();
        let mut defaults = BTreeMap::new();
        for (id, spelling) in active {
            let kinds = ["basic", "science-cpu", "science-cuda", "cpp", "development"];
            let valid_id = id.split_once('-').is_some_and(|(os, kind)| (os == "windows" || os == "linux") && kinds.contains(&kind));
            let generation = spelling.strip_prefix(&format!("{id}/")).is_some_and(|hash| (hash.len() == 32 || hash.len() == 64) && hash.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)));
            if !valid_id || !generation || !id.starts_with(&format!("{platform}-")) {
                return Err(std::io::Error::other("An installed component has an invalid execution target or generation."));
            }
            let path = root.join(&id).join(&spelling[id.len() + 1..]);
            if id == format!("{platform}-basic") {
                let join = |parts: &[&str]| parts.iter().fold(path.clone(), |path, part| path.join(part)).display().to_string();
                if cfg!(windows) {
                    defaults.insert("python".into(), join(&["python", "python.exe"]));
                    defaults.insert("node".into(), join(&["node", "node.exe"]));
                    defaults.insert("php".into(), join(&["php", "php.exe"]));
                } else {
                    defaults.insert("python".into(), join(&["python", "bin", "python3"]));
                    defaults.insert("node".into(), join(&["node", "bin", "node"]));
                }
            }
            if id == "windows-cpp" {
                defaults.insert("c".into(), path.join("cpp").join("bin").join("clang.exe").display().to_string());
                defaults.insert("cpp".into(), path.join("cpp").join("bin").join("clang++.exe").display().to_string());
            }
            entries.push((id, path));
        }
        *self.components.lock().unwrap() = (entries, defaults);
        Ok(())
    }

    fn workspace_root(&self, workspace_id: &str) -> HostResult<String> {
        self.projects.get(workspace_id).map(|project| project.path).ok_or_else(|| HostError::other(l("当前执行环境中没有所选项目。", "The selected workspace does not exist in this execution target.")))
    }

    fn builtin(&self) -> BTreeMap<String, String> {
        self.env.builtin_php.iter().map(|php| ("php".to_owned(), php.display().to_string())).collect()
    }

    fn candidates(&self, workspace_id: &str) -> HostResult<Vec<(String, String, &'static str)>> {
        let root = PathBuf::from(self.workspace_root(workspace_id)?);
        let mut found: Vec<(String, String, &'static str)> = Vec::new();
        let mut add = |language: &str, path: PathBuf, source: &'static str| {
            if found.len() >= MAX_CANDIDATES || !path.is_absolute() || !path.is_file() {
                return;
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                if std::fs::metadata(&path).map(|info| info.permissions().mode() & 0o111 == 0).unwrap_or(true) {
                    return;
                }
            }
            let text = path.display().to_string();
            let key = |text: &str| if cfg!(windows) { text.to_lowercase() } else { text.to_owned() };
            if !found.iter().any(|(known_language, known, _)| known_language == language && key(known) == key(&text)) {
                found.push((language.to_owned(), text, source));
            }
        };
        let saved = self.state.lock().unwrap().workspaces.get(workspace_id).map(|selection| selection.selected.clone()).unwrap_or_default();
        for (language, path) in saved {
            add(&language, PathBuf::from(path), "manual");
        }
        for (language, path) in self.builtin() {
            add(&language, PathBuf::from(path), "bundled");
        }
        for name in [".venv", "venv"] {
            let path = if cfg!(windows) { root.join(name).join("Scripts").join("python.exe") } else { root.join(name).join("bin").join("python") };
            add("python", path, "project");
        }
        let components = self.components.lock().unwrap().0.clone();
        for (_, component) in &components {
            for language in LANGUAGES {
                let directory = if language == "c" || language == "cpp" { "cpp" } else { language };
                add(language, component.join(directory).join("bin").join(command_name(language)), "bundled");
                add(language, component.join(directory).join(command_name(language)), "bundled");
                if cfg!(windows) && (language == "c" || language == "cpp") {
                    add(language, component.join(directory).join("bin").join(if language == "c" { "clang.exe" } else { "clang++.exe" }), "bundled");
                }
            }
        }
        let home = crate::env::home_dir();
        let python_in = |prefix: &Path| if cfg!(windows) { prefix.join("python.exe") } else { prefix.join("bin").join("python") };
        if let Ok(text) = std::fs::read_to_string(home.join(".conda").join("environments.txt")) {
            for prefix in text.lines().filter(|line| !line.trim().is_empty()).take(MAX_CANDIDATES) {
                add("python", python_in(Path::new(prefix.trim())), "conda");
            }
        }
        for prefix in ["miniforge3", "miniconda3", "anaconda3"] {
            add("python", python_in(&home.join(prefix)), "conda");
        }
        if cfg!(windows) {
            let local = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(|| home.join("AppData").join("Local"));
            if let Ok(entries) = std::fs::read_dir(local.join("Programs").join("Python")) {
                for entry in entries.flatten().take(MAX_CANDIDATES) {
                    if entry.path().is_dir() {
                        add("python", entry.path().join("python.exe"), "system");
                    }
                }
            }
        }
        if let Some(path) = std::env::var_os("PATH") {
            for directory in std::env::split_paths(&path).filter(|entry| !entry.as_os_str().is_empty()) {
                let directory = PathBuf::from(directory.to_string_lossy().trim_matches('"'));
                for language in LANGUAGES {
                    add(language, directory.join(command_name(language)), "system");
                }
                if cfg!(not(windows)) {
                    add("python", directory.join("python"), "system");
                }
            }
        }
        Ok(found)
    }

    fn component_powershell(&self) -> Option<PathBuf> {
        if !cfg!(windows) {
            return None;
        }
        let components = self.components.lock().unwrap();
        let basic = components.0.iter().find(|(id, _)| id == "windows-basic")?;
        let path = basic.1.join("pwsh").join("pwsh.exe");
        path.is_file().then_some(path)
    }

    fn snapshot(&self, workspace_id: &str) -> HostResult<Value> {
        self.workspace_root(workspace_id)?;
        let candidates = self.observations.lock().unwrap().get(workspace_id).cloned().unwrap_or_default();
        let saved = self.state.lock().unwrap().workspaces.get(workspace_id).map(|selection| selection.selected.clone()).unwrap_or_default();
        let mut selected = serde_json::Map::new();
        for (language, path) in saved {
            let candidate = candidates.iter().find(|candidate| candidate.language == language && candidate.path == path).cloned().unwrap_or_else(|| Candidate {
                id: identity(&self.env.target_id, &language, &path),
                language: language.clone(),
                path: path.clone(),
                source: "manual".into(),
                platform: platform().into(),
                version: None,
                prefix: None,
                ready: false,
                capabilities: Vec::new(),
                error: Some(l("请重新检测这个环境，确认它当前的能力。", "Recheck this environment to confirm its current capabilities.")),
            });
            selected.insert(language, serde_json::to_value(candidate)?);
        }
        let mut snapshot = json!({ "targetId": self.env.target_id, "platform": platform(), "workspaceId": workspace_id, "selected": selected, "candidates": candidates });
        if cfg!(windows) {
            let (path, core) = resolve_powershell(self.env.pwsh_path.as_deref(), self.component_powershell().as_deref());
            snapshot["powershell"] = json!({ "path": path, "edition": if core { "core" } else { "desktop" } });
        }
        Ok(snapshot)
    }

    async fn discover(&self, workspace_id: &str) -> HostResult<Value> {
        self.refresh_components()?;
        let candidates = self.candidates(workspace_id)?;
        let target = self.env.target_id.clone();
        let mut probes = Vec::new();
        for (language, path, source) in candidates {
            let target = target.clone();
            probes.push(async move { probe(&target, &language, &path, source).await });
        }
        let results: Vec<Candidate> = futures::stream::iter(probes).buffered(PROBE_CONCURRENCY).collect().await;
        self.observations.lock().unwrap().insert(workspace_id.to_owned(), results);
        self.snapshot(workspace_id)
    }

    fn remember(&self, workspace_id: &str, candidate: Candidate) {
        let mut observations = self.observations.lock().unwrap();
        let list = observations.entry(workspace_id.to_owned()).or_default();
        list.retain(|known| known.id != candidate.id);
        list.push(candidate);
    }

    async fn select(&self, workspace_id: &str, language: &str, path: Option<&str>) -> HostResult<Value> {
        let _guard = self.writes.lock().await;
        let root = self.workspace_root(workspace_id)?;
        let candidate = match path {
            Some(path) => {
                let candidate = probe(&self.env.target_id, language, path, "manual").await;
                if !candidate.ready {
                    return Err(HostError::other(candidate.error.unwrap_or_else(|| l("这个解释器在当前执行环境中不可用。", "This interpreter is not usable in the current execution target."))));
                }
                Some(candidate)
            }
            None => None,
        };
        let before = self.state.lock().unwrap().clone();
        {
            let mut state = self.state.lock().unwrap();
            let previous = state.workspaces.get(workspace_id).cloned().unwrap_or_default();
            let mut selected = previous.selected.clone();
            match &candidate {
                Some(candidate) => selected.insert(language.to_owned(), candidate.path.clone()),
                None => selected.remove(language),
            };
            let python_prefix = if language == "python" { candidate.as_ref().and_then(|candidate| candidate.prefix.clone()) } else { previous.python_prefix };
            state.workspaces.insert(workspace_id.to_owned(), Selection { root, selected, python_prefix });
        }
        let state = self.state.lock().unwrap().clone();
        if let Err(error) = write_json(&self.env.home.join("runtime-environments.json"), &state) {
            *self.state.lock().unwrap() = before;
            return Err(error.into());
        }
        if let Some(candidate) = candidate {
            self.remember(workspace_id, candidate);
        }
        for listener in self.listeners.lock().unwrap().iter() {
            listener(workspace_id);
        }
        self.snapshot(workspace_id)
    }

    /// The `runtime` method.
    pub async fn handle(&self, request: Value) -> HostResult<Value> {
        let op = request["op"].as_str().unwrap_or_default().to_owned();
        let workspace_id = request["workspaceId"].as_str().filter(|id| !id.is_empty()).ok_or_else(|| HostError::invalid("workspaceId is required"))?.to_owned();
        let language = request["language"].as_str().filter(|language| LANGUAGES.contains(language)).map(str::to_owned);
        let project = self.projects.get(&workspace_id).ok_or_else(|| HostError::new("runtime-error", l("所选项目不可用。", "The selected workspace is unavailable.")))?;
        let failure = |error: HostError| HostError::new("runtime-error", error.message);
        self.registry.get_or_register(&project.id, &project.path, &project.title).await.map_err(failure)?;
        if op == "status" {
            return self.snapshot(&workspace_id).map_err(failure);
        }
        self.activity.assert_can_start().map_err(failure)?;
        self.operations.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let result = match op.as_str() {
            "discover" => self.discover(&workspace_id).await,
            "probe" => match (language, request["path"].as_str()) {
                (Some(language), Some(path)) => {
                    let candidate = probe(&self.env.target_id, &language, path, "manual").await;
                    self.remember(&workspace_id, candidate);
                    self.snapshot(&workspace_id)
                }
                _ => Err(HostError::invalid("probe needs a language and a path")),
            },
            "select" => match language {
                Some(language) => self.select(&workspace_id, &language, request["path"].as_str()).await,
                None => Err(HostError::invalid("select needs a language")),
            },
            _ => Err(HostError::invalid(format!("Unknown runtime operation {op}"))),
        };
        self.operations.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        result.map_err(|error| if error.code == "invalid-request" { error } else { failure(error) })
    }

    /// The environment of a process in `cwd`, from the nearest project's selections; nothing in the Host's own
    /// environment changes.
    pub fn resolve(&self, cwd: &Path) -> HostResult<Resolved> {
        self.activity.assert_can_start()?;
        let state = self.state.lock().unwrap();
        let mut matches: Vec<&Selection> = state.workspaces.values().filter(|selection| contains(Path::new(&selection.root), cwd)).collect();
        matches.sort_by_key(|selection| std::cmp::Reverse(selection.root.len()));
        let chosen = matches.first();
        let (components, defaults) = self.components.lock().unwrap().clone();
        // A built-in interpreter counts only once its optional component has been downloaded.
        let mut executables: BTreeMap<String, String> = self.builtin().into_iter().filter(|(_, path)| Path::new(path).exists()).collect();
        executables.extend(defaults);
        if let Some(selection) = chosen {
            executables.extend(selection.selected.clone());
        }
        let mut directories: Vec<PathBuf> = Vec::new();
        for path in executables.values() {
            if let Some(folder) = Path::new(path).parent().map(Path::to_path_buf).filter(|folder| !directories.contains(folder)) {
                directories.push(folder);
            }
        }
        if let (Some(prefix), true) = (chosen.and_then(|selection| selection.python_prefix.as_ref()), cfg!(windows)) {
            directories.push(Path::new(prefix).join("Scripts"));
            directories.push(Path::new(prefix).join("Library").join("bin"));
        }
        for (id, component) in &components {
            if !(id.ends_with("-basic") || id.ends_with("-cpp") || id.ends_with("-development")) {
                continue;
            }
            for directory in ["python", "node", "php", "pwsh", "cpp", "cmake", "ninja", "llvm", "git/cmd", "codelldb/extension/adapter"] {
                let base = directory.split('/').fold(component.clone(), |path, part| path.join(part));
                directories.push(base.join("bin"));
                directories.push(base.clone());
                if cfg!(windows) && directory == "python" {
                    directories.push(base.join("Scripts"));
                }
            }
        }
        let host_path = std::env::var_os("PATH").unwrap_or_default();
        let joined = std::env::join_paths(directories.into_iter().chain(std::env::split_paths(&host_path))).unwrap_or_default();
        let mut environment = BTreeMap::from([("PATH".to_owned(), joined.to_string_lossy().into_owned()), ("PYTHONDONTWRITEBYTECODE".to_owned(), "1".to_owned())]);
        if components.iter().any(|(id, _)| id == "windows-cpp") {
            environment.insert("CMAKE_GENERATOR".into(), "Ninja".into());
        }
        if let (Some(php), true) = (executables.get("php"), cfg!(windows)) {
            if let Some(folder) = Path::new(php).parent() {
                environment.insert("RAINY_PHP_EXTENSION_DIR".into(), folder.join("ext").display().to_string());
            }
        }
        drop(state);
        Ok(Resolved { environment, executables, powershell: self.component_powershell() })
    }

    /// The environment of a project's primary folder.
    pub fn resolve_workspace(&self, workspace_id: &str) -> HostResult<Resolved> {
        let root = self.projects.get(workspace_id).map(|project| project.path).ok_or_else(|| HostError::other(l("所选项目不可用。", "The selected workspace is unavailable.")))?;
        self.resolve(Path::new(&root))
    }
}

use futures::StreamExt as _;
