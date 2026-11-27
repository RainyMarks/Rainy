//! Strata (github.com/Niko1221/Strata), the local inference engine for Qwen3.8-Flash-Next, Qwen3.6 and DeepSeek-V4.1-
//! Flash, as a tool Rainy downloads when asked and removes again: the Strata Terminal of the newest release, the engine
//! for this computer's graphics card, models from Strata's own catalog, and serving through `strata launch`. Rainy keeps
//! it at Strata's newest release: it checks Strata's signed update feed after starting and every few hours, and updates
//! when no model is serving. Everything but the model files is inside `<user data>\tools\strata`; every Strata process
//! runs with the environment of [`env`] and ends with Rainy. A server Rainy did not start is never stopped, and
//! connecting leaves the identity check to the selected Host (`models.connectStrata`).

pub mod catalog;
pub mod control;
pub mod env;
pub mod health;
pub mod mcp;
pub mod process;

mod lifecycle;
mod storage;
mod types;
#[cfg(test)]
mod tests;

pub use self::catalog::{Catalog, Family, Fit, Recommendation, Size};
pub use self::env::{Folders, Source};
pub use self::health::Health;
pub use self::lifecycle::Manager;
pub use self::types::{Choice, Connection, Engine, FoundModel, Installed, Job, JobKind, Phase, Server, ServerHealth, Settings, Status, Update};
