//! Files and folders handed to Rainy (3.x `launchQueue`, `deliverLaunch` and `openLaunched`): from its own command line
//! and from later launches, which the single-instance pipe hands to this process. They wait until the Host is ready
//! and the IDE has shown its last project again, then open there.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use gpui_kit::component::WindowExt as _;
use gpui_kit::*;
use rainy_carrier::instance::{self, Claim, Launch};
use rainy_carrier::launch::{self as paths, LaunchKind};
use rainy_carrier::{ExecutionTarget, wsl};

use crate::store::ide::{Ide, IdeEvent, Launched};
use crate::store::{Phase, Store};
use crate::ui::{tr, trf};

/// The paths each later launch handed over.
pub type Forwarded = async_channel::Receiver<Vec<PathBuf>>;

/// Paths waiting for the workbench.
struct Launches {
    queue: Vec<PathBuf>,
    delivering: bool,
}

impl Global for Launches {}

/// Become the Rainy of this profile, or hand this launch to the one running and exit. The receiver brings the later
/// launches; `None` when this process runs without the lock.
pub fn claim(user_data: &Path, argv: &[OsString]) -> Option<Forwarded> {
    let launch = Launch { argv: argv.to_vec(), cwd: std::env::current_dir().unwrap_or_default() };
    match instance::claim(user_data, &launch, paths::wait_pid(argv)) {
        Ok(Claim::First(lock)) => {
            let (sender, receiver) = async_channel::unbounded();
            lock.serve(move |launch| {
                let _ = sender.send_blocking(paths::launch_paths(&launch.argv, &launch.cwd));
            });
            Some(receiver)
        }
        Ok(Claim::Forwarded(pid)) => {
            log::info!("The launch went to the Rainy already running ({pid})");
            std::process::exit(0);
        }
        Ok(Claim::Alone) => None,
        Err(error) => {
            log::error!("Rainy is running for this profile but did not take the launch: {error:#}");
            std::process::exit(1);
        }
    }
}

/// Queue this process's own paths; none after a restart, which leaves them out.
pub fn init(argv: &[OsString], cx: &mut App) {
    let queue = paths::launch_paths(argv, &std::env::current_dir().unwrap_or_default());
    cx.set_global(Launches { queue, delivering: false });
}

/// Bring the window to the front for each later launch and queue its paths; open what is queued once the IDE has
/// restored its last project.
pub fn listen(forwarded: Option<Forwarded>, window: AnyWindowHandle, cx: &mut App) {
    let ide = Ide::global(cx);
    cx.subscribe(&ide, move |_, event: &IdeEvent, cx| {
        if *event == IdeEvent::Restored {
            let _ = window.update(cx, |_, window, cx| deliver(window, cx));
        }
    })
    .detach();
    let Some(forwarded) = forwarded else { return };
    cx.spawn(async move |cx: &mut AsyncApp| {
        while let Ok(paths) = forwarded.recv().await {
            let _ = window.update(cx, |_, window, cx| {
                // Restored if minimized, shown and focused, also while the startup screen is up.
                window.activate_window();
                cx.global_mut::<Launches>().queue.extend(paths);
                deliver(window, cx);
            });
        }
    })
    .detach();
}

/// Open the queued paths, one batch at a time, once the workbench can.
fn deliver(window: &mut Window, cx: &mut App) {
    let store = Store::global(cx);
    let ready = matches!(store.read(cx).phase, Phase::Ready) && Ide::global(cx).read(cx).restored;
    let launches = cx.global_mut::<Launches>();
    if !ready || launches.delivering || launches.queue.is_empty() {
        return;
    }
    launches.delivering = true;
    let queued = std::mem::take(&mut launches.queue);
    let target = store.read(cx).target.clone().unwrap_or_else(ExecutionTarget::windows);
    window
        .spawn(cx, async move |cx: &mut AsyncWindowContext| {
            if let Err(error) = open(queued, target, cx).await {
                log::warn!("Opening the launched paths failed: {error:#}");
                let detail = format!("{error:#}");
                let _ = cx.update(|window, cx| alert(window, cx, "native.launchFailed", detail.into()));
            }
            let _ = cx.update(|window, cx| {
                cx.global_mut::<Launches>().delivering = false;
                deliver(window, cx);
            });
        })
        .detach();
}

/// One batch: refused on an SSH server; missing paths are reported; on WSL the paths are mapped with one `wslpath` call.
async fn open(queued: Vec<PathBuf>, target: ExecutionTarget, cx: &mut AsyncWindowContext) -> anyhow::Result<()> {
    if matches!(target, ExecutionTarget::Ssh { .. }) {
        let listed = list(&queued);
        cx.update(|window, cx| {
            let detail = trf(cx, "native.launchSshDetail", &[("target", &target.label()), ("paths", &listed)]);
            alert(window, cx, "native.launchSsh", detail);
        })?;
        return Ok(());
    }
    let (entries, missing) = cx.background_spawn(async move { paths::inspect(&queued) }).await;
    if !missing.is_empty() {
        let listed = list(&missing);
        cx.update(|window, cx| alert(window, cx, "native.launchMissing", listed.into()))?;
    }
    if entries.is_empty() {
        return Ok(());
    }
    let host_paths = match &target {
        ExecutionTarget::Wsl { distro, .. } => {
            let (distro, windows) = (distro.clone(), entries.iter().map(|entry| entry.path.clone()).collect::<Vec<_>>());
            cx.background_spawn(async move { wsl::linux_paths(&distro, &windows.iter().map(PathBuf::as_path).collect::<Vec<_>>()) }).await?
        }
        _ => entries.iter().map(|entry| entry.path.to_string_lossy().into_owned()).collect(),
    };
    let launched: Vec<Launched> =
        entries.iter().zip(host_paths).map(|(entry, path)| Launched { path, directory: entry.kind == LaunchKind::Directory }).collect();
    let opened = cx.update(|window, cx| Ide::global(cx).update(cx, |ide, cx| ide.open_launched(launched, window, cx)))?;
    opened.await;
    Ok(())
}

fn list(paths: &[PathBuf]) -> String {
    paths.iter().map(|path| path.display().to_string()).collect::<Vec<_>>().join("\n")
}

/// A message with the title `key` and the detail below it.
fn alert(window: &mut Window, cx: &mut App, key: &str, detail: SharedString) {
    let title = tr(cx, key);
    window.open_alert_dialog(cx, move |alert, _, cx| alert.title(title.clone()).description(detail.clone()).ok_text(tr(cx, "native.ok")));
}
