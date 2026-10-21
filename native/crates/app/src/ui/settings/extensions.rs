//! Skills & MCP (`ExtensionsSection.tsx`): discovered skills, and the MCP servers every chat may use.

use std::collections::BTreeMap;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputState, Textarea, TextareaState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::tag::Tag;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, StyledExt as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use rainy_carrier::ExecutionTarget;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::heading;
use super::models_extra::{card, notice};
use crate::store::ide::Ide;
use crate::store::{Store, StoreEvent};
use crate::ui::{tr, trf};

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
    #[serde(default)]
    pub tools: Vec<String>,
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

/// Why a form cannot be saved: a message key and its values.
type FormError = (&'static str, Vec<(&'static str, String)>);

fn valid_name(name: &str) -> bool {
    (1..=32).contains(&name.chars().count()) && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn env_line(line: &str) -> Option<(String, String)> {
    let (key, value) = line.split_once('=')?;
    let mut chars = key.chars();
    let first = chars.next()?;
    ((first.is_ascii_alphabetic() || first == '_') && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')).then(|| (key.to_owned(), value.to_owned()))
}

fn header_line(line: &str) -> Option<(String, String)> {
    let (key, value) = line.split_once(':')?;
    let token = !key.is_empty() && key.chars().all(|c| c.is_ascii_alphanumeric() || "!#$%&'*+.^_`|~-".contains(c));
    let value = value.trim_start_matches([' ', '\t']);
    let clean = value.chars().all(|c| c == '\t' || !(c.is_control()));
    (token && clean).then(|| (key.to_owned(), value.trim_end().to_owned()))
}

/// The form's fields as text.
struct Form {
    previous: Option<String>,
    desktop: bool,
    transport: &'static str,
    name: Entity<InputState>,
    command: Entity<InputState>,
    args: Entity<TextareaState>,
    env: Entity<TextareaState>,
    url: Entity<InputState>,
    headers: Entity<TextareaState>,
    tools: Vec<String>,
    error: Option<SharedString>,
}

impl Form {
    fn parse(&self, enabled: bool, others: &[String], cx: &App) -> Result<McpServer, FormError> {
        let lines = |state: &Entity<TextareaState>| -> Vec<String> { state.read(cx).value().split('\n').map(|line| line.trim().to_owned()).collect() };
        let name = self.name.read(cx).value().trim().to_owned();
        if !valid_name(&name) {
            return Err(("settings.mcpNameInvalid", vec![]));
        }
        if others.contains(&name) {
            return Err(("settings.mcpNameTaken", vec![("name", name)]));
        }
        let mut tools = self.tools.clone();
        tools.dedup();
        if self.transport == "streamable-http" {
            let url = self.url.read(cx).value().trim().to_owned();
            let host = url.strip_prefix("https://").or_else(|| url.strip_prefix("http://")).map(|rest| rest.split(['/', '?', '#']).next().unwrap_or_default());
            if host.is_none_or(str::is_empty) {
                return Err(("settings.mcpUrlRequired", vec![]));
            }
            let mut headers = BTreeMap::new();
            for (index, line) in lines(&self.headers).iter().enumerate() {
                if line.is_empty() {
                    continue;
                }
                let Some((key, value)) = header_line(line) else { return Err(("settings.mcpHeaderInvalid", vec![("line", (index + 1).to_string())])) };
                if headers.keys().any(|other: &String| other.eq_ignore_ascii_case(&key)) {
                    return Err(("settings.mcpHeaderDuplicate", vec![("name", key)]));
                }
                headers.insert(key, value);
            }
            return Ok(McpServer {
                name,
                enabled,
                transport: "streamable-http".into(),
                command: None,
                args: None,
                env: None,
                url: Some(url),
                headers: (!headers.is_empty()).then_some(headers),
                tools,
            });
        }
        let command = self.command.read(cx).value().trim().to_owned();
        if command.is_empty() {
            return Err(("settings.mcpCommandRequired", vec![]));
        }
        let mut env = BTreeMap::new();
        for (index, line) in lines(&self.env).iter().enumerate() {
            if line.is_empty() {
                continue;
            }
            let Some((key, value)) = env_line(line) else { return Err(("settings.mcpEnvInvalid", vec![("line", (index + 1).to_string())])) };
            env.insert(key, value);
        }
        let args: Vec<String> = lines(&self.args).into_iter().filter(|line| !line.is_empty()).collect();
        Ok(McpServer {
            name,
            enabled,
            transport: "stdio".into(),
            command: Some(command),
            args: Some(args),
            env: (!env.is_empty()).then_some(env),
            url: None,
            headers: None,
            tools,
        })
    }
}

pub struct ExtensionsPage {
    store: Entity<Store>,
    status: Option<Status>,
    error: Option<SharedString>,
    busy: bool,
    removing: Option<String>,
    form: Option<Form>,
    _subscriptions: Vec<Subscription>,
}

impl ExtensionsPage {
    pub fn new(store: Entity<Store>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let subscriptions = vec![cx.subscribe(&store, |this, _, event: &StoreEvent, cx| {
            if let StoreEvent::Host { name, data } = event
                && name == "extensions.changed"
                && let Ok(status) = serde_json::from_value::<Status>(data.clone())
            {
                this.merge(status, cx);
            }
        })];
        let mut page = Self { store, status: None, error: None, busy: false, removing: None, form: None, _subscriptions: subscriptions };
        page.read(window, cx);
        page
    }

    /// Server changes come back with the skills of whichever folder the Host read last; keep this project's.
    fn merge(&mut self, mut status: Status, cx: &mut Context<Self>) {
        if let Some(previous) = &self.status {
            status.skills = previous.skills.clone();
        }
        self.status = Some(status);
        cx.notify();
    }

    fn read(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let cwd = Ide::global(cx).read(cx).workspace.as_ref().map(|workspace| workspace.path.clone());
        let params = match cwd {
            Some(cwd) => json!({ "cwd": cwd }),
            None => json!({}),
        };
        let call = self.store.read(cx).call("extensions.status", params);
        cx.spawn_in(window, async move |this, cx| {
            let result = call.await;
            let _ = this.update(cx, |this, cx| {
                match result.map(serde_json::from_value::<Status>) {
                    Ok(Ok(status)) => {
                        this.status = Some(status);
                        this.error = None;
                    }
                    Ok(Err(error)) => this.error = Some(error.to_string().into()),
                    Err(error) => this.error = Some(error.message.into()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Run one change; the answer is the new status, merged; `done` is told the change succeeded.
    fn change(&mut self, method: &str, params: Value, done: impl FnOnce(&mut Self, &mut Window, &mut Context<Self>) + 'static, window: &mut Window, cx: &mut Context<Self>) {
        let call = self.store.read(cx).call(method, params);
        self.busy = true;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = call.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                match result.map(serde_json::from_value::<Status>) {
                    Ok(Ok(status)) => {
                        this.merge(status, cx);
                        done(this, window, cx);
                    }
                    Ok(Err(error)) => window.push_notification(Notification::error(error.to_string()), cx),
                    Err(error) => window.push_notification(Notification::error(error.message), cx),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn servers(&self) -> Vec<McpServer> {
        self.status.as_ref().map(|status| status.servers.clone()).unwrap_or_default()
    }

    fn open_form(&mut self, server: Option<McpServer>, desktop: bool, window: &mut Window, cx: &mut Context<Self>) {
        let server = server.unwrap_or(McpServer {
            name: String::new(),
            enabled: true,
            transport: "stdio".into(),
            command: None,
            args: None,
            env: None,
            url: None,
            headers: None,
            tools: Vec::new(),
        });
        let input = |value: String, window: &mut Window, cx: &mut Context<Self>| cx.new(|cx| InputState::new(window, cx).default_value(value));
        let area = |value: String, window: &mut Window, cx: &mut Context<Self>| cx.new(|cx| TextareaState::new(window, cx).auto_grow(2, 8).default_value(value));
        let previous = self.servers().iter().any(|known| known.name == server.name && !server.name.is_empty()).then(|| server.name.clone());
        let env = server.env.iter().flatten().map(|(key, value)| format!("{key}={value}")).collect::<Vec<_>>().join("\n");
        let headers = server.headers.iter().flatten().map(|(key, value)| format!("{key}: {value}")).collect::<Vec<_>>().join("\n");
        self.form = Some(Form {
            previous,
            desktop,
            transport: if server.transport == "streamable-http" { "streamable-http" } else { "stdio" },
            name: input(server.name.clone(), window, cx),
            command: input(server.command.clone().unwrap_or_default(), window, cx),
            args: area(server.args.clone().unwrap_or_default().join("\n"), window, cx),
            env: area(env, window, cx),
            url: input(server.url.clone().unwrap_or_default(), window, cx),
            headers: area(headers, window, cx),
            tools: server.tools.clone(),
            error: None,
        });
        self.removing = None;
        cx.notify();
    }

    /// The desktop preset: Windows-MCP through uvx, from WSL through `cmd.exe`.
    fn open_desktop(&mut self, wsl: bool, window: &mut Window, cx: &mut Context<Self>) {
        let taken: Vec<String> = self.servers().into_iter().map(|server| server.name).collect();
        let mut name = "windows".to_owned();
        let mut index = 2;
        while taken.contains(&name) {
            name = format!("windows-{index}");
            index += 1;
        }
        let mut parts: Vec<String> = vec![
            "uvx".into(),
            "--python".into(),
            "3.14".into(),
            "--exclude-newer".into(),
            WINDOWS_MCP_DEPENDENCIES_BEFORE.into(),
            format!("windows-mcp=={WINDOWS_MCP_VERSION}"),
            "serve".into(),
            "--exclude-tools".into(),
            WINDOWS_MCP_EXCLUDED_TOOLS.into(),
        ];
        if wsl {
            parts.splice(0..0, ["cmd.exe".to_owned(), "/c".to_owned()]);
        }
        let command = parts.remove(0);
        let mut env = BTreeMap::from([("ANONYMIZED_TELEMETRY".to_owned(), "false".to_owned())]);
        if wsl {
            // WSLENV carries the variable from the Linux Host to the Windows program.
            env.insert("WSLENV".into(), "ANONYMIZED_TELEMETRY".into());
        }
        let server = McpServer {
            name,
            enabled: true,
            transport: "stdio".into(),
            command: Some(command),
            args: Some(parts),
            env: Some(env),
            url: None,
            headers: None,
            tools: Vec::new(),
        };
        self.open_form(Some(server), true, window, cx);
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let servers = self.servers();
        let Some(form) = &mut self.form else { return };
        // The list's switch may have changed the server while the form was open; the form does not edit that state.
        let enabled = servers.iter().find(|server| Some(&server.name) == form.previous.as_ref()).is_none_or(|server| server.enabled);
        let others: Vec<String> = servers.iter().filter(|server| Some(&server.name) != form.previous.as_ref()).map(|server| server.name.clone()).collect();
        match form.parse(enabled, &others, cx) {
            Err((key, vars)) => {
                let vars: Vec<(&str, &dyn std::fmt::Display)> = vars.iter().map(|(name, value)| (*name, value as &dyn std::fmt::Display)).collect();
                form.error = Some(trf(cx, key, &vars));
                cx.notify();
            }
            Ok(server) => {
                form.error = None;
                let mut params = json!({ "server": server });
                if let Some(previous) = &form.previous {
                    params["previousName"] = json!(previous);
                }
                self.change("extensions.saveServer", params, |this, window, cx| {
                    this.form = None;
                    window.push_notification(tr(cx, "settings.settingsSaved").to_string(), cx);
                }, window, cx);
            }
        }
    }

    fn toggle(&mut self, server: McpServer, enabled: bool, window: &mut Window, cx: &mut Context<Self>) {
        let previous = server.name.clone();
        self.change("extensions.saveServer", json!({ "server": McpServer { enabled, ..server }, "previousName": previous }), |_, _, _| {}, window, cx);
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

    fn render_skills(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let project = Ide::global(cx).read(cx).workspace.as_ref().map(|workspace| workspace.path.clone());
        let name = tr(cx, "settings.skillsName");
        let root = project.unwrap_or_else(|| tr(cx, "settings.skillsProject").to_string());
        let separator = if root.contains('\\') && !root.contains('/') { '\\' } else { '/' };
        let project_path = [root.trim_end_matches(['\\', '/']), ".rainy", "skills", &name, "SKILL.md"].join(&separator.to_string());
        let user_path = format!("~/.rainy-agent/skills/{name}/SKILL.md");
        let mut body = vec![div().text_xs().text_color(theme.muted_foreground).child(trf(cx, "settings.skillsHint", &[("project", &project_path), ("user", &user_path)])).into_any_element()];
        match &self.status {
            None => body.push(h_flex().child(Spinner::new().small()).into_any_element()),
            Some(status) if status.skills.is_empty() => body.push(div().text_sm().text_color(theme.muted_foreground).child(tr(cx, "settings.settingsNoSkills")).into_any_element()),
            Some(status) => {
                for skill in &status.skills {
                    let scope = tr(cx, if skill.scope == "project" { "settings.skillScopeProject" } else { "settings.skillScopeUser" });
                    body.push(
                        v_flex()
                            .id(SharedString::from(format!("skill-{}", skill.id)))
                            .gap_0p5()
                            .py_1()
                            .child(h_flex().gap_2().items_center().text_sm().child(skill.name.clone()).child(Tag::info().small().child(scope)))
                            .when(!skill.description.is_empty(), |this| this.child(div().text_xs().text_color(theme.muted_foreground).child(skill.description.clone())))
                            .child(div().text_xs().font_family(theme.mono_font_family.clone()).text_color(theme.muted_foreground).child(skill.path.clone()))
                            .into_any_element(),
                    );
                }
            }
        }
        body.insert(
            0,
            h_flex()
                .justify_end()
                .child(Button::new("skills-refresh").outline().small().label(tr(cx, "settings.settingsRefresh")).disabled(self.busy).on_click(cx.listener(|this, _, window, cx| this.read(window, cx))))
                .into_any_element(),
        );
        card(cx, "settings.skillsTitle", None, body)
    }

    fn render_servers(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let Some(status) = &self.status else { return card(cx, "settings.mcpTitle", None, vec![]) };
        // Windows-MCP drives this PC's desktop, which a Host on an SSH server cannot reach.
        let target = Store::global(cx).read(cx).target.clone();
        let desktop = match target {
            None | Some(ExecutionTarget::Windows { .. }) => Some(false),
            Some(ExecutionTarget::Wsl { .. }) => Some(true),
            Some(ExecutionTarget::Ssh { .. }) => None,
        };
        let has_desktop = status.servers.iter().any(is_windows_mcp);
        let actions = h_flex()
            .gap_2()
            .justify_end()
            .when_some(desktop.filter(|_| !has_desktop), |this, wsl| {
                this.child(Button::new("mcp-desktop").outline().small().label(tr(cx, "settings.mcpAddDesktop")).disabled(self.busy).on_click(cx.listener(
                    move |this, _, window, cx| this.open_desktop(wsl, window, cx),
                )))
            })
            .child(Button::new("mcp-add").outline().small().label(tr(cx, "settings.mcpAdd")).disabled(self.busy).on_click(cx.listener(|this, _, window, cx| {
                this.open_form(None, false, window, cx)
            })));
        let mut body = vec![actions.into_any_element()];
        if status.servers.is_empty() {
            body.push(div().text_sm().text_color(theme.muted_foreground).child(tr(cx, "settings.mcpEmpty")).into_any_element());
        }
        for (index, server) in status.servers.iter().enumerate() {
            let live = status.status.iter().find(|entry| entry.name == server.name);
            let state = if server.enabled { live.map_or("connecting", |entry| entry.state.as_str()) } else { "disabled" };
            let (label, tag) = match state {
                "ready" => ("settings.mcpStateReady", Tag::success()),
                "error" => ("settings.mcpStateError", Tag::danger()),
                "disabled" => ("settings.mcpStateDisabled", Tag::secondary()),
                _ => ("settings.mcpStateConnecting", Tag::warning()),
            };
            let line = if server.transport == "stdio" {
                server.command.iter().chain(server.args.iter().flatten()).cloned().collect::<Vec<_>>().join(" ")
            } else {
                server.url.clone().unwrap_or_default()
            };
            let tools = live.filter(|entry| !entry.tools.is_empty()).map(|entry| {
                trf(cx, "settings.mcpTools", &[("enabled", &entry.tools.iter().filter(|tool| tool.enabled).count()), ("total", &entry.tools.len())])
            });
            let error = live.filter(|_| state == "error").and_then(|entry| entry.error.clone());
            let (toggled, edited, removed) = (server.clone(), server.clone(), server.name.clone());
            let desktop_server = is_windows_mcp(server);
            let confirming = self.removing.as_deref() == Some(server.name.as_str());
            body.push(
                h_flex()
                    .gap_3()
                    .py_2()
                    .border_t_1()
                    .border_color(theme.border)
                    .items_start()
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_0p5()
                            .child(
                                h_flex()
                                    .gap_2()
                                    .items_center()
                                    .text_sm()
                                    .child(server.name.clone())
                                    .child(Tag::secondary().outline().small().child(if server.transport == "stdio" { "stdio" } else { "HTTP" }))
                                    .child(tag.small().child(tr(cx, label))),
                            )
                            .child(div().text_xs().font_family(theme.mono_font_family.clone()).text_color(theme.muted_foreground).overflow_hidden().text_ellipsis().child(line))
                            .when_some(tools, |this, tools| this.child(div().text_xs().text_color(theme.muted_foreground).child(tools)))
                            .when_some(error, |this, error| this.child(div().text_xs().text_color(theme.danger).child(error))),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(Switch::new(("mcp-enabled", index)).checked(server.enabled).disabled(self.busy).on_click(cx.listener(
                                move |this, checked: &bool, window, cx| this.toggle(toggled.clone(), *checked, window, cx),
                            )))
                            .child(Button::new(("mcp-edit", index)).small().outline().label(tr(cx, "settings.mcpEdit")).disabled(self.busy).on_click(cx.listener(
                                move |this, _, window, cx| this.open_form(Some(edited.clone()), desktop_server, window, cx),
                            )))
                            .child(
                                Button::new(("mcp-remove", index))
                                    .small()
                                    .map(|button| if confirming { button.danger() } else { button.outline() })
                                    .label(tr(cx, if confirming { "settings.mcpRemoveConfirm" } else { "settings.mcpRemove" }))
                                    .disabled(self.busy)
                                    .on_click(cx.listener(move |this, _, window, cx| this.remove(removed.clone(), window, cx))),
                            ),
                    )
                    .into_any_element(),
            );
        }
        if let Some(form) = &self.form {
            body.push(self.render_form(form, status, cx));
        }
        card(cx, "settings.mcpTitle", None, body)
    }

    fn render_form(&self, form: &Form, status: &Status, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let label = |key: &str| div().text_xs().text_color(theme.muted_foreground).child(tr(cx, key));
        let title = match &form.previous {
            None => tr(cx, "settings.mcpFormAdd"),
            Some(name) => trf(cx, "settings.mcpFormEdit", &[("name", name)]),
        };
        let known: Vec<McpTool> = form.previous.as_ref().and_then(|name| status.status.iter().find(|entry| entry.name == *name)).map(|entry| entry.tools.clone()).unwrap_or_default();
        let mut names: Vec<String> = known.iter().map(|tool| tool.name.clone()).collect();
        for tool in &form.tools {
            if !names.contains(tool) {
                names.push(tool.clone());
            }
        }
        let transport = form.transport;
        let transport_button = |value: &'static str, key: &str| {
            let button = Button::new(SharedString::from(format!("mcp-transport-{value}"))).small().label(tr(cx, key));
            let button = if transport == value { button.primary() } else { button.outline() };
            button.on_click(cx.listener(move |this, _, _, cx| {
                if let Some(form) = &mut this.form {
                    form.transport = value;
                    cx.notify();
                }
            }))
        };
        v_flex()
            .gap_2()
            .mt_2()
            .p_3()
            .rounded(theme.radius)
            .border_1()
            .border_color(theme.border)
            .child(div().text_sm().font_semibold().child(title))
            .when(form.desktop, |this| {
                this.child(div().text_xs().text_color(theme.muted_foreground).child(tr(cx, "settings.mcpDesktopNote")))
                    .child(notice(cx, tr(cx, "settings.mcpDesktopSetup"), false))
            })
            .child(label("settings.mcpName"))
            .child(Input::new(&form.name).small())
            .child(label("settings.mcpTransport"))
            .child(h_flex().gap_1().child(transport_button("stdio", "settings.mcpStdio")).child(transport_button("streamable-http", "settings.mcpHttp")))
            .when(transport == "stdio", |this| {
                this.child(label("settings.mcpCommand"))
                    .child(Input::new(&form.command).small())
                    .child(label("settings.mcpArgs"))
                    .child(Textarea::new(&form.args))
                    .child(label("settings.mcpEnv"))
                    .child(Textarea::new(&form.env))
            })
            .when(transport == "streamable-http", |this| {
                this.child(label("settings.mcpUrl")).child(Input::new(&form.url).small()).child(label("settings.mcpHeaders")).child(Textarea::new(&form.headers))
            })
            .child(div().mt_1().text_sm().font_semibold().child(tr(cx, "settings.mcpToolsTitle")))
            .child(div().text_xs().text_color(theme.muted_foreground).child(tr(cx, if names.is_empty() { "settings.mcpToolsUnknown" } else { "settings.mcpToolsHint" })))
            .child(h_flex().flex_wrap().gap_3().children(names.into_iter().enumerate().map(|(index, name)| {
                let checked = form.tools.contains(&name);
                let tool = name.clone();
                Checkbox::new(("mcp-tool", index)).label(name).checked(checked).on_click(cx.listener(move |this, checked: &bool, _, cx| {
                    if let Some(form) = &mut this.form {
                        form.tools.retain(|known| *known != tool);
                        if *checked {
                            form.tools.push(tool.clone());
                        }
                        cx.notify();
                    }
                }))
            })))
            .when_some(form.error.clone(), |this, error| this.child(notice(cx, error, true)))
            .child(
                h_flex()
                    .gap_2()
                    .child(Button::new("mcp-save").primary().small().label(tr(cx, "settings.settingsSave")).disabled(self.busy).on_click(cx.listener(|this, _, window, cx| this.save(window, cx))))
                    .child(Button::new("mcp-cancel").outline().small().label(tr(cx, "settings.settingsCancel")).disabled(self.busy).on_click(cx.listener(|this, _, _, cx| {
                        this.form = None;
                        cx.notify();
                    }))),
            )
            .into_any_element()
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
