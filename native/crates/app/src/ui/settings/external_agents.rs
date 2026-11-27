//! External CLI agents (Codex CLI, Antigravity / `agy acp`, and custom ACP servers) inside Settings → Models.

use std::collections::HashMap;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _, StyledExt as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use rainy_carrier::ExecutionTarget;
use rainy_protocol::{ExternalAgentConfig, ExternalAgentStatus, ExternalStatusResponse};
use serde_json::{Value, json};

use super::models_extra::{card, notice};
use crate::store::{Store, StoreEvent};
use crate::ui::{tr, trf};

/// Editing fields for one external agent entry (binary path and extra arguments), tracking the last server-synced values
/// so background status refreshes never clobber in-progress user edits.
struct AgentDraft {
    binary: Entity<InputState>,
    args: Entity<InputState>,
    synced_binary: String,
    synced_args: String,
}

fn should_sync_draft(current: &str, synced: &str, force: bool) -> bool {
    force || current == synced
}

/// Manages detection, configuration, custom ACP creation, and OS-aware install/login guides for external agents.
pub struct ExternalAgentsCard {
    store: Entity<Store>,
    agents: Vec<ExternalAgentStatus>,
    drafts: HashMap<String, AgentDraft>,
    expanded: Option<String>,
    show_custom_form: bool,
    custom_name: Entity<InputState>,
    custom_binary: Entity<InputState>,
    custom_args: Entity<InputState>,
    probing: bool,
    saving: bool,
    error: Option<SharedString>,
    _subscriptions: Vec<Subscription>,
}

/// Split space-separated arguments while supporting simple single/double quoted segments.
pub(crate) fn parse_args(raw: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    for ch in raw.chars() {
        match quote {
            Some(q) if ch == q => quote = None,
            Some(_) => current.push(ch),
            None if ch == '"' || ch == '\'' => quote = Some(ch),
            None if ch.is_whitespace() => {
                if !current.is_empty() {
                    out.push(std::mem::take(&mut current));
                }
            }
            None => current.push(ch),
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

fn format_args(args: &[String]) -> String {
    args.iter()
        .map(|arg| if arg.chars().any(char::is_whitespace) { format!("\"{arg}\"") } else { arg.clone() })
        .collect::<Vec<_>>()
        .join(" ")
}

fn agent_key(agent: &ExternalAgentStatus) -> String {
    agent.kind.clone()
}

fn is_custom_kind(kind: &str) -> bool {
    kind != "codex" && kind != "antigravity"
}

/// Generate a unique valid `kind` slug (1..=32 ASCII lowercase/digits/hyphens, starting with a letter)
/// that avoids built-in kinds (`codex`, `antigravity`) and any already-configured external agent.
fn make_custom_kind(name: &str, binary: &str, existing: &[ExternalAgentStatus]) -> String {
    let source = if !name.trim().is_empty() { name } else { binary };
    let mut base = String::new();
    let mut last_dash = false;
    for ch in source.chars() {
        let lower = ch.to_ascii_lowercase();
        if lower.is_ascii_lowercase() || lower.is_ascii_digit() {
            base.push(lower);
            last_dash = false;
        } else if !base.is_empty() && !last_dash {
            base.push('-');
            last_dash = true;
        }
    }
    while base.ends_with('-') {
        base.pop();
    }
    if base.is_empty() || !base.as_bytes()[0].is_ascii_lowercase() {
        base = if base.is_empty() { "acp".to_owned() } else { format!("acp-{base}") };
    }
    if base.len() > 28 {
        base.truncate(28);
        while base.ends_with('-') {
            base.pop();
        }
    }
    if base == "codex" || base == "antigravity" {
        base = format!("acp-{base}");
    }
    let taken = |candidate: &str| {
        candidate == "codex"
            || candidate == "antigravity"
            || existing.iter().any(|agent| agent.kind == candidate)
    };
    if !taken(&base) {
        return base;
    }
    for suffix in 2..=999 {
        let suffix_str = format!("-{suffix}");
        let max_prefix = 32usize.saturating_sub(suffix_str.len());
        let prefix = base[..base.len().min(max_prefix)].trim_end_matches('-');
        let candidate = format!("{prefix}{suffix_str}");
        if !taken(&candidate) {
            return candidate;
        }
    }
    format!("acp-{}", existing.len() + 1)
}

fn default_hints(kind: &str, linux: bool) -> (Option<&'static str>, Option<&'static str>) {
    match kind {
        "codex" => (Some("npm i -g @openai/codex"), Some("codex login")),
        "antigravity" => (
            Some(if linux {
                "curl -fsSL https://antigravity.google/cli/install.sh | bash"
            } else {
                "irm https://antigravity.google/cli/install.ps1 | iex"
            }),
            Some("agy acp --login"),
        ),
        _ => (None, None),
    }
}

impl ExternalAgentsCard {
    pub fn new(store: Entity<Store>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let custom_name = cx.new(|cx| InputState::new(window, cx).placeholder(tr(cx, "external.customNamePlaceholder")));
        let custom_binary = cx.new(|cx| InputState::new(window, cx).placeholder(tr(cx, "external.customBinaryPlaceholder")));
        let custom_args = cx.new(|cx| InputState::new(window, cx).placeholder(tr(cx, "external.customArgsPlaceholder")));
        let subscriptions = vec![cx.subscribe_in(&store, window, |this, _, event: &StoreEvent, window, cx| match event {
            StoreEvent::Host { name, data } if name == "external.changed" => {
                if let Ok(resp) = serde_json::from_value::<ExternalStatusResponse>(data.clone()) {
                    this.apply_agents(resp.agents, None, window, cx);
                }
            }
            StoreEvent::HostReconnected(_) => this.load_status(window, cx),
            StoreEvent::PrefsChanged => {
                let name_ph = tr(cx, "external.customNamePlaceholder");
                let bin_ph = tr(cx, "external.customBinaryPlaceholder");
                let args_ph = tr(cx, "external.customArgsPlaceholder");
                this.custom_name.update(cx, |input, cx| input.set_placeholder(name_ph, window, cx));
                this.custom_binary.update(cx, |input, cx| input.set_placeholder(bin_ph, window, cx));
                this.custom_args.update(cx, |input, cx| input.set_placeholder(args_ph, window, cx));
                let draft_bin_ph = tr(cx, "external.binaryPlaceholder");
                let draft_args_ph = tr(cx, "external.argsPlaceholder");
                for draft in this.drafts.values() {
                    let bin_ph = draft_bin_ph.clone();
                    let args_ph = draft_args_ph.clone();
                    draft.binary.update(cx, |input, cx| input.set_placeholder(bin_ph, window, cx));
                    draft.args.update(cx, |input, cx| input.set_placeholder(args_ph, window, cx));
                }
                cx.notify();
            }
            _ => {}
        })];
        let initial = store.read(cx).external_agents.clone();
        let mut card = Self {
            store,
            agents: initial,
            drafts: HashMap::new(),
            expanded: None,
            show_custom_form: false,
            custom_name,
            custom_binary,
            custom_args,
            probing: false,
            saving: false,
            error: None,
            _subscriptions: subscriptions,
        };
        card.load_status(window, cx);
        card
    }

    /// Ask the Host to read the usage limits of every installed agent that has them again.
    fn refresh_quotas(&self, force: bool, cx: &mut Context<Self>) {
        for agent in self.agents.iter().filter(|agent| agent.enabled && agent.installed) {
            crate::ui::quota::refresh(&self.store, &agent.kind, force, cx);
        }
    }

    fn apply_agents(&mut self, agents: Vec<ExternalAgentStatus>, force_sync_kind: Option<&str>, window: &mut Window, cx: &mut Context<Self>) {
        self.agents = agents.clone();
        self.store.update(cx, |store, cx| {
            store.external_agents = agents;
            cx.notify();
        });
        for agent in &self.agents {
            let key = agent_key(agent);
            if let Some(draft) = self.drafts.get_mut(&key) {
                let binary = agent.binary.clone().unwrap_or_default();
                let args = format_args(&agent.args);
                let force = force_sync_kind == Some(key.as_str());
                let current_binary = draft.binary.read(cx).value().to_string();
                if should_sync_draft(&current_binary, &draft.synced_binary, force) {
                    if current_binary != binary {
                        draft.binary.update(cx, |input, cx| input.set_value(binary.clone(), window, cx));
                    }
                    draft.synced_binary = binary;
                }
                let current_args = draft.args.read(cx).value().to_string();
                if should_sync_draft(&current_args, &draft.synced_args, force) {
                    if current_args != args {
                        draft.args.update(cx, |input, cx| input.set_value(args.clone(), window, cx));
                    }
                    draft.synced_args = args;
                }
            }
        }
        cx.notify();
    }

    fn ensure_draft(&mut self, agent: &ExternalAgentStatus, window: &mut Window, cx: &mut Context<Self>) {
        let key = agent_key(agent);
        if self.drafts.contains_key(&key) {
            return;
        }
        let binary_placeholder = tr(cx, "external.binaryPlaceholder");
        let args_placeholder = tr(cx, "external.argsPlaceholder");
        let binary_val = agent.binary.clone().unwrap_or_default();
        let args_val = format_args(&agent.args);
        let synced_binary = binary_val.clone();
        let synced_args = args_val.clone();
        let binary = cx.new(|cx| InputState::new(window, cx).placeholder(binary_placeholder).default_value(binary_val));
        let args = cx.new(|cx| InputState::new(window, cx).placeholder(args_placeholder).default_value(args_val));
        self.drafts.insert(key, AgentDraft { binary, args, synced_binary, synced_args });
    }

    fn load_status(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let call = self.store.read(cx).call("external.status", json!({}));
        cx.spawn_in(window, async move |this, cx| {
            let result = call.await;
            let _ = this.update_in(cx, |this, window, cx| match result.map(serde_json::from_value::<ExternalStatusResponse>) {
                Ok(Ok(resp)) => {
                    this.error = None;
                    this.apply_agents(resp.agents, None, window, cx);
                    this.refresh_quotas(false, cx);
                }
                Ok(Err(err)) => {
                    this.error = Some(err.to_string().into());
                    cx.notify();
                }
                Err(err) => {
                    this.error = Some(crate::ui::error_text(cx, &err));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn reprobe(&mut self, kind: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        if self.probing {
            return;
        }
        self.probing = true;
        self.error = None;
        cx.notify();
        let params = match kind {
            Some(kind) => json!({ "kind": kind }),
            None => json!({}),
        };
        let call = self.store.read(cx).call("external.probe", params);
        cx.spawn_in(window, async move |this, cx| {
            let result = call.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.probing = false;
                match result.map(serde_json::from_value::<ExternalStatusResponse>) {
                    Ok(Ok(resp)) => {
                        this.error = None;
                        this.apply_agents(resp.agents, None, window, cx);
                        this.refresh_quotas(true, cx);
                        window.push_notification(Notification::success(tr(cx, "external.probeDone")), cx);
                    }
                    Ok(Err(err)) => this.error = Some(err.to_string().into()),
                    Err(err) => this.error = Some(crate::ui::error_text(cx, &err)),
                }
                cx.notify();
            });
        })
        .detach();
    }

    #[allow(clippy::too_many_arguments)]
    fn configure(&mut self, config: ExternalAgentConfig, remove: bool, probe_after: bool, sync_draft: bool, label: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving {
            return;
        }
        self.saving = true;
        self.error = None;
        cx.notify();
        let kind_for_probe = config.kind.clone();
        let mut params = json!({ "config": config });
        if remove {
            params["remove"] = Value::Bool(true);
        }
        let call = self.store.read(cx).call("external.configure", params);
        cx.spawn_in(window, async move |this, cx| {
            let result = call.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.saving = false;
                match result.map(serde_json::from_value::<ExternalStatusResponse>) {
                    Ok(Ok(resp)) => {
                        this.error = None;
                        let force_sync = (sync_draft && !remove).then_some(kind_for_probe.as_str());
                        this.apply_agents(resp.agents, force_sync, window, cx);
                        let msg = if remove {
                            trf(cx, "external.removed", &[("name", &label)])
                        } else {
                            trf(cx, "external.saved", &[("name", &label)])
                        };
                        window.push_notification(Notification::success(msg), cx);
                        if probe_after && !remove {
                            this.reprobe(Some(kind_for_probe), window, cx);
                        }
                    }
                    Ok(Err(err)) => this.error = Some(err.to_string().into()),
                    Err(err) => this.error = Some(crate::ui::error_text(cx, &err)),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn save_agent(&mut self, agent: ExternalAgentStatus, window: &mut Window, cx: &mut Context<Self>) {
        let key = agent_key(&agent);
        let (binary, args) = match self.drafts.get(&key) {
            Some(draft) => {
                let b = draft.binary.read(cx).value().trim().to_owned();
                let a = parse_args(draft.args.read(cx).value().trim());
                ((!b.is_empty()).then_some(b), a)
            }
            None => (agent.binary.clone(), agent.args.clone()),
        };
        let config = ExternalAgentConfig {
            kind: agent.kind.clone(),
            enabled: agent.enabled,
            binary,
            args,
            label: Some(agent.label.clone()),
            default_model: agent.default_model.clone(),
            default_thinking: agent.default_thinking.clone(),
        };
        self.configure(config, false, true, true, agent.label, window, cx);
    }

    fn add_custom_acp(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let binary = self.custom_binary.read(cx).value().trim().to_owned();
        if binary.is_empty() {
            self.error = Some(tr(cx, "external.customRequired"));
            cx.notify();
            return;
        }
        let name_raw = self.custom_name.read(cx).value().trim().to_owned();
        let label = if name_raw.is_empty() { binary.clone() } else { name_raw.clone() };
        let kind = make_custom_kind(&name_raw, &binary, &self.agents);
        let args = parse_args(self.custom_args.read(cx).value().trim());
        let config = ExternalAgentConfig {
            kind,
            enabled: true,
            binary: Some(binary),
            args,
            label: Some(label.clone()),
            default_model: None,
            default_thinking: None,
        };
        self.custom_name.update(cx, |input, cx| input.set_value("", window, cx));
        self.custom_binary.update(cx, |input, cx| input.set_value("", window, cx));
        self.custom_args.update(cx, |input, cx| input.set_value("", window, cx));
        self.show_custom_form = false;
        self.configure(config, false, true, false, label, window, cx);
    }

    fn render_copy_row(&self, id: String, label: SharedString, command: String, cx: &App) -> AnyElement {
        let theme = cx.theme();
        let copy_text = command.clone();
        h_flex()
            .gap_2()
            .items_center()
            .child(div().w(px(72.)).flex_shrink_0().text_xs().text_color(theme.muted_foreground).child(label))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .px_2()
                    .py_1()
                    .rounded(theme.radius)
                    .bg(theme.muted)
                    .font_family(theme.mono_font_family.clone())
                    .text_xs()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .child(command),
            )
            .child(
                Button::new(SharedString::from(id))
                    .ghost()
                    .xsmall()
                    .icon(IconName::Copy)
                    .label(tr(cx, "external.copy"))
                    .on_click(move |_, window, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(copy_text.clone()));
                        window.push_notification(Notification::success(tr(cx, "external.copied")), cx);
                    }),
            )
            .into_any_element()
    }

    fn render_agent_row(&mut self, index: usize, agent: &ExternalAgentStatus, linux: bool, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let key = agent_key(agent);
        let expanded = self.expanded.as_deref() == Some(key.as_str());
        if expanded {
            self.ensure_draft(agent, window, cx);
        }
        let dot_color = if !agent.enabled {
            theme.muted_foreground.opacity(0.45)
        } else if agent.installed {
            theme.success
        } else {
            theme.warning
        };
        let status_text: SharedString = if !agent.enabled {
            tr(cx, "external.disabled")
        } else if agent.installed {
            match &agent.version {
                Some(ver) if !ver.trim().is_empty() => trf(cx, "external.version", &[("version", ver)]),
                _ => tr(cx, "external.ready"),
            }
        } else {
            tr(cx, "external.notInstalled")
        };
        let mut meta_parts: Vec<String> = vec![status_text.to_string()];
        if agent.installed && !agent.models.is_empty() {
            meta_parts.push(trf(cx, "external.modelsCount", &[("count", &agent.models.len())]).to_string());
        }
        if let Some(path) = agent.binary_path.as_deref().filter(|s| !s.is_empty()) {
            meta_parts.push(path.to_owned());
        }
        let toggle_agent = agent.clone();
        let save_agent = agent.clone();
        let remove_agent = agent.clone();
        let toggle_key = key.clone();
        let is_custom = is_custom_kind(&agent.kind);
        let (def_install, def_login) = default_hints(&agent.kind, linux);
        let install_hint = agent.install_hint.clone().or_else(|| def_install.map(str::to_owned));
        let login_hint = agent.login_hint.clone().or_else(|| def_login.map(str::to_owned));
        let os_label = if linux { "Linux / WSL / SSH" } else { "Windows" };

        let mut container = v_flex()
            .id(SharedString::from(format!("ext-agent-{index}")))
            .gap_2()
            .p_3()
            .rounded(theme.radius)
            .border_1()
            .border_color(theme.border)
            .bg(theme.background)
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(div().size(px(8.)).flex_shrink_0().rounded_full().bg(dot_color))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_0p5()
                            .child(div().text_sm().font_medium().truncate().child(agent.label.clone()))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .whitespace_nowrap()
                                    .child(meta_parts.join(" · ")),
                            ),
                    )
                    .child(
                        Switch::new(SharedString::from(format!("ext-enable-{index}")))
                            .checked(agent.enabled)
                            .disabled(self.saving)
                            .on_click(cx.listener(move |this, checked: &bool, window, cx| {
                                let cfg = ExternalAgentConfig {
                                    kind: toggle_agent.kind.clone(),
                                    enabled: *checked,
                                    binary: toggle_agent.binary.clone(),
                                    args: toggle_agent.args.clone(),
                                    label: Some(toggle_agent.label.clone()),
                                    default_model: toggle_agent.default_model.clone(),
                                    default_thinking: toggle_agent.default_thinking.clone(),
                                };
                                this.configure(cfg, false, false, false, toggle_agent.label.clone(), window, cx);
                            })),
                    )
                    .child(
                        Button::new(SharedString::from(format!("ext-expand-{index}")))
                            .ghost()
                            .xsmall()
                            .icon(if expanded { IconName::ChevronDown } else { IconName::ChevronRight })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if this.expanded.as_deref() == Some(toggle_key.as_str()) {
                                    this.expanded = None;
                                } else {
                                    this.expanded = Some(toggle_key.clone());
                                }
                                cx.notify();
                            })),
                    )
                    .when(is_custom, |this| {
                        this.child(
                            Button::new(SharedString::from(format!("ext-remove-{index}")))
                                .ghost()
                                .xsmall()
                                .label(tr(cx, "external.remove"))
                                .disabled(self.saving)
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    let cfg = ExternalAgentConfig {
                                        kind: remove_agent.kind.clone(),
                                        enabled: false,
                                        binary: remove_agent.binary.clone(),
                                        args: remove_agent.args.clone(),
                                        label: Some(remove_agent.label.clone()),
                                        default_model: None,
                                        default_thinking: None,
                                    };
                                    this.configure(cfg, true, false, false, remove_agent.label.clone(), window, cx);
                                })),
                        )
                    }),
            );

        if let Some(err) = agent.error.as_deref().filter(|s| !s.is_empty()) {
            // Why an agent was not found reads as a note; a found one that failed is an error.
            let color = if agent.installed { theme.danger } else { theme.muted_foreground };
            container = container.child(div().text_xs().text_color(color).child(err.to_owned()));
        }
        if agent.enabled {
            let locale = self.store.read(cx).locale();
            if let Some(quota) = crate::ui::quota::section(agent, None, locale, cx) {
                container = container.child(div().max_w(px(420.)).child(quota));
            }
        }

        if agent.kind == "antigravity" {
            container = container.child(
                div()
                    .px_2p5()
                    .py_1p5()
                    .rounded(theme.radius)
                    .bg(theme.muted.opacity(0.6))
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(tr(cx, "external.agyComplianceNote")),
            );
        }

        if !agent.installed || expanded {
            if install_hint.is_some() || login_hint.is_some() {
                let mut guide = v_flex()
                    .gap_1p5()
                    .pt_1()
                    .child(div().text_xs().font_medium().text_color(theme.muted_foreground).child(trf(cx, "external.installGuide", &[("os", &os_label)])));
                if let Some(cmd) = install_hint {
                    guide = guide.child(self.render_copy_row(format!("ext-copy-install-{index}"), tr(cx, "external.installCmd"), cmd, cx));
                }
                if let Some(cmd) = login_hint {
                    guide = guide.child(self.render_copy_row(format!("ext-copy-login-{index}"), tr(cx, "external.loginCmd"), cmd, cx));
                }
                container = container.child(guide);
            }
        }

        if expanded && let Some(draft) = self.drafts.get(&key) {
            container = container.child(
                v_flex()
                    .gap_2()
                    .pt_2()
                    .border_t_1()
                    .border_color(theme.border)
                    .child(
                        v_flex()
                            .gap_1()
                            .child(div().text_xs().text_color(theme.muted_foreground).child(tr(cx, "external.binaryLabel")))
                            .child(Input::new(&draft.binary).small().disabled(self.saving)),
                    )
                    .child(
                        v_flex()
                            .gap_1()
                            .child(div().text_xs().text_color(theme.muted_foreground).child(tr(cx, "external.argsLabel")))
                            .child(Input::new(&draft.args).small().disabled(self.saving)),
                    )
                    .child(
                        h_flex().gap_2().child(
                            Button::new(SharedString::from(format!("ext-save-{index}")))
                                .primary()
                                .small()
                                .label(tr(cx, "external.saveAndProbe"))
                                .loading(self.saving)
                                .on_click(cx.listener(move |this, _, window, cx| this.save_agent(save_agent.clone(), window, cx))),
                        ),
                    ),
            );
        }

        container.into_any_element()
    }
}

impl Render for ExternalAgentsCard {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let linux = matches!(self.store.read(cx).target, Some(ExecutionTarget::Wsl { .. } | ExecutionTarget::Ssh { .. }));
        let mut body: Vec<AnyElement> = Vec::new();
        body.push(
            h_flex()
                .gap_2()
                .items_center()
                .child(
                    Button::new("ext-reprobe")
                        .outline()
                        .small()
                        .icon(Icon::new(IconName::RefreshCw))
                        .label(tr(cx, if self.probing { "external.probing" } else { "external.reprobe" }))
                        .loading(self.probing)
                        .disabled(self.probing || self.saving)
                        .on_click(cx.listener(|this, _, window, cx| this.reprobe(None, window, cx))),
                )
                .child(
                    Button::new("ext-toggle-custom")
                        .ghost()
                        .small()
                        .icon(IconName::Plus)
                        .label(tr(cx, "external.addCustomTitle"))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.show_custom_form = !this.show_custom_form;
                            cx.notify();
                        })),
                )
                .into_any_element(),
        );
        if let Some(err) = &self.error {
            body.push(notice(cx, err.clone(), true));
        }
        let agents = self.agents.clone();
        for (index, agent) in agents.iter().enumerate() {
            body.push(self.render_agent_row(index, agent, linux, window, cx));
        }
        if self.show_custom_form {
            body.push(
                v_flex()
                    .gap_2()
                    .p_3()
                    .rounded(theme.radius)
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.background)
                    .child(div().text_sm().font_medium().child(tr(cx, "external.addCustomTitle")))
                    .child(
                        h_flex()
                            .gap_2()
                            .flex_wrap()
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w(px(160.))
                                    .gap_1()
                                    .child(div().text_xs().text_color(theme.muted_foreground).child(tr(cx, "external.customName")))
                                    .child(Input::new(&self.custom_name).small()),
                            )
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w(px(180.))
                                    .gap_1()
                                    .child(div().text_xs().text_color(theme.muted_foreground).child(tr(cx, "external.customBinary")))
                                    .child(Input::new(&self.custom_binary).small()),
                            ),
                    )
                    .child(
                        v_flex()
                            .gap_1()
                            .child(div().text_xs().text_color(theme.muted_foreground).child(tr(cx, "external.customArgs")))
                            .child(Input::new(&self.custom_args).small()),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("ext-custom-add")
                                    .primary()
                                    .small()
                                    .label(tr(cx, "external.addCustom"))
                                    .disabled(self.saving)
                                    .on_click(cx.listener(|this, _, window, cx| this.add_custom_acp(window, cx))),
                            )
                            .child(Button::new("ext-custom-cancel").ghost().small().label(tr(cx, "chat.cancel")).on_click(cx.listener(|this, _, _, cx| {
                                this.show_custom_form = false;
                                cx.notify();
                            }))),
                    )
                    .into_any_element(),
            );
        }
        card(cx, "external.settingsTitle", Some("external.settingsLead"), body)
    }
}

#[cfg(test)]
mod tests {
    use ::core::prelude::v1::test;

    use super::*;

    #[test]
    fn parses_quoted_and_plain_arguments() {
        assert_eq!(parse_args("acp --profile \"my profile\" 'a b'"), vec!["acp", "--profile", "my profile", "a b"]);
        assert_eq!(parse_args("   "), Vec::<String>::new());
    }

    #[test]
    fn generates_unique_valid_custom_kinds_and_aligned_hints() {
        let make = |kind: &str| ExternalAgentStatus {
            kind: kind.into(),
            label: kind.into(),
            enabled: true,
            installed: false,
            binary: None,
            args: Vec::new(),
            binary_path: None,
            version: None,
            models: Vec::new(),
            default_model: None,
            default_thinking: None,
            install_hint: None,
            login_hint: None,
            error: None,
            commands: Vec::new(),
            quota: None,
        };
        let existing = vec![make("codex"), make("antigravity"), make("my-acp")];
        assert_eq!(make_custom_kind("My ACP", "my-acp", &existing), "my-acp-2");
        assert_eq!(make_custom_kind("Codex", "codex", &existing), "acp-codex");
        assert_eq!(make_custom_kind("123 Tool", "tool", &existing), "acp-123-tool");
        assert!(is_custom_kind("my-acp-2"));
        assert!(!is_custom_kind("codex"));
        assert!(!is_custom_kind("antigravity"));
        assert_eq!(default_hints("antigravity", false).1, Some("agy acp --login"));
        assert_eq!(default_hints("antigravity", true).1, Some("agy acp --login"));
    }

    #[test]
    fn draft_sync_preserves_unsaved_user_edits() {
        assert!(should_sync_draft("/usr/bin/codex", "/usr/bin/codex", false));
        assert!(!should_sync_draft("/custom/codex", "/usr/bin/codex", false));
        assert!(should_sync_draft("/custom/codex", "/usr/bin/codex", true));
    }
}
