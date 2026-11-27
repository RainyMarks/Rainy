//! Model, external agent, prompt, network, security, hooks, extensions and project-memory RPC handlers.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};

use super::params::{
    ExtensionsParams, ExternalConfigureParams, ExternalProbeParams, HooksParams, MemoryEdit, MemoryNote, MemorySwitches, ModeParams, NameParams, OptionalWorkspace, ProviderParams, ProxyParams,
    RuleParams, SaveServerParams, TextParams, TrustParams, WorkspaceRef,
};
use super::{Agent, permission_mode};
use crate::agent::prompts;
use crate::agent::store::MetaPatch;
use crate::error::HostError;
use crate::locale::l;
use crate::settings::{ModelSelection, check_server};

impl Agent {
    pub(super) fn register_external(self: &Arc<Self>) {
        // The models of Codex and Antigravity, while none are known, are listed in the background (neither opens a
        // conversation); `external.changed` carries them.
        self.on("external.status", |agent, _: Value| async move {
            let background = agent.clone();
            tokio::spawn(async move {
                let (external, home) = (&background.services.external, &background.services.env.home);
                let (codex, agy) = tokio::join!(external.list_models_if_unknown("codex", home), external.list_models_if_unknown("antigravity", home));
                if codex || agy {
                    background.services.rpc.emit("external.changed", &external.status());
                }
            });
            Ok(agent.services.external.status())
        });
        // Read an agent's usage limits again (Codex and Antigravity), unless read within the last minute and not
        // `force`d, and its models while none are known; the answer and `external.changed` carry them.
        self.on("external.quota", |agent, params: Value| async move {
            let kind = params.get("kind").and_then(Value::as_str).unwrap_or_default().to_owned();
            let force = params.get("force").and_then(Value::as_bool).unwrap_or(false);
            let cwd = params.get("cwd").and_then(Value::as_str).map(PathBuf::from).filter(|path| path.is_dir()).unwrap_or_else(|| agent.services.env.home.clone());
            let external = &agent.services.external;
            let (quota, models) = tokio::join!(external.refresh_quota(&kind, force, &cwd), external.list_models_if_unknown(&kind, &cwd));
            if quota || models {
                agent.services.rpc.emit("external.changed", &external.status());
            }
            Ok(external.status())
        });
        self.on("external.probe", |agent, params: ExternalProbeParams| async move {
            let cwd = match params.cwd.as_deref().map(str::trim).filter(|p| !p.is_empty()) {
                Some(raw) => {
                    let path = PathBuf::from(raw);
                    if !path.is_dir() {
                        return Err(HostError::invalid(l("探测目录不存在或不是有效文件夹。", "The probe directory does not exist or is not a valid folder.")));
                    }
                    path
                }
                None => agent.services.env.home.clone(),
            };
            agent
                .services
                .external
                .probe(params.kind.as_deref(), params.binary.as_deref(), params.args.as_deref(), &cwd)
                .await
        });
        self.on("external.configure", |agent, params: ExternalConfigureParams| async move {
            let status = agent.services.external.configure(params.config, params.remove).await?;
            agent.services.rpc.emit("external.changed", &status);
            Ok(status)
        });
    }

    pub(super) fn register_models(self: &Arc<Self>) {
        self.on("models.status", |agent, _: Value| async move { Ok(agent.services.models.status()) });
        self.on("models.configure", |agent, input: Value| async move { agent.services.models.configure(&input) });
        self.on("models.remove", |agent, params: ProviderParams| async move { agent.services.models.remove(&params.provider) });
        self.on("models.select", |agent, selection: ModelSelection| async move { agent.services.models.select(&selection) });
        self.on("models.discover", |agent, input: Value| async move { agent.services.models.discover(&input).await });
        self.on("models.probe", |agent, input: Value| async move { agent.services.models.probe(&input).await });
        self.on("models.connectStrata", |agent, input: Value| async move { agent.services.models.connect_strata(&input).await });
        self.on("prompt.global", |agent, params: TextParams| async move { agent.services.models.set_global_prompt(&params.text) });
        self.on("prompts.status", |agent, params: prompts::StatusParams| async move {
            let cwd = agent.project_folder(params.workspace_id.as_deref());
            Ok(json!({
                "builtin": prompts::builtin_status(&agent.services.settings),
                "instructions": prompts::instructions_status(&agent.services.env.home, cwd.as_deref()),
            }))
        });
        self.on("prompts.builtin", |agent, params: prompts::BuiltinParams| async move { prompts::set_builtin(&agent.services.settings, &params) });
        self.on("prompts.saveInstructions", |agent, params: prompts::SaveParams| async move {
            let cwd = agent.project_folder(params.workspace_id.as_deref());
            prompts::save_instructions(&agent.services.env.home, cwd.as_deref(), &params)
        });
    }

    pub(super) fn register_settings(self: &Arc<Self>) {
        self.on("network.status", |agent, _: Value| async move { Ok(agent.network_status().await) });
        self.on("network.set", |agent, params: ProxyParams| async move {
            let choice = agent.proxy_choice(params)?;
            let data = agent.services.settings.update(|data| {
                data.proxy = Some(choice);
                Ok(())
            })?;
            agent.services.network.configure(data.proxy);
            Ok(agent.network_status().await)
        });
        self.on("network.test", |agent, params: ProxyParams| async move {
            let proxy = agent.proxy_choice(params)?;
            let resolved = agent.services.models.resolve(None).ok_or_else(|| HostError::new("no-model", l("请先配置模型，再测试连接。", "Configure a model before testing the connection.")))?;
            let url = resolved.setup.base_url;
            let target = url::Url::parse(&url).map_err(|error| HostError::invalid(error.to_string()))?;
            let started = crate::text::now_ms();
            let client = agent.services.network.client_with(&proxy, &target).await;
            // Any HTTP answer, even 401 or 404, shows the service is reachable through these settings.
            let answer = client.get(target).timeout(Duration::from_secs(15)).send().await;
            let ms = crate::text::now_ms().saturating_sub(started);
            Ok(match answer {
                Ok(response) => json!({ "ok": true, "url": url, "status": response.status().as_u16(), "ms": ms }),
                Err(error) => {
                    let detail = crate::agent::llm::transport_error(&error);
                    json!({ "ok": false, "url": url, "ms": ms, "error": detail })
                }
            })
        });
        self.on("security.status", |agent, params: OptionalWorkspace| async move { Ok(agent.security_status(params.workspace_id.as_deref())) });
        self.on("security.setDefault", |agent, params: ModeParams| async move {
            let next = permission_mode(&params.mode)?;
            let previous = agent.services.settings.default_permission();
            if next != previous {
                // Chats from before 2.1.1 have no mode of their own; they keep the one they have been running with.
                for chat in agent.services.store.list().into_iter().filter(|chat| chat.permission.is_none()) {
                    match agent.live(&chat.id) {
                        Some(session) => {
                            session.set_permission(&previous).await?;
                        }
                        None => {
                            agent.services.store.set_meta(&chat.id, MetaPatch { permission: Some(previous.clone()), ..MetaPatch::default() }).await?;
                        }
                    }
                }
            }
            agent.services.settings.set_default_permission(&next)?;
            for session in agent.live_sessions() {
                session.refresh();
            }
            let status = agent.security_status(None);
            agent.services.rpc.emit("security.changed", json!({ "defaultMode": status["defaultMode"] }));
            Ok(status)
        });
        self.on("security.addRule", |agent, params: RuleParams| async move {
            agent.services.settings.set_permission_rule_kind(&params.workspace_id, &params.kind, &params.rule, true)?;
            let status = agent.security_status(Some(&params.workspace_id));
            agent.services.rpc.emit("security.changed", &status);
            Ok(status)
        });
        self.on("security.removeRule", |agent, params: RuleParams| async move {
            agent.services.settings.set_permission_rule_kind(&params.workspace_id, &params.kind, &params.rule, false)?;
            let status = agent.security_status(Some(&params.workspace_id));
            agent.services.rpc.emit("security.changed", &status);
            Ok(status)
        });
        for method in ["settings.trustWorkspace", "security.setTrust"] {
            self.on(method, |agent, params: TrustParams| async move {
                let target = params
                    .path
                    .as_deref()
                    .map(str::trim)
                    .filter(|p| !p.is_empty())
                    .map(str::to_owned)
                    .or_else(|| agent.cwd_for(params.workspace_id.as_deref()))
                    .or_else(|| params.workspace_id.as_deref().map(str::trim).filter(|id| !id.is_empty()).map(str::to_owned))
                    .ok_or_else(|| HostError::invalid(l("需要提供工作区 ID 或路径。", "A workspaceId or path is required.")))?;
                agent.services.settings.set_workspace_trusted(&target, params.trusted)?;
                let status = agent.security_status(params.workspace_id.as_deref());
                agent.services.rpc.emit("security.changed", &status);
                let cwd = agent.extensions_cwd.lock().unwrap().clone();
                agent.services.rpc.emit("extensions.changed", agent.extensions_status(cwd.as_deref()));
                Ok(status)
            });
        }
        self.on("hooks.list", |agent, params: OptionalWorkspace| async move {
            let global = agent.services.settings.hooks();
            let cwd = agent.cwd_for(params.workspace_id.as_deref());
            let trusted = agent.services.settings.is_workspace_trusted(params.workspace_id.as_deref(), cwd.as_deref().map(Path::new));
            let has_project = cwd.as_deref().is_some_and(|dir| crate::agent::hooks::has_project_hooks(Path::new(dir)));
            let project = if trusted {
                cwd.as_deref().map(|dir| crate::agent::hooks::load_project_hooks(Path::new(dir))).unwrap_or_default()
            } else {
                Vec::new()
            };
            Ok(json!({
                "global": global,
                "project": project,
                "trusted": trusted,
                "hasProjectHooks": has_project,
                "projectHooksSkipped": !trusted && has_project,
            }))
        });
        self.on("hooks.save", |agent, params: HooksParams| async move {
            let saved = agent.services.settings.set_hooks(params.hooks)?;
            Ok(json!({ "global": saved }))
        });
        self.on("extensions.status", |agent, params: ExtensionsParams| async move {
            *agent.extensions_cwd.lock().unwrap() = params.cwd.clone();
            Ok(agent.extensions_status(params.cwd.as_deref()))
        });
        self.on("extensions.saveServer", |agent, params: SaveServerParams| async move {
            check_server(&params.server)?;
            agent.save_servers(|servers| {
                let previous = params.previous_name.as_deref().unwrap_or(&params.server.name);
                if params.server.name != previous && servers.iter().any(|item| item.name == params.server.name) {
                    let name = &params.server.name;
                    return Err(HostError::new("duplicate", l(&format!("已存在名为 {name} 的 MCP 服务器。"), &format!("An MCP server named {name} already exists."))));
                }
                match servers.iter().position(|item| item.name == previous) {
                    Some(at) => servers[at] = params.server,
                    None => servers.push(params.server),
                }
                Ok(())
            })
        });
        self.on("extensions.removeServer", |agent, params: NameParams| async move {
            agent.save_servers(|servers| {
                servers.retain(|item| item.name != params.name);
                Ok(())
            })
        });
        self.on("memory.status", |agent, params: WorkspaceRef| async move { agent.services.memory.status(&params.workspace_id).await });
        self.on("memory.setEnabled", |agent, params: MemorySwitches| async move {
            agent.services.memory.set_enabled(&params.workspace_id, params.enabled, params.generation_enabled).await
        });
        self.on("memory.edit", |agent, params: MemoryEdit| async move {
            agent.services.memory.edit(&params.workspace_id, &params.id, &params.text, params.expected_revision).await
        });
        self.on("memory.delete", |agent, params: MemoryNote| async move { agent.services.memory.remove(&params.workspace_id, Some(&params.id)).await });
        self.on("memory.clear", |agent, params: WorkspaceRef| async move { agent.services.memory.remove(&params.workspace_id, None).await });
    }
}
