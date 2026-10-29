//! Whether anything is running, for target switches and update installs the carrier asks about (3.x `activity.ts`).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::error::HostError;
use crate::locale::l;

type Probe = Arc<dyn Fn() -> bool + Send + Sync>;

/// Probes of running work, and the gate the carrier closes before it replaces this Host.
#[derive(Default)]
pub struct Activity {
    probes: Mutex<Vec<Probe>>,
    frozen: AtomicBool,
}

impl Activity {
    /// Add a probe that reports whether its owner has running work, for the life of the Host.
    pub fn add(&self, probe: impl Fn() -> bool + Send + Sync + 'static) {
        self.probes.lock().unwrap().push(Arc::new(probe));
    }

    pub fn active(&self) -> bool {
        // The probes lock their owners' state; they run without this lock held.
        let probes: Vec<Probe> = self.probes.lock().unwrap().clone();
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
