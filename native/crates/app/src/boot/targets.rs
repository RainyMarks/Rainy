//! Execution target preparation and Host launch for Windows, WSL and SSH.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::{Context as _, Result, anyhow, bail};
use rainy_carrier::askpass::Askpass;
use rainy_carrier::modules::{ModuleError, Modules};
use rainy_carrier::ssh::{self, InstallProgress, ReconnectOutcome, RuntimeProbe, ServerRuntime};
use rainy_carrier::wsl::{self, Installed, RuntimeFiles};
use rainy_carrier::{ExecutionTarget, HostCommand, HostProcess, HostStopped, targets};
use serde_json::json;

use super::Step;
use super::events::CarrierEvents;
use super::messages::{due, megabytes, module_message};
use super::paths::{Paths, saved_locale};
use crate::i18n::{self, Locale};

/// The module holding the base of the Linux runtime.
const LINUX_RUNTIME: &str = "linux-runtime";
const LINUX_APP: &str = "linux-app";

type ProbeRunner<'a> = dyn Fn(&[(String, String)]) -> Result<std::result::Result<RuntimeProbe, ssh::RunOutput>> + 'a;
type InstallRunner<'a> = dyn Fn(&[(String, String)]) -> Result<std::result::Result<ServerRuntime, ssh::RunOutput>> + 'a;

/// What the user chose after the Linux runtime could not be downloaded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeChoice {
    Retry,
    /// Start the Windows Host instead, and keep using Windows.
    Windows,
    Quit,
}

/// Asks the window what to do after the Linux runtime could not be downloaded, showing why.
pub type AskRuntime<'a> = dyn Fn(String) -> RuntimeChoice + Sync + 'a;

/// Work a Host's start leaves for the background maintenance: removing the Linux runtimes earlier versions installed
/// for its target (`--prune`). Only an installed release build has any.
pub type Prune = Box<dyn FnOnce() + Send>;

/// A started Host.
pub struct Started {
    pub target: ExecutionTarget,
    pub host: HostProcess,
    pub prune: Option<Prune>,
}

/// The native Windows Host (`WindowsHostTransport`).
pub fn windows_host(paths: &Paths) -> Result<HostCommand> {
    let root = paths.resources.join("windows-host");
    let host = root.join("rainy-host.exe");
    if !host.is_file() {
        bail!(i18n::text(saved_locale(&paths.user_data), "native.windowsHostMissing"));
    }
    let data = |name: &str| paths.user_data.join(name).display().to_string();
    let php = paths.user_data.join("modules").join("php").join("php.exe");
    let ide_tools = std::env::join_paths(["ide-python", "ide-web"].map(|id| paths.user_data.join("modules").join(id)))
        .map(|joined| joined.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut env = shared_env(&ExecutionTarget::windows());
    env.extend(
        [
            ("RAINY_CARRIER_STATE_ROOT", data("carrier-state")),
            ("RAINY_HOME", data("native-home")),
            ("RAINY_TOOLCHAIN_ROOT", data("env")),
            ("RAINY_BUILTIN_PHP", php.display().to_string()),
            ("RAINY_IDE_TOOLS", ide_tools),
            ("RAINY_CARRIER_PROXY", "1".to_owned()),
        ]
        .map(|(name, value)| (name.to_owned(), value)),
    );
    Ok(HostCommand {
        program: host,
        args: Vec::new(),
        env: env.into_iter().map(|(name, value)| (name.into(), value.into())).collect(),
        cwd: Some(paths.install_root.clone()),
        scrub_environment: true,
        wait_for_loopback: false,
        expected_port: None,
    })
}

/// Variables every Host gets.
fn shared_env(target: &ExecutionTarget) -> Vec<(String, String)> {
    let mut env = vec![
        ("RAINY_EXECUTION_TARGET_ID".to_owned(), target.id().to_owned()),
        ("RAINY_EXECUTION_TARGET_LABEL".to_owned(), target.label().to_owned()),
        ("RAINY_CARRIER_CREDENTIALS".to_owned(), "1".to_owned()),
    ];
    if !matches!(target, ExecutionTarget::Ssh { .. }) {
        env.push(("RAINY_CARRIER_PREVIEW".to_owned(), "1".to_owned()));
    }
    if std::env::var("RAINY_CONFIGURE_DEEPSEEK").as_deref() == Ok("1") {
        env.push(("RAINY_CONFIGURE_DEEPSEEK".to_owned(), "1".to_owned()));
    }
    env
}

/// This version's Linux runtime files.
pub fn runtime_files(paths: &Paths, modules: &Modules) -> RuntimeFiles {
    let staged = paths.resources.join("linux-app.tar.gz");
    let app = if !paths.packaged() && staged.is_file() { staged.clone() } else { modules.archive(LINUX_APP).unwrap_or(staged) };
    RuntimeFiles {
        metadata: paths.resources.join("linux-runtime.json"),
        app,
        installer: paths.resources.join("install-runtime.py"),
        base: development_base(paths).or_else(|| modules.archive(LINUX_RUNTIME)),
    }
}

/// The base archive a development checkout staged (3.x `linuxRuntimeArchive` when not packaged).
fn development_base(paths: &Paths) -> Option<PathBuf> {
    let base = paths.resources.join("linux-runtime.tar.xz");
    (!paths.packaged() && base.is_file()).then_some(base)
}

/// Download a part of this version's Linux runtime (the base or the application layer) with its progress.
fn download_linux_runtime(modules: &Modules, id: &str, locale: Locale, ask: &AskRuntime<'_>, progress: &(dyn Fn(Step) + Sync)) -> Result<bool> {
    let shown = Mutex::new(None);
    let report = |done: u64, total: u64| {
        if due(&shown, done >= total) {
            let text = i18n::format(locale, "native.linuxDownloading", &[("done", &megabytes(done)), ("total", &megabytes(total))]);
            progress(Step { text, fraction: (total > 0).then(|| done as f32 / total as f32) });
        }
    };
    loop {
        match modules.install(id, &report) {
            Ok(()) => return Ok(true),
            Err(error) => {
                log::warn!("The Linux runtime was not downloaded: {error}");
                if error.cancelled() || error == ModuleError::Closing {
                    bail!(i18n::text(locale, "native.linuxNotDownloaded"));
                }
                match ask(module_message(locale, &error)) {
                    RuntimeChoice::Retry => {}
                    RuntimeChoice::Windows => return Ok(false),
                    RuntimeChoice::Quit => bail!(i18n::text(locale, "native.linuxNotDownloaded")),
                }
            }
        }
    }
}

/// The unpacked runtime is all a target needs; only the next version downloads an archive again.
fn remove_linux_runtime(paths: &Paths, modules: &Modules, events: &CarrierEvents, ids: &[&str]) {
    for id in ids {
        if paths.packaged()
            && let Err(error) = modules.remove(id)
        {
            events.write_log(&format!("The Linux runtime archive {id} was not removed: {error}"));
        }
    }
}

/// Install this version's runtime in the distribution and describe its Host (`prepareWsl`).
pub fn wsl_host(
    paths: &Paths,
    target: &ExecutionTarget,
    modules: &Modules,
    events: &CarrierEvents,
    ask: &AskRuntime<'_>,
    progress: &(dyn Fn(Step) + Sync),
) -> Result<Option<(HostCommand, Option<Prune>)>> {
    let ExecutionTarget::Wsl { distro, .. } = target else { bail!("Not a WSL target") };
    let locale = saved_locale(&paths.user_data);
    let starting = || progress(i18n::format(locale, "native.startingWsl", &[("distro", distro)]).into());
    starting();
    let mut files = runtime_files(paths, modules);
    let carrier_state = paths.user_data.join("carrier-state");
    std::fs::create_dir_all(&carrier_state)?;
    let mut installed = wsl::install_runtime(distro, &files)?;
    for _ in 0..2 {
        let id = match installed {
            Installed::NeedsApp => LINUX_APP,
            Installed::NeedsArchive => LINUX_RUNTIME,
            Installed::Ready { .. } => break,
        };
        if !download_linux_runtime(modules, id, locale, ask, progress)? {
            return Ok(None);
        }
        starting();
        files = runtime_files(paths, modules);
        installed = wsl::install_runtime(distro, &files)?;
    }
    let Installed::Ready { host } = installed else { bail!(i18n::text(locale, "native.linuxMismatch")) };
    remove_linux_runtime(paths, modules, events, &[LINUX_RUNTIME, LINUX_APP]);
    let state = wsl::linux_paths(distro, &[&carrier_state])?
        .into_iter()
        .next()
        .context("WSL did not translate carrier-state path")?;
    let mut env = shared_env(target);
    env.push(("RAINY_CARRIER_STATE_ROOT".to_owned(), state));
    let prune = paths.packaged().then(|| {
        let (distro, files) = (distro.clone(), files.clone());
        Box::new(move || match wsl::prune_runtimes(&distro, &files) {
            Ok(removed) if !removed.is_empty() => log::info!("Removed {} WSL runtimes that earlier versions unpacked", removed.len()),
            Ok(_) => {}
            Err(error) => log::warn!("Earlier WSL runtimes were not removed: {error:#}"),
        }) as Prune
    });
    Ok(Some((wsl::host_command(distro, &host, &env), prune)))
}

/// Read the saved per-host research profile for an SSH target, if configured.
pub fn ssh_profile(paths: &Paths, target: &ExecutionTarget) -> Option<targets::SshHostProfile> {
    if !matches!(target, ExecutionTarget::Ssh { .. }) {
        return None;
    }
    let preferences = targets::read_preferences(&paths.desktop_settings()).unwrap_or_default();
    let profile = targets::ssh_host_profile(&preferences, target.id());
    (!profile.is_empty()).then_some(profile)
}

/// Start the Host for a specific `target` (Windows, WSL, or SSH).
pub fn start_target(
    paths: &Paths,
    target: ExecutionTarget,
    events: Arc<CarrierEvents>,
    askpass: Option<&Arc<Askpass>>,
    modules: &Modules,
    ask: &AskRuntime<'_>,
    progress: &(dyn Fn(Step) + Sync),
) -> Result<Started> {
    let prepared = match &target {
        ExecutionTarget::Windows { .. } => Some((windows_host(paths)?, None)),
        ExecutionTarget::Wsl { .. } => wsl_host(paths, &target, modules, &events, ask, progress)?,
        ExecutionTarget::Ssh { .. } => {
            let askpass = askpass.with_context(|| i18n::text(saved_locale(&paths.user_data), "native.sshAskpassFailed"))?;
            if let Some((host, prune)) = ssh_host(paths, &target, events.clone(), askpass, modules, ask, progress)? {
                return Ok(Started { target, host, prune });
            }
            None
        }
    };
    let Some((command, prune)) = prepared else {
        progress(i18n::text(saved_locale(&paths.user_data), "native.starting").to_owned().into());
        let host = start_host(&windows_host(paths)?, &events)?;
        let fallback = ExecutionTarget::windows();
        if let Err(error) = targets::save_target(&paths.desktop_settings(), &fallback) {
            log::warn!("The execution target was not saved: {error:#}");
        }
        return Ok(Started { target: fallback, host, prune: None });
    };
    Ok(Started { host: start_host(&command, &events)?, target, prune })
}

/// The target this launch uses, and its Host once it is ready.
pub fn start(
    paths: &Paths,
    events: Arc<CarrierEvents>,
    askpass: Option<&Arc<Askpass>>,
    modules: &Modules,
    ask: &AskRuntime<'_>,
    progress: &(dyn Fn(Step) + Sync),
) -> Result<Started> {
    let preferences = targets::recover_preferences(&paths.desktop_settings())?;
    let saved = targets::saved_target(&preferences).unwrap_or_else(|_| ExecutionTarget::windows());
    start_target(paths, saved, events, askpass, modules, ask, progress)
}

/// Start a Windows or WSL Host; one that stops before it is ready is explained with the last lines it wrote.
fn start_host(command: &HostCommand, events: &Arc<CarrierEvents>) -> Result<HostProcess> {
    events.clear_recent();
    HostProcess::start(command, events.clone()).map_err(|error| {
        if error.downcast_ref::<HostStopped>().is_none() {
            return error;
        }
        let recent = events.recent();
        let said: Vec<&str> = recent.lines().map(str::trim).filter(|line| !line.is_empty()).collect();
        match said.len() {
            0 => error,
            count => anyhow!("{error}\n{}", said[count.saturating_sub(5)..].join("\n")),
        }
    })
}

/// Install this version's runtime on the server (when needed) and start the Host over SSH.
fn ssh_host(
    paths: &Paths,
    target: &ExecutionTarget,
    events: Arc<CarrierEvents>,
    askpass: &Arc<Askpass>,
    modules: &Modules,
    ask: &AskRuntime<'_>,
    progress: &(dyn Fn(Step) + Sync),
) -> Result<Option<(HostProcess, Option<Prune>)>> {
    let locale = saved_locale(&paths.user_data);
    let destination = ssh::destination_of(target)?;
    let profile = ssh_profile(paths, target);
    let name = ssh::name(&destination);
    progress(i18n::format(locale, "native.sshConnecting", &[("target", &target.label())]).into());
    let mut files = runtime_files(paths, modules);
    let log = |line: &str| events.write_log(line);
    let run_ssh = |run: &ProbeRunner<'_>| {
        let session = askpass.session(&ssh::askpass_scope(&destination));
        match run(session.env())? {
            Ok(answer) => Ok(answer),
            Err(output) => {
                if ssh::login_refused(&output.stderr) {
                    session.failed();
                }
                Err(ssh::failure(&destination, output.code, &output.stderr))
            }
        }
    };
    let installer = |run: &InstallRunner<'_>| {
        let session = askpass.session(&ssh::askpass_scope(&destination));
        match run(session.env())? {
            Ok(answer) => Ok(answer),
            Err(output) => {
                if ssh::login_refused(&output.stderr) {
                    session.failed();
                }
                Err(ssh::failure(&destination, output.code, &output.stderr))
            }
        }
    };

    let probe = run_ssh(&|env| ssh::probe_runtime_with_profile(&destination, &files, env, paths.packaged(), profile.as_ref(), &log))?;
    let (host, pruned_during_probe) = match probe {
        RuntimeProbe::Ready { host } => (host, true),
        RuntimeProbe::Unsupported(system) => return Err(ssh::unsupported_error(&destination, &system)),
        RuntimeProbe::NeedsInstall { needs_app, .. } => {
            if needs_app && !files.app.is_file() {
                if !download_linux_runtime(modules, LINUX_APP, locale, ask, progress)? {
                    return Ok(None);
                }
                files = runtime_files(paths, modules);
                progress(i18n::format(locale, "native.sshConnecting", &[("target", &target.label())]).into());
            }
            let pieces = if std::env::var("RAINY_SSH_NO_FETCH").as_deref() == Ok("1") { json!([]) } else { modules.pieces(LINUX_RUNTIME) };
            let fraction = |done: u64, total: u64| (total > 0).then(|| done as f32 / total as f32);
            let report = |step: InstallProgress| {
                progress(match step {
                    InstallProgress::Downloading { done, total } => Step {
                        text: i18n::format(locale, "native.sshDownloading", &[("done", &megabytes(done)), ("total", &megabytes(total))]),
                        fraction: fraction(done, total),
                    },
                    InstallProgress::Uploading { done, total } => Step {
                        text: i18n::format(locale, "native.sshUploading", &[("name", &name), ("done", &megabytes(done)), ("total", &megabytes(total))]),
                        fraction: fraction(done, total),
                    },
                })
            };
            let mut answer = installer(&|env| ssh::fetch_runtime_with_profile(&destination, &files, &pieces, env, profile.as_ref(), &report, &log))?;
            let mut uploaded = false;
            if let ServerRuntime::NeedsArchive { error } = &answer {
                if let Some(error) = error {
                    events.write_log(&format!("{} could not download the Linux runtime itself: {error}", target.label()));
                }
                let base = match development_base(paths) {
                    Some(base) => base,
                    None => {
                        if !download_linux_runtime(modules, LINUX_RUNTIME, locale, ask, progress)? {
                            return Ok(None);
                        }
                        modules.path(LINUX_RUNTIME)?
                    }
                };
                answer = installer(&|env| ssh::receive_runtime_with_profile(&destination, &files, &base, env, profile.as_ref(), &report, &log))?;
                uploaded = true;
            }
            let host = ssh::installed_paths(&destination, answer, uploaded)?;
            if uploaded {
                remove_linux_runtime(paths, modules, &events, &[LINUX_RUNTIME]);
            }
            (host, false)
        }
    };

    progress(i18n::text(locale, "native.starting").to_owned().into());
    let env = shared_env(target);
    let recent_events = events.clone();
    let clear_events = events.clone();
    let process = ssh::start_host_process_with_profile(
        &destination,
        &host,
        &env,
        profile.as_ref(),
        askpass,
        events.clone(),
        &move || recent_events.recent(),
        &move || clear_events.clear_recent(),
    )?;
    let prune = (paths.packaged() && !pruned_during_probe).then(|| {
        let (askpass, destination, files, events, profile) = (askpass.clone(), destination.clone(), files.clone(), events.clone(), profile.clone());
        Box::new(move || match ssh::prune_runtimes_with_profile(&destination, &files, askpass.session(&ssh::askpass_scope(&destination)).env(), profile.as_ref()) {
            Ok(removed) if !removed.is_empty() => events.write_log(&format!("Removed {} Linux runtimes that earlier versions installed", removed.len())),
            Ok(_) => {}
            Err(error) => events.write_log(&format!("Earlier Linux runtimes were not removed: {error:#}")),
        }) as Prune
    });
    Ok(Some((process, prune)))
}

/// Reconnect an SSH Host in place without restarting RainyAgent.
pub fn reconnect_ssh(
    paths: &Paths,
    target: &ExecutionTarget,
    events: Arc<CarrierEvents>,
    askpass: &Arc<Askpass>,
    modules: &Modules,
    previous: Option<(&str, u32)>,
) -> Result<ReconnectOutcome> {
    let destination = ssh::destination_of(target)?;
    let profile = ssh_profile(paths, target);
    let files = runtime_files(paths, modules);
    let env = shared_env(target);
    let recent_events = events.clone();
    let clear_events = events.clone();
    let log_events = events.clone();
    ssh::reconnect_host_with_profile(
        &destination,
        &files,
        &env,
        profile.as_ref(),
        askpass,
        events,
        previous,
        &move || recent_events.recent(),
        &move || clear_events.clear_recent(),
        &move |line| log_events.write_log(line),
    )
}
