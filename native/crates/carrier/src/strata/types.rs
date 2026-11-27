//! Public status, settings, job and server state types for Strata.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use serde::{Deserialize, Serialize};

use super::catalog::Catalog;
use super::env::Source;
use super::health::Health;

pub(super) const DEFAULT_PORT: u16 = 8080;

/// Rainy's choices about Strata, in `tools\strata\rainy.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    /// Where model downloads go; none: [`super::env::default_data_dir`].
    pub data_dir: Option<String>,
    pub port: u16,
    /// The context to serve with; none: the one Strata picks for this computer.
    pub context: Option<u32>,
    /// Where downloads come from once the person chose it ([`Self::source_chosen`]); before that
    /// [`Status::settings`] holds the one that applies: ModelScope for a Chinese interface, else Hugging Face.
    pub source: Source,
    /// The person picked the source. Rainy up to 4.8 saved Hugging Face with any other change, so a file without this
    /// mark has the automatic one.
    pub source_chosen: bool,
    /// The model to start.
    pub selected: Option<Choice>,
    /// Update to Strata's newest release by itself.
    pub auto_update: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self { data_dir: None, port: DEFAULT_PORT, context: None, source: Source::default(), source_chosen: false, selected: None, auto_update: true }
    }
}

impl Settings {
    /// The source downloads use: the chosen one, else ModelScope for a Chinese interface (Hugging Face is slow or
    /// unreachable in mainland China; the files are the same).
    pub fn effective_source(&self) -> Source {
        match self.source_chosen {
            true => self.source,
            false if crate::locale::chinese() => Source::Modelscope,
            false => Source::Huggingface,
        }
    }
}

/// Model files in a data folder that Strata has no run config for: an earlier download, possibly unfinished, that a
/// download of the same model picks up where they are instead of starting over.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FoundModel {
    /// The catalog id (the model folder's name, in lower case).
    pub id: String,
    /// The data folder holding `models\<id>`.
    pub data_dir: String,
    /// Every GGUF there has Strata's finish mark and nothing is half downloaded.
    pub complete: bool,
    pub bytes: u64,
}

/// A model of the catalog: `--family` and `--model`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Choice {
    pub family: String,
    pub model: String,
}

/// What is installed: the Strata release, its Terminal and the engine for this computer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Installed {
    /// Strata's own version (`0.2.7`), once read.
    pub strata: Option<String>,
    /// The Terminal ("Control") version.
    pub control: String,
    pub engine: Option<Engine>,
    /// The folder of a Strata the person installed themselves, which Rainy uses as it is: never updated or removed by
    /// Rainy. None for the Strata Rainy downloaded.
    #[serde(default)]
    pub external: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Engine {
    pub version: String,
    /// `cuda`, `hip`, `sycl`, `metal`.
    pub backend: String,
    /// `cuda13`, `rdna3`.
    pub variant: String,
}

/// What a check of Strata's update feed found.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Update {
    /// The engine it offers when that is newer or none is installed.
    pub engine: Option<String>,
    /// The Terminal version it offers when newer.
    pub control: Option<String>,
    /// Why no engine is offered for this computer, in Strata's words.
    pub no_engine: Option<String>,
    pub checked: u64,
}

impl Update {
    pub fn available(&self) -> bool {
        self.engine.is_some() || self.control.is_some()
    }
}

/// The one background job: a step key the interface words, and its progress.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Job {
    pub kind: JobKind,
    /// `terminal`, `unpack`, `verify`, `engine`, `engine-verify`, `engine-test`, `control`, `stop`, `remove`, or
    /// `model:<phase>` with Strata's own phase (`download`, `verify`, `pictures`, `draft_download`, `pack`, ...).
    pub step: String,
    pub done: u64,
    pub total: u64,
    /// Strata's own sentence for the step, when it gave one.
    pub detail: Option<String>,
    pub cancellable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum JobKind {
    Install,
    Update,
    Uninstall,
    /// `local`: the model's files are already here and only what is missing beside them is fetched.
    Download { family: String, model: String, local: bool },
    Remove { id: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    #[default]
    Stopped,
    Starting,
    Running,
    Stopping,
    /// A Strata server Rainy did not start answers on the port.
    External,
}

/// The model server on the configured port.
#[derive(Debug, Clone, PartialEq, Serialize, Default)]
pub struct Server {
    pub phase: Phase,
    pub health: Option<ServerHealth>,
    /// What Rainy started or is starting.
    pub choice: Option<Choice>,
    /// Seconds since the start began.
    pub elapsed: u64,
}

/// [`Health`] for the interface.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerHealth {
    pub base_url: String,
    pub model: String,
    pub context_window: u64,
    pub loaded: bool,
    pub authentication_required: bool,
}

impl From<&Health> for ServerHealth {
    fn from(health: &Health) -> Self {
        Self {
            base_url: health.base_url.clone(),
            model: health.model.clone(),
            context_window: health.context_window,
            loaded: health.loaded,
            authentication_required: health.authentication_required,
        }
    }
}

/// What the interface shows.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Status {
    pub installed: Option<Installed>,
    pub job: Option<Job>,
    pub server: Server,
    pub catalog: Option<Catalog>,
    pub catalog_loading: bool,
    pub catalog_error: Option<String>,
    pub update: Option<Update>,
    pub checking: bool,
    pub settings: Settings,
    /// Where downloads go now.
    pub data_dir: String,
    pub data_free_gb: Option<f64>,
    pub error: Option<String>,
    /// What the last job did, when it ended well.
    pub notice: Option<String>,
    /// Files of the Strata Rainy 3.x to 4.2 bundled (0.1.39), in bytes.
    pub legacy_bytes: u64,
    /// Model files on this computer that Strata has not set up (see [`FoundModel`]).
    pub found: Vec<FoundModel>,
}

/// A loaded, credential-free server for the selected Host to verify and save.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Connection {
    #[serde(rename = "baseURL")]
    pub base_url: String,
    pub model: String,
    pub context_window: u64,
}

#[derive(Default)]
pub(super) struct Inner {
    pub settings: Settings,
    pub installed: Option<Installed>,
    pub job: Option<Job>,
    pub cancel: Option<Arc<AtomicBool>>,
    pub server: Server,
    pub owned: bool,
    pub started: Option<Instant>,
    pub start_cancel: Option<Arc<AtomicBool>>,
    pub start_generation: u64,
    pub catalog: Option<Catalog>,
    pub catalog_loading: bool,
    pub catalog_error: Option<String>,
    pub update: Option<Update>,
    pub checking: bool,
    /// An update found while a model served, to install once it stops.
    pub update_waiting: bool,
    pub error: Option<String>,
    pub notice: Option<String>,
    pub data_free_gb: Option<f64>,
    pub legacy_bytes: u64,
    /// The data folder when none is chosen, found once: see `Manager::automatic_data_dir`.
    pub automatic_data: Option<PathBuf>,
    pub found: Vec<FoundModel>,
    pub closed: bool,
}
