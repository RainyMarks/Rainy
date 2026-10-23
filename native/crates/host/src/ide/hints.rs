//! Change hints for the page (3.x `ide/hints.ts`): a file in a folder of an open editor tab changed, or a run produced
//! events. The page reads the actual state with its usual requests, so a lost hint only delays an update until its
//! slow fallback poll.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use notify::{RecursiveMode, Watcher as _};

/// Hints of one kind and project are sent at most this often.
const INTERVAL: Duration = Duration::from_millis(100);
/// Folders watched per project folder; open tabs beyond them rely on the fallback poll.
const MAX_WATCHED: usize = 64;

type Emit = Arc<dyn Fn(&str, &str) + Send + Sync>;

struct WatchSet {
    watcher: notify::RecommendedWatcher,
    folders: Vec<PathBuf>,
    lease: u64,
}

struct Inner {
    emit: Emit,
    lease: Duration,
    /// Throttle state per `kind:workspace`: whether another hint is pending.
    throttled: Mutex<HashMap<String, bool>>,
    sets: Mutex<HashMap<String, WatchSet>>,
    leases: std::sync::atomic::AtomicU64,
}

#[derive(Clone)]
pub struct Hints {
    inner: Arc<Inner>,
}

impl Hints {
    /// `lease`: watchers of a project folder close when the page has not checked its files for this long.
    pub fn new(emit: impl Fn(&str, &str) + Send + Sync + 'static, lease: Duration) -> Self {
        Self { inner: Arc::new(Inner { emit: Arc::new(emit), lease, throttled: Mutex::default(), sets: Mutex::default(), leases: Default::default() }) }
    }

    /// Send a hint now, or once at the end of the current interval when one was sent recently.
    pub fn notify(&self, workspace_id: &str, kind: &str) {
        Self::notify_inner(&self.inner, workspace_id, kind);
    }

    fn notify_inner(inner: &Arc<Inner>, workspace_id: &str, kind: &str) {
        let key = format!("{kind}:{workspace_id}");
        {
            let mut throttled = inner.throttled.lock().unwrap();
            if let Some(pending) = throttled.get_mut(&key) {
                *pending = true;
                return;
            }
            throttled.insert(key.clone(), false);
        }
        (inner.emit)(workspace_id, kind);
        let weak = Arc::downgrade(inner);
        let (workspace_id, kind) = (workspace_id.to_owned(), kind.to_owned());
        tokio::spawn(async move {
            tokio::time::sleep(INTERVAL).await;
            let Some(inner) = weak.upgrade() else { return };
            let pending = inner.throttled.lock().unwrap().remove(&key).unwrap_or(false);
            if pending {
                Self::notify_inner(&inner, &workspace_id, &kind);
            }
        });
    }

    /// Watch exactly these folders of one project folder, replacing its previous set, until the lease ends.
    pub fn watch(&self, workspace_id: &str, root_id: &str, folders: Vec<PathBuf>) {
        let key = format!("{workspace_id}\0{root_id}");
        let mut wanted: Vec<PathBuf> = Vec::new();
        for folder in folders {
            if !wanted.contains(&folder) && wanted.len() < MAX_WATCHED {
                wanted.push(folder);
            }
        }
        let lease = self.inner.leases.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut sets = self.inner.sets.lock().unwrap();
        if !sets.contains_key(&key) {
            let weak: Weak<Inner> = Arc::downgrade(&self.inner);
            let workspace = workspace_id.to_owned();
            let handle = tokio::runtime::Handle::current();
            let watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
                if event.is_ok_and(|event| !event.kind.is_access()) {
                    if let Some(inner) = weak.upgrade() {
                        let workspace = workspace.clone();
                        handle.spawn(async move { Self::notify_inner(&inner, &workspace, "files") });
                    }
                }
            });
            match watcher {
                Ok(watcher) => {
                    sets.insert(key.clone(), WatchSet { watcher, folders: Vec::new(), lease });
                }
                Err(error) => {
                    log::debug!("Folders cannot be watched: {error}");
                    return;
                }
            }
        }
        let set = sets.get_mut(&key).expect("inserted above");
        set.lease = lease;
        for folder in set.folders.clone() {
            if !wanted.contains(&folder) {
                let _ = set.watcher.unwatch(&folder);
                set.folders.retain(|known| *known != folder);
            }
        }
        for folder in wanted {
            // Network shares and some WSL mounts cannot be watched; the fallback poll covers them.
            if !set.folders.contains(&folder) && set.watcher.watch(&folder, RecursiveMode::NonRecursive).is_ok() {
                set.folders.push(folder);
            }
        }
        if set.folders.is_empty() {
            sets.remove(&key);
            return;
        }
        drop(sets);
        let weak = Arc::downgrade(&self.inner);
        let duration = self.inner.lease;
        tokio::spawn(async move {
            tokio::time::sleep(duration).await;
            let Some(inner) = weak.upgrade() else { return };
            let mut sets = inner.sets.lock().unwrap();
            if sets.get(&key).is_some_and(|set| set.lease == lease) {
                sets.remove(&key);
            }
        });
    }

    pub fn close(&self) {
        self.inner.sets.lock().unwrap().clear();
    }
}
