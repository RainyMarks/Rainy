//! Start a native Windows Host from a `windows-host` folder with a throwaway home, call a few methods and stop it.
//!
//! cargo run -p rainy-carrier --example smoke -- <windows-host folder> <scratch folder>

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context as _, Result};
use rainy_carrier::{HostCommand, HostEvents, HostProcess, RpcClient, RpcEvent, credential, proxy};
use serde_json::json;

struct Events {
    user_data: PathBuf,
}

impl HostEvents for Events {
    fn credential_key(&self) -> Option<String> {
        credential::credential_key(&self.user_data, &credential::Dpapi, &|line| eprintln!("[key] {line}")).ok().flatten()
    }
    fn system_proxy(&self, url: &str) -> Option<String> {
        let proxy = proxy::system_proxy(url);
        eprintln!("[proxy] {url} -> {proxy:?}");
        proxy
    }
    fn diagnostic(&self, line: &str) {
        eprintln!("[host] {line}");
    }
    fn exited(&self, code: Option<i32>) {
        eprintln!("[host exited] {code:?}");
    }
}

fn main() -> Result<()> {
    let mut args = std::env::args_os().skip(1);
    let root = PathBuf::from(args.next().context("windows-host folder")?);
    let scratch = PathBuf::from(args.next().context("scratch folder")?);
    let user_data = scratch.join("user-data");
    std::fs::create_dir_all(&user_data)?;
    let command = HostCommand {
        program: root.join("rainy-host.exe"),
        args: Vec::new(),
        env: [
            ("RAINY_EXECUTION_TARGET_ID", "windows-local".into()),
            ("RAINY_EXECUTION_TARGET_LABEL", "Windows".into()),
            ("RAINY_CARRIER_CREDENTIALS", "1".into()),
            ("RAINY_CARRIER_PROXY", "1".into()),
            ("RAINY_HOME", user_data.join("native-home").display().to_string()),
            ("RAINY_CARRIER_STATE_ROOT", user_data.join("carrier-state").display().to_string()),
            ("RAINY_TOOLCHAIN_ROOT", user_data.join("env").display().to_string()),
        ]
        .into_iter()
        .map(|(name, value)| (name.into(), value.into()))
        .collect(),
        cwd: Some(scratch.clone()),
        scrub_environment: true,
        wait_for_loopback: false,
        expected_port: None,
    };
    let started = Instant::now();
    let host = HostProcess::start(&command, Arc::new(Events { user_data }))?;
    println!("ready in {:?}: port {} pid {} home {}", started.elapsed(), host.ready().port, host.ready().pid, host.ready().home);
    let (client, events) = RpcClient::connect(host.ready())?;
    smol::block_on(async {
        let info = client.call("app.info", json!(null)).await?;
        println!("app.info = {info}");
        let prefs: rainy_protocol::UiPreferences = client.request("prefs.get", json!(null)).await?;
        println!("prefs.locale = {} theme = {}", prefs.locale, prefs.theme);
        let models: rainy_protocol::ModelsStatus = client.request("models.status", json!(null)).await?;
        println!("models = {} presets = {}", models.models.len(), models.presets.len());
        let sessions: Vec<rainy_protocol::SessionSummary> = client.request("sessions.list", json!({})).await?;
        println!("sessions = {}", sessions.len());
        let created: rainy_protocol::SessionSummary = client.request("sessions.create", json!({"workspaceId": null})).await?;
        let snapshot: rainy_protocol::SessionSnapshot = client.request("sessions.get", json!({"sessionId": created.id})).await?;
        println!("created {} with {} entries, permission {}", snapshot.summary.id, snapshot.entries.len(), snapshot.permission);
        let missing = client.call("no.such.method", json!(null)).await;
        println!("unknown method -> {missing:?}");
        while let Ok(event) = events.try_recv() {
            if let RpcEvent::Event { name, .. } = event {
                println!("event {name}");
            }
        }
        anyhow::Ok(())
    })?;
    client.close();
    let stopping = Instant::now();
    host.stop();
    println!("stopped in {:?}", stopping.elapsed());
    Ok(())
}
