//! Start a native Windows Host with a given data folder, call one method and print the result as JSON.
//!
//! cargo run -p rainy-carrier --example call -- <windows-host folder> <user-data folder> <method> [params JSON]

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context as _, Result};
use rainy_carrier::{HostCommand, HostEvents, HostProcess, RpcClient, credential};
use serde_json::Value;

struct Events {
    user_data: PathBuf,
}

impl HostEvents for Events {
    fn credential_key(&self) -> Option<String> {
        credential::credential_key(&self.user_data, &credential::Dpapi, &|line| eprintln!("[key] {line}")).ok().flatten()
    }
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let root = PathBuf::from(args.next().context("windows-host folder")?);
    let user_data = PathBuf::from(args.next().context("user-data folder")?);
    let method = args.next().context("method")?;
    let params: Value = match args.next() {
        Some(text) => serde_json::from_str(&text).context("params JSON")?,
        None => Value::Null,
    };
    std::fs::create_dir_all(&user_data)?;
    let data = |name: &str| user_data.join(name).display().to_string();
    let command = HostCommand {
        program: root.join("node").join("node.exe"),
        args: vec![root.join("app").join("dist").join("host.js").into()],
        env: [
            ("RAINY_EXECUTION_TARGET_ID", "windows-local".to_owned()),
            ("RAINY_EXECUTION_TARGET_LABEL", "Windows".to_owned()),
            ("RAINY_CARRIER_CREDENTIALS", "1".to_owned()),
            ("RAINY_HOME", data("native-home")),
            ("RAINY_CARRIER_STATE_ROOT", data("carrier-state")),
            ("RAINY_TOOLCHAIN_ROOT", data("env")),
        ]
        .into_iter()
        .map(|(name, value)| (name.into(), value.into()))
        .collect(),
        cwd: None,
        scrub_environment: true,
        wait_for_loopback: false,
        expected_port: None,
    };
    let host = HostProcess::start(&command, Arc::new(Events { user_data: user_data.clone() }))?;
    let (client, _events) = RpcClient::connect(host.ready())?;
    let result = smol::block_on(client.call(&method, params));
    client.close();
    host.stop();
    match result {
        Ok(value) => println!("{}", serde_json::to_string_pretty(&value)?),
        Err(error) => anyhow::bail!("{}: {}", error.code, error.message),
    }
    Ok(())
}
