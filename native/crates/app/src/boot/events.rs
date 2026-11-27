//! Host process event bridge (`CarrierEvents`), credential key caching, and `host.log` rotation.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use rainy_carrier::{HostEvents, credential, proxy};
use serde_json::{Value, json};

use super::paths::Paths;

type PreviewSender = async_channel::Sender<(Value, std::sync::mpsc::Sender<Value>)>;
type ExitCallback = Box<dyn Fn(Option<i32>) + Send>;

/// Past this size, `host.log` moves to `host.log.1` when Rainy starts, replacing the one before, as `rainy.log` does;
/// the Host and every MCP server it runs write there.
pub(super) const HOST_LOG_LIMIT: u64 = 4 * 1024 * 1024;

/// Security alert raised when Windows DPAPI credential protection recovers or fails.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialAlert {
    Warning(String),
    Error(String),
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Answers the Host's requests and writes its output to `host.log`.
pub struct CarrierEvents {
    user_data: PathBuf,
    log: Mutex<Option<std::fs::File>>,
    /// The last lines the Host (or `ssh`) wrote, to explain a start that failed.
    recent: Mutex<VecDeque<String>>,
    /// Carries the agent's preview commands to the window, each with the channel for its answer.
    pub preview: Mutex<Option<PreviewSender>>,
    /// Reports DPAPI recovery warnings or fatal protection errors to the UI instead of swallowing them.
    pub credential_alert: Mutex<Option<async_channel::Sender<CredentialAlert>>>,
    key: Mutex<Option<Option<String>>>,
    pub on_exit: Mutex<Option<ExitCallback>>,
}

impl CarrierEvents {
    pub fn new(paths: &Paths) -> Arc<Self> {
        rotate_host_log(&paths.log_path());
        let log = std::fs::OpenOptions::new().create(true).append(true).open(paths.log_path()).ok();
        Arc::new(Self {
            user_data: paths.user_data.clone(),
            log: Mutex::new(log),
            recent: Mutex::default(),
            preview: Mutex::new(None),
            credential_alert: Mutex::new(None),
            key: Mutex::new(None),
            on_exit: Mutex::new(None),
        })
    }

    pub fn clear_recent(&self) {
        lock(&self.recent).clear();
    }

    pub fn recent(&self) -> String {
        Vec::from(lock(&self.recent).clone()).join("\n")
    }

    pub fn write_log(&self, line: &str) {
        use std::io::Write as _;
        if let Some(file) = lock(&self.log).as_mut() {
            let _ = writeln!(file, "{line}");
        }
    }
}

pub(super) fn rotate_host_log(path: &Path) {
    if std::fs::metadata(path).is_ok_and(|metadata| metadata.len() >= HOST_LOG_LIMIT) {
        let mut older = path.as_os_str().to_owned();
        older.push(".1");
        if let Err(error) = std::fs::rename(path, &older) {
            log::warn!("{} was not moved aside: {error}", path.display());
        }
    }
}

impl HostEvents for CarrierEvents {
    fn credential_key(&self) -> Option<String> {
        // Read or created once per run; every Host started in this run encrypts API keys with it.
        let mut cached = lock(&self.key);
        if let Some(key) = cached.as_ref() {
            return key.clone();
        }
        let key = match credential::load_credential_key(&self.user_data, &credential::Dpapi, &|line| self.write_log(line)) {
            Ok(loaded) => {
                if !loaded.warnings.is_empty() {
                    let message = loaded.warnings.iter().map(|w| w.localized_message()).collect::<Vec<_>>().join("\n");
                    self.write_log(&format!("DPAPI credential warning: {message}"));
                    if let Some(tx) = lock(&self.credential_alert).as_ref() {
                        let _ = tx.try_send(CredentialAlert::Warning(message));
                    }
                }
                Some(loaded.key)
            }
            Err(error) => {
                let message = error.localized_message();
                self.write_log(&format!("DPAPI credential protection failed; API keys entered in this run stay unencrypted until a later run: {error:#}"));
                if let Some(tx) = lock(&self.credential_alert).as_ref() {
                    let _ = tx.try_send(CredentialAlert::Error(message));
                }
                None
            }
        };
        *cached = Some(key.clone());
        key
    }

    fn system_proxy(&self, url: &str) -> Option<String> {
        proxy::system_proxy(url)
    }

    fn preview(&self, command: Value) -> Value {
        let unavailable = || json!({ "error": "The preview window is unavailable." });
        let Some(sender) = lock(&self.preview).clone() else { return unavailable() };
        let (reply, answer) = std::sync::mpsc::channel();
        if sender.send_blocking((command, reply)).is_err() {
            return unavailable();
        }
        // A load waits up to 20 s and a settle a few more; past this the window is not answering.
        answer.recv_timeout(std::time::Duration::from_secs(90)).unwrap_or_else(|_| unavailable())
    }

    fn diagnostic(&self, line: &str) {
        self.write_log(line);
        let mut recent = lock(&self.recent);
        recent.push_back(line.to_owned());
        if recent.len() > 40 {
            recent.pop_front();
        }
    }

    fn exited(&self, code: Option<i32>) {
        self.write_log(&format!("Host exited: {code:?}"));
        if let Some(callback) = lock(&self.on_exit).as_ref() {
            callback(code);
        }
    }
}
