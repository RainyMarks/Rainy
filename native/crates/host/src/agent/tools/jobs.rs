//! Commands that keep running after their tool call returns: development servers, watchers, builds (3.x `jobs.ts`).
//! One registry per chat; its jobs are stopped when the chat closes or the Host stops.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use tokio::io::AsyncReadExt as _;
use tokio::sync::watch;

/// Jobs one chat may have running at once; a new one past this is refused rather than queued.
const MAX_JOBS: usize = 8;
/// Output kept per job until the model reads it; older output is dropped and the drop reported.
const BUFFER_BYTES: usize = 256 * 1024;

/// What the model and the page see of a background command.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Job {
    pub id: String,
    pub command: String,
    pub description: String,
    pub cwd: String,
    pub started_at: u64,
    /// `running` or `exited`.
    pub state: &'static str,
    pub exit_code: Option<i32>,
    pub signal: Option<String>,
    /// Unread output bytes.
    pub pending: usize,
}

struct Entry {
    job: Job,
    pid: Option<u32>,
    output: Vec<u8>,
    dropped: usize,
    exited: watch::Receiver<bool>,
}

/// What a command to start in the background is.
pub struct Spec {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub env: BTreeMap<OsString, OsString>,
    pub command: String,
    pub description: String,
}

/// Background commands of one chat.
pub struct Jobs {
    entries: Mutex<Vec<Arc<Mutex<Entry>>>>,
    next: Mutex<u32>,
    on_change: Box<dyn Fn() + Send + Sync>,
}

impl Jobs {
    /// `on_change` runs when a job starts, exits or is stopped, so the page can follow.
    pub fn new(on_change: impl Fn() + Send + Sync + 'static) -> Arc<Self> {
        Arc::new(Self { entries: Mutex::default(), next: Mutex::new(1), on_change: Box::new(on_change) })
    }

    fn find(&self, id: &str) -> Result<Arc<Mutex<Entry>>, String> {
        let entries = self.entries.lock().unwrap();
        entries.iter().find(|entry| entry.lock().unwrap().job.id == id.trim()).cloned().ok_or_else(|| {
            let known: Vec<String> = entries.iter().map(|entry| entry.lock().unwrap().job.id.clone()).collect();
            format!("unknown background command \"{id}\"{}", if known.is_empty() { "; none is running".to_owned() } else { format!("; running or finished: {}", known.join(", ")) })
        })
    }

    /// Start a command that keeps running.
    pub fn start(self: &Arc<Self>, spec: Spec) -> Result<Job, String> {
        let live = self.entries.lock().unwrap().iter().filter(|entry| entry.lock().unwrap().job.state == "running").count();
        if live >= MAX_JOBS {
            return Err(format!("too many background commands are running ({live}); stop one with stop_command before starting another"));
        }
        let id = {
            let mut next = self.next.lock().unwrap();
            *next += 1;
            format!("cmd-{}", *next - 1)
        };
        let (exited_tx, exited) = watch::channel(false);
        let job = Job {
            id: id.clone(),
            command: spec.command,
            description: spec.description,
            cwd: spec.cwd.to_string_lossy().into_owned(),
            started_at: crate::text::now_ms(),
            state: "running",
            exit_code: None,
            signal: None,
            pending: 0,
        };
        let entry = Arc::new(Mutex::new(Entry { job: job.clone(), pid: None, output: Vec::new(), dropped: 0, exited }));
        self.entries.lock().unwrap().push(entry.clone());
        match crate::process::spawn(&spec.program, &spec.args, &spec.cwd, &spec.env, false) {
            Ok(mut child) => {
                entry.lock().unwrap().pid = child.id();
                let readers: Vec<_> = [child.stdout.take().map(|out| Box::new(out) as Box<dyn tokio::io::AsyncRead + Unpin + Send>), child.stderr.take().map(|err| Box::new(err) as _)]
                    .into_iter()
                    .flatten()
                    .map(|mut stream| {
                        let entry = entry.clone();
                        tokio::spawn(async move {
                            let mut buffer = [0u8; 8192];
                            while let Ok(read) = stream.read(&mut buffer).await {
                                if read == 0 {
                                    break;
                                }
                                push(&mut entry.lock().unwrap(), &buffer[..read]);
                            }
                        })
                    })
                    .collect();
                let (jobs, entry) = (self.clone(), entry.clone());
                tokio::spawn(async move {
                    let status = child.wait().await;
                    for reader in readers {
                        let _ = reader.await;
                    }
                    {
                        let mut entry = entry.lock().unwrap();
                        entry.job.state = "exited";
                        if let Ok(status) = status {
                            let (code, signal) = crate::process::outcome(&status);
                            entry.job.exit_code = code;
                            entry.job.signal = signal;
                        }
                    }
                    let _ = exited_tx.send(true);
                    (jobs.on_change)();
                });
            }
            Err(error) => {
                let mut held = entry.lock().unwrap();
                held.job.state = "exited";
                push(&mut held, format!("\n[could not start: {error}]\n").as_bytes());
                let _ = exited_tx.send(true);
            }
        }
        (self.on_change)();
        Ok(job)
    }

    /// The output a job produced since the last read; a finished job is forgotten once read.
    pub fn read(&self, id: &str) -> Result<(Job, String), String> {
        let entry = self.find(id)?;
        let (job, text) = {
            let mut held = entry.lock().unwrap();
            let output = std::mem::take(&mut held.output);
            held.job.pending = 0;
            let dropped = std::mem::take(&mut held.dropped);
            let note = if dropped > 0 { format!("[{dropped} earlier bytes dropped; the command produced more output than is kept]\n") } else { String::new() };
            (held.job.clone(), note + &crate::process::console_text(&output))
        };
        if job.state == "exited" {
            self.entries.lock().unwrap().retain(|other| !Arc::ptr_eq(other, &entry));
        }
        Ok((job, text))
    }

    /// Drop a finished job without reading it; its unread output is lost.
    pub fn forget(&self, id: &str) -> Result<(), String> {
        let entry = self.find(id)?;
        let job = entry.lock().unwrap().job.clone();
        if job.state == "running" {
            return Err(format!("background command \"{}\" is still running; stop it first", job.id));
        }
        self.entries.lock().unwrap().retain(|other| !Arc::ptr_eq(other, &entry));
        (self.on_change)();
        Ok(())
    }

    /// Every job, newest last.
    pub fn list(&self) -> Vec<Job> {
        self.entries.lock().unwrap().iter().map(|entry| entry.lock().unwrap().job.clone()).collect()
    }

    /// Stop a running job and wait for it to go; its unread output stays readable.
    pub async fn stop(&self, id: &str) -> Result<Job, String> {
        let entry = self.find(id)?;
        stop_entry(&entry).await;
        let job = {
            let mut held = entry.lock().unwrap();
            held.job.state = "exited";
            held.job.clone()
        };
        (self.on_change)();
        Ok(job)
    }

    /// Stop every job.
    pub async fn close(&self) {
        let entries: Vec<_> = std::mem::take(&mut *self.entries.lock().unwrap());
        futures::future::join_all(entries.iter().map(stop_entry)).await;
        (self.on_change)();
    }
}

fn push(entry: &mut Entry, chunk: &[u8]) {
    entry.output.extend_from_slice(chunk);
    if entry.output.len() > BUFFER_BYTES {
        let excess = entry.output.len() - BUFFER_BYTES;
        entry.output.drain(..excess);
        entry.dropped += excess;
    }
    entry.job.pending = entry.output.len();
}

async fn stop_entry(entry: &Arc<Mutex<Entry>>) {
    let (pid, mut exited) = {
        let held = entry.lock().unwrap();
        (held.pid.filter(|_| held.job.state == "running"), held.exited.clone())
    };
    if let Some(pid) = pid {
        crate::process::kill_pid_tree(pid).await;
        let _ = tokio::time::timeout(Duration::from_secs(5), exited.wait_for(|done| *done)).await;
    }
}
