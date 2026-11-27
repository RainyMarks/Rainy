//! Skills & MCP (`ExtensionsSection.tsx`): discovered skills, and the MCP servers every chat may use, added one by one
//! or imported from pasted MCP JSON.

mod import_panel;
mod server_form;
mod server_list;
mod skills;

use std::collections::{BTreeMap, HashSet};

use gpui_kit::component::notification::Notification;
use gpui_kit::component::{WindowExt as _, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use rainy_carrier::mcp_json::McpImportEnvironment;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use self::import_panel::{Import, import_environment};
use self::server_form::Form;
use super::heading;
use super::models_extra::notice;
use super::widgets::AsyncPageGuard;
use crate::store::ide::{Ide, IdeEvent};
use crate::store::{Store, StoreEvent};
use crate::ui::trf;

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct McpServer {
    pub name: String,
    pub enabled: bool,
    /// `stdio` or `streamable-http`.
    pub transport: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<BTreeMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers: Option<BTreeMap<String, String>>,
    /// Tools the server lists that are allowed; empty allows all.
    #[serde(default)]
    pub tools: Vec<String>,
    /// Tools turned off in the server's tool list; a tool the server adds later is on.
    #[serde(default, rename = "disabledTools", skip_serializing_if = "Vec::is_empty")]
    pub disabled_tools: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
struct McpTool {
    name: String,
    #[serde(default)]
    description: String,
    enabled: bool,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
struct McpStatus {
    name: String,
    state: String,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    tools: Vec<McpTool>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
struct Skill {
    id: String,
    scope: String,
    name: String,
    #[serde(default)]
    description: String,
    path: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
struct Status {
    skills: Vec<Skill>,
    servers: Vec<McpServer>,
    status: Vec<McpStatus>,
}

/// The Windows-MCP release the desktop preset installs, its dependency cut-off, and the tools it leaves out.
const WINDOWS_MCP_VERSION: &str = "0.8.7";
const WINDOWS_MCP_DEPENDENCIES_BEFORE: &str = "2026-10-04T00:00:00Z";
const WINDOWS_MCP_EXCLUDED_TOOLS: &str = "PowerShell,FileSystem,Registry,Process,Scrape,Notification";

fn is_windows_mcp(server: &McpServer) -> bool {
    server.transport == "stdio"
        && server.command.iter().chain(server.args.iter().flatten()).any(|part| {
            let lower = part.to_ascii_lowercase();
            lower.strip_prefix("windows-mcp").is_some_and(|rest| rest.is_empty() || rest.starts_with(['=', '<', '>', '@', '~', '!']))
        })
}

/// Computes the next `disabled_tools` list when toggling `names` on or off for a server with `live` tools.
fn next_disabled_tools(server: &McpServer, live: &[McpTool], names: &[String], on: bool) -> Vec<String> {
    let mut disabled: Vec<String> = live.iter().filter(|tool| !tool.enabled).map(|tool| tool.name.clone()).collect();
    disabled.retain(|name| !names.contains(name));
    if !on {
        disabled.extend(names.iter().cloned());
    }
    // Names the server no longer lists stay as they were.
    for name in &server.disabled_tools {
        if !live.iter().any(|tool| tool.name == *name) && !disabled.contains(name) {
            disabled.push(name.clone());
        }
    }
    disabled.sort();
    disabled.dedup();
    disabled
}

pub struct ExtensionsPage {
    store: Entity<Store>,
    cwd: Option<String>,
    status: Option<Status>,
    error: Option<SharedString>,
    guard: AsyncPageGuard,
    removing: Option<String>,
    /// Servers whose tool list is open.
    expanded: HashSet<String>,
    form: Option<Form>,
    import: Option<Import>,
    /// Counts file reads, so one that finishes after the import closed or another read started is dropped.
    file_read: usize,
    import_env: Option<McpImportEnvironment>,
    _subscriptions: Vec<Subscription>,
}

impl ExtensionsPage {
    pub fn new(store: Entity<Store>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let import_env = import_environment(cx);
        let subscriptions = vec![
            cx.subscribe_in(&Ide::global(cx), window, |this, _, event: &IdeEvent, window, cx| {
                if *event == IdeEvent::WorkspaceChanged {
                    this.guard.invalidate();
                    if let Some(status) = &mut this.status {
                        status.skills.clear();
                    }
                    this.read(window, cx);
                }
            }),
            cx.subscribe_in(&store, window, |this, _, event: &StoreEvent, window, cx| match event {
                StoreEvent::Host { name, data } if name == "extensions.changed" => {
                    if let Ok(status) = serde_json::from_value::<Status>(data.clone()) {
                        this.merge(status, cx);
                    }
                }
                StoreEvent::HostReconnected(_) => {
                    this.guard.invalidate();
                    this.import_env = import_environment(cx);
                    this.read(window, cx);
                }
                StoreEvent::PrefsChanged => cx.notify(),
                _ => {}
            }),
            // Refresh an open import preview when startup resolves or switches the execution target, without mutating
            // state during render.
            cx.observe(&store, |this, _, cx| {
                let env = import_environment(cx);
                if this.import_env != env {
                    this.import_env = env;
                    if this.import.is_some() {
                        this.refresh_preview(cx);
                    } else {
                        cx.notify();
                    }
                }
            }),
        ];
        let mut page = Self {
            store,
            cwd: None,
            status: None,
            error: None,
            guard: AsyncPageGuard::default(),
            removing: None,
            expanded: HashSet::new(),
            form: None,
            import: None,
            file_read: 0,
            import_env,
            _subscriptions: subscriptions,
        };
        page.read(window, cx);
        page
    }

    /// Server changes come back with the skills of whichever folder the Host read last; keep this project's.
    fn merge(&mut self, mut status: Status, cx: &mut Context<Self>) {
        if let Some(previous) = &self.status {
            status.skills = previous.skills.clone();
        }
        self.status = Some(status);
        // The configured servers changed, also while an import runs: servers saved since then are left out.
        self.refresh_preview(cx);
        cx.notify();
    }

    fn read(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.cwd = Ide::global(cx).read(cx).workspace.as_ref().map(|workspace| workspace.path.clone());
        let cwd = self.cwd.clone();
        let params = match &cwd {
            Some(cwd) => json!({ "cwd": cwd }),
            None => json!({}),
        };
        let call = self.store.read(cx).call("extensions.status", params);
        let generation = self.guard.begin_read();
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = call.await;
            let _ = this.update(cx, |this, cx| {
                let current = this.guard.end_read(generation);
                if !current || this.cwd != cwd {
                    cx.notify();
                    return;
                }
                match result.map(serde_json::from_value::<Status>) {
                    Ok(Ok(status)) => {
                        this.status = Some(status);
                        this.error = None;
                        this.refresh_preview(cx);
                    }
                    Ok(Err(error)) => this.error = Some(error.to_string().into()),
                    Err(error) => this.error = Some(crate::ui::error_text(cx, &error)),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Run one change; the answer is the new status, merged; `done` is told the change succeeded.
    fn change(
        &mut self,
        method: &str,
        params: Value,
        done: impl FnOnce(&mut Self, &mut Window, &mut Context<Self>) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let call = self.store.read(cx).call(method, params);
        let generation = self.guard.begin_write();
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = call.await;
            let _ = this.update_in(cx, |this, window, cx| {
                let current = this.guard.end_write(generation);
                if !current {
                    cx.notify();
                    return;
                }
                match result.map(serde_json::from_value::<Status>) {
                    Ok(Ok(status)) => {
                        this.merge(status, cx);
                        done(this, window, cx);
                    }
                    Ok(Err(error)) => window.push_notification(Notification::error(error.to_string()), cx),
                    Err(error) => window.push_notification(crate::ui::error_notification(cx, &error), cx),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn servers(&self) -> Vec<McpServer> {
        self.status.as_ref().map(|status| status.servers.clone()).unwrap_or_default()
    }

    fn toggle(&mut self, server: McpServer, enabled: bool, window: &mut Window, cx: &mut Context<Self>) {
        let previous = server.name.clone();
        self.change("extensions.saveServer", json!({ "server": McpServer { enabled, ..server }, "previousName": previous }), |_, _, _| {}, window, cx);
    }

    /// Turn some (or all, when `names` is `None`) of a server's tools on or off, looking up the live tool list on click
    /// rather than cloning it per tool row during render.
    fn toggle_tools(&mut self, server_name: &str, names: Option<&[String]>, on: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(status) = &self.status else { return };
        let Some(server) = status.servers.iter().find(|entry| entry.name == server_name).cloned() else { return };
        let live: Vec<McpTool> = status
            .status
            .iter()
            .find(|entry| entry.name == server_name)
            .map(|entry| entry.tools.clone())
            .unwrap_or_default();
        let all_names: Vec<String>;
        let target_names = match names {
            Some(names) => names,
            None => {
                all_names = live.iter().map(|tool| tool.name.clone()).collect();
                &all_names
            }
        };
        let disabled = next_disabled_tools(&server, &live, target_names, on);
        let previous = server.name.clone();
        let changed = McpServer { tools: Vec::new(), disabled_tools: disabled, ..server };
        self.change("extensions.saveServer", json!({ "server": changed, "previousName": previous }), |_, _, _| {}, window, cx);
    }

    /// The first click asks, the second removes.
    fn remove(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.removing.as_deref() != Some(name.as_str()) {
            self.removing = Some(name);
            cx.notify();
            return;
        }
        self.removing = None;
        let removed = name.clone();
        self.change("extensions.removeServer", json!({ "name": name }), move |this, window, cx| {
            if this.form.as_ref().is_some_and(|form| form.previous.as_deref() == Some(removed.as_str())) {
                this.form = None;
            }
            window.push_notification(trf(cx, "settings.mcpRemoved", &[("name", &removed)]).to_string(), cx);
        }, window, cx);
    }
}

impl Render for ExtensionsPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let skills = self.render_skills(cx);
        let servers = self.render_servers(cx);
        v_flex()
            .gap_4()
            .child(heading(cx, "settings.extensionsTitle", "settings.extensionsNote"))
            .when_some(self.error.clone(), |this, error| this.child(notice(cx, error, true)))
            .child(skills)
            .child(servers)
    }
}

#[cfg(test)]
mod tests {
    use ::core::prelude::v1::test;

    use super::*;

    #[test]
    fn deduplicates_non_adjacent_tools_and_computes_disabled_tools() {
        assert_eq!(server_form::normalize_tools(&["b".into(), "a".into(), "b".into(), "c".into(), "a".into()]), vec!["a", "b", "c"]);

        let server = McpServer {
            name: "demo".into(),
            enabled: true,
            transport: "stdio".into(),
            command: Some("demo".into()),
            args: None,
            env: None,
            url: None,
            headers: None,
            tools: Vec::new(),
            disabled_tools: vec!["stale_tool".into(), "t2".into()],
        };
        let live = vec![
            McpTool { name: "t1".into(), description: String::new(), enabled: true },
            McpTool { name: "t2".into(), description: String::new(), enabled: false },
        ];
        assert_eq!(next_disabled_tools(&server, &live, &["t1".into()], false), vec!["stale_tool", "t1", "t2"]);
        assert_eq!(next_disabled_tools(&server, &live, &["t2".into()], true), vec!["stale_tool"]);
    }
}
