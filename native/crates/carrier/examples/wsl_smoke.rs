//! Install the Linux runtime in a WSL distribution under a throwaway HOME, start the Host there and call it.
//!
//! RAINY_DEV_WSL_HOME=/tmp/rainy-gpui-home cargo run -p rainy-carrier --example wsl_smoke -- <dev-resources folder> [distro]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context as _, Result, bail};
use rainy_carrier::wsl::{self, Installed, RuntimeFiles};
use rainy_carrier::{ExecutionTarget, HostEvents, HostProcess, RpcClient, targets};
use serde_json::json;

struct Events;
impl HostEvents for Events {
    fn diagnostic(&self, line: &str) {
        eprintln!("[host] {line}");
    }
}

fn main() -> Result<()> {
    if std::env::var("RAINY_DEV_WSL_HOME").is_err() {
        bail!("Set RAINY_DEV_WSL_HOME so the test never touches the real ~/.rainy-agent");
    }
    let mut args = std::env::args().skip(1);
    let resources = PathBuf::from(args.next().context("dev-resources folder")?);
    let distros = targets::wsl_distributions();
    println!("distributions: {:?}", distros.iter().map(ExecutionTarget::label).collect::<Vec<_>>());
    let wanted = args.next();
    let distro = distros
        .iter()
        .find_map(|target| match target {
            ExecutionTarget::Wsl { distro, .. } if wanted.as_deref().is_none_or(|w| w == distro) => Some(distro.clone()),
            _ => None,
        })
        .context("no WSL 2 distribution")?;
    let started = Instant::now();
    wsl::warm_up(&distro);
    println!("warm-up {:?}", started.elapsed());
    let files = RuntimeFiles {
        metadata: resources.join("linux-runtime.json"),
        app: resources.join("linux-app.tar.gz"),
        installer: resources.join("install-runtime.py"),
        base: None,
    };
    let mut installed = wsl::install_runtime(&distro, &files)?;
    println!("without base archive: {installed:?} ({:?})", started.elapsed());
    if installed == Installed::NeedsArchive {
        installed = wsl::install_runtime(&distro, &RuntimeFiles { base: Some(resources.join("linux-runtime.tar.xz")), ..files.clone() })?;
        println!("with base archive: {installed:?} ({:?})", started.elapsed());
    }
    let Installed::Ready { host } = installed else { bail!("runtime not installed") };
    let home = std::env::var("RAINY_DEV_WSL_HOME")?;
    let command = wsl::host_command(&distro, &host, &[
        ("RAINY_EXECUTION_TARGET_ID".into(), format!("wsl:{distro}")),
        ("RAINY_EXECUTION_TARGET_LABEL".into(), format!("WSL · {distro}")),
        ("RAINY_HOME".into(), format!("{home}/.rainy-agent")),
    ]);
    let host_started = Instant::now();
    let process = HostProcess::start(&command, Arc::new(Events))?;
    println!("Host ready in {:?} on port {}", host_started.elapsed(), process.ready().port);
    let (client, _events) = RpcClient::connect(process.ready())?;
    let info = smol::block_on(client.call("app.info", json!(null)));
    println!("app.info = {info:?}");
    client.close();
    process.stop();
    println!("pruned: {:?}", wsl::prune_runtimes(&distro, &files)?);
    Ok(())
}
