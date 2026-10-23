//! The Rainy Host: everything that runs where the user's files are (Windows, WSL or an SSH server) — the agent, the
//! editor services, runs and terminals — served to the interface over one loopback HTTP server.

mod activity;
mod control;
mod credentials;
mod env;
mod error;
mod git;
mod ide;
mod locale;
mod map;
mod network;
mod paths;
mod process;
mod projects;
mod registry;
mod rpc;
mod runtime;
mod search;
mod server;
mod settings;
mod store;
mod text;

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result};
use axum::http::StatusCode;
use serde_json::{Value, json};

use crate::activity::Activity;
use crate::control::ControlInput;
use crate::env::HostEnv;
use crate::error::HostError;
use crate::network::Network;
use crate::projects::Projects;
use crate::registry::Registry;
use crate::rpc::RpcHub;
use crate::runtime::Runtime;
use crate::server::HostServer;
use crate::settings::Settings;

/// Diagnostics go to stderr; stdout belongs to the control channel.
struct StderrLog;

impl log::Log for StderrLog {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.level() <= log::Level::Info || std::env::var_os("RAINY_HOST_DEBUG").is_some()
    }

    fn log(&self, record: &log::Record<'_>) {
        if self.enabled(record.metadata()) && record.target().starts_with("rainy_host") {
            eprintln!("[host] {}", record.args());
        }
    }

    fn flush(&self) {}
}

/// Everything the modules share.
pub struct Host {
    pub env: HostEnv,
    pub settings: Arc<Settings>,
    pub network: Arc<Network>,
    pub projects: Arc<Projects>,
    pub registry: Arc<Registry>,
    pub activity: Arc<Activity>,
    pub rpc: Arc<RpcHub>,
    pub server: Arc<HostServer>,
    pub runtime: Arc<Runtime>,
    pub control: ControlInput,
    pub ide: std::sync::OnceLock<Arc<ide::Ide>>,
    pub maps: Arc<map::ProjectMaps>,
}

fn main() {
    let _ = log::set_logger(&StderrLog).map(|()| log::set_max_level(log::LevelFilter::Debug));
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("tokio runtime");
    if let Err(error) = runtime.block_on(start()) {
        log::error!("{error:#}");
        control::send(json!({ "type": "fatal", "message": format!("{error:#}") }));
        std::process::exit(1);
    }
}

async fn start() -> Result<()> {
    let env = HostEnv::read()?;
    // Replies to the Host's own requests can arrive while it is still starting.
    let (control, mut commands) = ControlInput::start();
    store::create_private_dir(&env.home).context("The Host home cannot be created")?;
    store::create_private_dir(&env.tmp).context("The Host's temporary folder cannot be created")?;

    // API keys are encrypted with a key the carrier keeps with Windows data protection; it is null when Windows cannot.
    let mut seal = None;
    if std::env::var("RAINY_CARRIER_CREDENTIALS").as_deref() == Ok("1") {
        let reply = control.ask(control::request("credential-key", Value::Null), Duration::from_secs(30)).await?;
        match reply.get("key") {
            Some(Value::String(key)) => {
                use base64::Engine as _;
                let bytes = base64::engine::general_purpose::STANDARD.decode(key).context("The carrier sent an invalid credential key")?;
                seal = Some(credentials::Seal::new(&bytes)?);
            }
            Some(Value::Null) => {}
            _ => anyhow::bail!("The carrier sent an invalid credential key"),
        }
    }
    let settings = Arc::new(Settings::load(&env.home, seal)?);
    locale::set(&settings.locale());

    // The Windows carrier answers which proxy the system uses for each address; without it, the environment decides.
    let system: Option<network::SystemProxy> = (std::env::var("RAINY_CARRIER_PROXY").as_deref() == Ok("1")).then(|| {
        let control = control.clone();
        let ask: network::SystemProxy = Arc::new(move |url: String| {
            let control = control.clone();
            Box::pin(async move {
                let reply = control.ask(control::request("system-proxy", json!({ "url": url })), Duration::from_secs(5)).await?;
                match reply.get("url") {
                    Some(Value::String(url)) => Ok(Some(url.clone())),
                    Some(Value::Null) => Ok(None),
                    _ => Err(anyhow::anyhow!("The carrier sent an invalid system proxy")),
                }
            })
        });
        ask
    });
    let network = Arc::new(Network::new(env.windows, system));
    network.configure(settings.get().proxy);

    let projects = Arc::new(Projects::load(env.home.join("projects.json"))?);
    let registry = Arc::new(Registry::new(env.carrier_state_root.clone(), env.target_id.clone()));
    let activity = Arc::new(Activity::default());
    let rpc = Arc::new(RpcHub::default());
    let workbench = settings.get().workbench.unwrap_or_default();
    let (info_env, label) = (env.clone(), env.target_label.clone().unwrap_or_else(|| env.kind().to_owned()));
    let server = HostServer::new(env.renderer_root(), move || {
        vec![
            ("__RAINY_AGENT__".to_owned(), json!({ "name": "Rainy", "version": info_env.version, "environment": info_env.kind(), "label": label })),
            ("__RAINY_WORKBENCH_CONFIG__".to_owned(), Value::Object(workbench.clone())),
        ]
    });
    let runtime = Runtime::new(env.clone(), projects.clone(), registry.clone(), activity.clone())?;
    let maps = Arc::new(map::ProjectMaps::new(projects.clone(), env.home.join("maps")));
    let host = Arc::new(Host { env: env.clone(), settings, network, projects, registry, activity, rpc, server, runtime, control, ide: std::sync::OnceLock::new(), maps });
    let _ = host.ide.set(ide::Ide::new(&host));

    register(&host);
    host.server.listen(env.port).await?;

    // A project the registry refuses (the same folder recorded for another target, as with two SSH aliases of one
    // server) stays usable here; only its cross-target identity is missing.
    for project in host.projects.list() {
        if let Err(error) = host.registry.get_or_register(&project.id, &project.path, &project.title, None).await {
            log::warn!("Project {} is not registered for this execution target: {}", project.path, error.message);
        }
    }

    let ready = json!({ "type": "ready", "protocol": 1, "url": host.server.launch_url(), "pid": std::process::id(), "home": env.home });
    if let Ok(file) = std::env::var("RAINY_READY_FILE") {
        let _ = store::write_atomic(std::path::Path::new(&file), ready.to_string().as_bytes());
    }
    control::send(ready);
    log::info!("ready on 127.0.0.1:{}", host.server.port());

    let detached = std::env::var("RAINY_DETACHED").as_deref() == Ok("1");
    loop {
        tokio::select! {
            command = commands.recv() => match command {
                Some(command) => {
                    if command.get("type").and_then(Value::as_str) == Some("stop") {
                        break;
                    }
                    serve_command(&host, command);
                }
                None if detached => std::future::pending::<()>().await,
                None => break,
            },
            _ = tokio::signal::ctrl_c() => break,
        }
    }
    stop(&host).await;
    Ok(())
}

/// Answer one carrier command.
fn serve_command(host: &Arc<Host>, command: control::Command) {
    let Some(id) = command.get("id").and_then(Value::as_str).map(str::to_owned) else { return };
    match command.get("type").and_then(Value::as_str) {
        Some("inspect-activity") => {
            let mode = command.get("mode").and_then(Value::as_str).unwrap_or("observe");
            control::send(json!({ "type": "activity", "id": id, "active": host.activity.inspect(mode) }));
        }
        Some("inspect-project") => {
            let host = host.clone();
            let requested = command.get("workspaceId").and_then(Value::as_str).map(str::to_owned);
            tokio::spawn(async move {
                let answer = inspect_project(&host, requested).await;
                control::send(match answer {
                    Ok(project) => json!({ "type": "project", "id": id, "project": project }),
                    Err(code) => json!({ "type": "project", "id": id, "error": code }),
                });
            });
        }
        _ => {}
    }
}

async fn inspect_project(host: &Host, requested: Option<String>) -> Result<Value, &'static str> {
    let Some(id) = requested.or_else(|| selected_workspace(host)) else { return Ok(Value::Null) };
    let project = host.projects.get(&id).ok_or("workspace-unavailable")?;
    let project_id = host.registry.get_or_register(&project.id, &project.path, &project.title, None).await.map_err(|_| "project-unavailable")?;
    Ok(json!({ "projectId": project_id, "workspaceId": project.id, "roots": project.roots() }))
}

/// The project selected last in the editor.
fn selected_workspace(host: &Host) -> Option<String> {
    host.ide.get().and_then(|ide| ide.selection())
}

async fn stop(host: &Arc<Host>) {
    host.rpc.close();
    host.maps.close();
    if let Some(ide) = host.ide.get() {
        let _ = tokio::time::timeout(Duration::from_secs(10), ide.close()).await;
    }
    let _ = std::fs::remove_dir_all(&host.env.tmp);
}

fn register(host: &Arc<Host>) {
    let rpc = &host.rpc;
    {
        let host = host.clone();
        rpc.register("app.info", move |_| {
            let env = host.env.clone();
            async move {
                Ok(json!({
                    "name": "Rainy", "version": env.version, "environment": env.kind(), "executionTargetId": env.target_id,
                    "platform": env.platform(), "home": env.home, "node": "",
                }))
            }
        });
    }
    {
        let host = host.clone();
        rpc.register("prefs.get", move |_| {
            let prefs = host.settings.prefs();
            async move { Ok(prefs) }
        });
    }
    {
        let host = host.clone();
        rpc.register("prefs.set", move |params| {
            let host = host.clone();
            async move {
                let Value::Object(change) = params else { return Err(HostError::invalid("Preferences must be an object")) };
                let prefs = host.settings.set_prefs(change)?;
                locale::set(&host.settings.locale());
                host.rpc.emit("prefs.changed", &prefs);
                Ok(prefs)
            }
        });
    }
    {
        let host = host.clone();
        rpc.register("ide", move |params| {
            let host = host.clone();
            async move {
                let ide = host.ide.get().cloned().ok_or_else(|| HostError::other("The IDE is not ready."))?;
                ide.handle(&host, params).await
            }
        });
    }
    {
        let host = host.clone();
        rpc.register("runtime", move |params| {
            let host = host.clone();
            async move { host.runtime.handle(params).await }
        });
    }
    {
        let maps = host.maps.clone();
        rpc.register("map.get", move |params| {
            let maps = maps.clone();
            async move {
                let workspace_id = params["workspaceId"].as_str().filter(|id| !id.is_empty()).ok_or_else(|| HostError::invalid("workspaceId is required"))?.to_owned();
                maps.get(&workspace_id, params["refresh"] == true).await
            }
        });
    }
    {
        let search = Arc::new(search::TextSearch::new(host.projects.clone()));
        for method in ["search.text", "search.replace"] {
            let search = search.clone();
            rpc.register(method, move |params| {
                let search = search.clone();
                async move {
                    tokio::task::spawn_blocking(move || if method == "search.text" { search.search(&params) } else { search.replace(&params) })
                        .await
                        .map_err(|error| HostError::other(error.to_string()))?
                }
            });
        }
    }
    {
        let git = Arc::new(git::Git::new(host.projects.clone(), host.runtime.clone()));
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct GitRequest {
            workspace_id: String,
            #[serde(default)]
            root_id: String,
            #[serde(default)]
            paths: Vec<String>,
            #[serde(default)]
            message: String,
            #[serde(default)]
            stage_all: bool,
        }
        for method in ["git.status", "git.stage", "git.unstage", "git.discard", "git.commit", "git.init"] {
            let git = git.clone();
            rpc.register(method, move |params| {
                let git = git.clone();
                async move {
                    let request: GitRequest = rpc::params(params)?;
                    let (ws, root) = (request.workspace_id.as_str(), request.root_id.as_str());
                    match method {
                        "git.status" => git.status(ws).await,
                        "git.stage" => git.stage(ws, root, &request.paths).await.map(|()| Value::Null),
                        "git.unstage" => git.unstage(ws, root, &request.paths).await.map(|()| Value::Null),
                        "git.discard" => git.discard(ws, root, &request.paths).await.map(|()| Value::Null),
                        "git.commit" => git.commit(ws, root, &request.message, request.stage_all).await,
                        _ => git.init(ws, root).await.map(|()| Value::Null),
                    }
                }
            });
        }
    }
    {
        let host_socket = host.rpc.clone();
        host.server.socket("/rpc", Arc::new(move |socket, _| Box::pin(host_socket.clone().serve(socket))));
    }
    {
        let icon = host.env.resources.join("icon.png");
        host.server.route(
            "/rainy/icon.png",
            Arc::new(move |_, _| {
                let icon = icon.clone();
                Box::pin(async move {
                    match tokio::fs::read(&icon).await {
                        Ok(bytes) => {
                            let mut response = axum::response::Response::new(axum::body::Body::from(bytes));
                            let headers = response.headers_mut();
                            headers.insert(axum::http::header::CONTENT_TYPE, axum::http::HeaderValue::from_static("image/png"));
                            headers.insert(axum::http::header::CACHE_CONTROL, axum::http::HeaderValue::from_static("private, max-age=86400"));
                            response
                        }
                        Err(_) => server::empty(StatusCode::NOT_FOUND),
                    }
                })
            }),
        );
    }
}
