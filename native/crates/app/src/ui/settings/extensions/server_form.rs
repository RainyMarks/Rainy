//! Manual MCP server form (and Windows-MCP desktop preset) inside Settings → Skills & MCP.

use std::collections::BTreeMap;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputState, Textarea, TextareaState};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, StyledExt as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use rainy_carrier::mcp_json;
use serde_json::json;

use super::{ExtensionsPage, McpServer, WINDOWS_MCP_DEPENDENCIES_BEFORE, WINDOWS_MCP_EXCLUDED_TOOLS, WINDOWS_MCP_VERSION};
use crate::ui::settings::models_extra::notice;
use crate::ui::{tr, trf};

/// Why a form cannot be saved: a message key and its values.
pub(super) type FormError = (&'static str, Vec<(&'static str, String)>);

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

/// Sort before deduplicating so non-adjacent duplicates are also removed cleanly.
pub(super) fn normalize_tools(raw: &[String]) -> Vec<String> {
    let mut tools = raw.to_vec();
    tools.sort();
    tools.dedup();
    tools
}

/// The form's fields as text.
pub(super) struct Form {
    pub previous: Option<String>,
    pub desktop: bool,
    pub transport: &'static str,
    pub name: Entity<InputState>,
    pub command: Entity<InputState>,
    pub args: Entity<TextareaState>,
    pub env: Entity<TextareaState>,
    pub url: Entity<InputState>,
    pub headers: Entity<TextareaState>,
    pub tools: Vec<String>,
    pub disabled_tools: Vec<String>,
    pub error: Option<SharedString>,
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
        let tools = normalize_tools(&self.tools);
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
                disabled_tools: self.disabled_tools.clone(),
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
            disabled_tools: self.disabled_tools.clone(),
        })
    }
}

impl ExtensionsPage {
    pub(super) fn open_form(&mut self, server: Option<McpServer>, desktop: bool, window: &mut Window, cx: &mut Context<Self>) {
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
            disabled_tools: Vec::new(),
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
            disabled_tools: server.disabled_tools.clone(),
            error: None,
        });
        self.removing = None;
        self.import = None;
        self.file_read += 1;
        cx.notify();
    }

    /// Whether pasted text should start an import: MCP JSON pasted into an empty new server form is imported instead of
    /// landing in one field, so nothing typed is lost.
    fn imports_paste(&self, text: &str, cx: &App) -> bool {
        let Some(form) = &self.form else { return false };
        let typed = [&form.name, &form.command, &form.url].iter().any(|state| !mcp_json::is_blank(&state.read(cx).value()))
            || [&form.args, &form.env, &form.headers].iter().any(|state| !mcp_json::is_blank(&state.read(cx).value()));
        form.previous.is_none() && !typed && mcp_json::looks_like_mcp_json(text)
    }

    /// The paste hook of the server form's fields.
    fn paste_handler(cx: &Context<Self>) -> impl Fn(&ClipboardItem, &mut Window, &mut App) -> bool + 'static {
        let page = cx.entity().downgrade();
        move |item, window, cx| {
            let Some(text) = item.text() else { return false };
            if !page.read_with(cx, |this, cx| this.imports_paste(&text, cx)).unwrap_or(false) {
                return false;
            }
            let page = page.clone();
            window.defer(cx, move |window, cx| {
                let _ = page.update(cx, |this, cx| this.start_import(text, window, cx));
            });
            true
        }
    }

    /// The desktop preset: Windows-MCP through uvx, from WSL through `cmd.exe`.
    pub(super) fn open_desktop(&mut self, wsl: bool, window: &mut Window, cx: &mut Context<Self>) {
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
            disabled_tools: Vec::new(),
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

    pub(super) fn render_form(&self, form: &Form, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let busy = self.guard.busy();
        let label = |key: &str| div().text_xs().text_color(theme.muted_foreground).child(tr(cx, key));
        let title = match &form.previous {
            None => tr(cx, "settings.mcpFormAdd"),
            Some(name) => trf(cx, "settings.mcpFormEdit", &[("name", name)]),
        };
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
            .child(Input::new(&form.name).small().on_paste(Self::paste_handler(cx)))
            .child(label("settings.mcpTransport"))
            .child(h_flex().gap_1().child(transport_button("stdio", "settings.mcpStdio")).child(transport_button("streamable-http", "settings.mcpHttp")))
            .when(transport == "stdio", |this| {
                this.child(label("settings.mcpCommand"))
                    .child(Input::new(&form.command).small().on_paste(Self::paste_handler(cx)))
                    .child(label("settings.mcpArgs"))
                    .child(Textarea::new(&form.args).on_paste(Self::paste_handler(cx)))
                    .child(label("settings.mcpEnv"))
                    .child(Textarea::new(&form.env).on_paste(Self::paste_handler(cx)))
            })
            .when(transport == "streamable-http", |this| {
                this.child(label("settings.mcpUrl"))
                    .child(Input::new(&form.url).small().on_paste(Self::paste_handler(cx)))
                    .child(label("settings.mcpHeaders"))
                    .child(Textarea::new(&form.headers).on_paste(Self::paste_handler(cx)))
            })
            .child(div().text_xs().text_color(theme.muted_foreground).child(tr(cx, "settings.mcpToolsInList")))
            .when_some(form.error.clone(), |this, error| this.child(notice(cx, error, true)))
            .child(
                h_flex()
                    .gap_2()
                    .child(Button::new("mcp-save").primary().small().label(tr(cx, "settings.settingsSave")).disabled(busy).on_click(cx.listener(|this, _, window, cx| this.save(window, cx))))
                    .child(Button::new("mcp-cancel").outline().small().label(tr(cx, "settings.settingsCancel")).disabled(busy).on_click(cx.listener(|this, _, _, cx| {
                        this.form = None;
                        cx.notify();
                    }))),
            )
            .into_any_element()
    }
}
