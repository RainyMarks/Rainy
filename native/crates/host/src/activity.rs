//! Whether anything is running, for target switches and update installs the carrier asks about (3.x `activity.ts`).

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use crate::error::HostError;
use crate::locale::l;

type Probe = Arc<dyn Fn() -> bool + Send + Sync>;

/// Probes of running work, and the gate the carrier closes before it replaces this Host.
#[derive(Default)]
pub struct Activity {
    probes: Mutex<Vec<(usize, Probe)>>,
    next: AtomicUsize,
    frozen: AtomicBool,
}

impl Activity {
    /// Add a probe that reports whether its owner has running work; returns its id for [`Activity::remove`].
    pub fn add(&self, probe: impl Fn() -> bool + Send + Sync + 'static) -> usize {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        self.probes.lock().unwrap().push((id, Arc::new(probe)));
        id
    }

    pub fn remove(&self, id: usize) {
        self.probes.lock().unwrap().retain(|(known, _)| *known != id);
    }

    pub fn active(&self) -> bool {
        let probes: Vec<Probe> = self.probes.lock().unwrap().iter().map(|(_, probe)| probe.clone()).collect();
        probes.iter().any(|probe| probe())
    }

    /// The carrier's `inspect-activity`: `freeze` blocks new work when idle, `resume` lifts the block.
    pub fn inspect(&self, mode: &str) -> bool {
        let active = self.active();
        match mode {
            "resume" => self.frozen.store(false, Ordering::Relaxed),
            "freeze" if !active => self.frozen.store(true, Ordering::Relaxed),
            _ => {}
        }
        active
    }

    /// Refuse new work while the carrier is replacing this Host.
    pub fn assert_can_start(&self) -> Result<(), HostError> {
        if self.frozen.load(Ordering::Relaxed) {
            return Err(HostError::other(l("执行环境正在切换，请等待应用重新打开。", "The execution environment is switching. Wait for Rainy to open again.")));
        }
        Ok(())
    }
}
