//! Runtime (`RuntimeSection.tsx`): the execution environment, SSH hosts and the interpreters the current project uses.

use std::collections::BTreeMap;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, StyledExt as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use rainy_carrier::targets::{PortForwardRule, SshHostProfile};
use rainy_carrier::{ExecutionTarget, ssh, targets};
use serde::Deserialize;
use serde_json::{Value, json};

use super::models_extra::{card, notice};
use super::widgets::{AsyncPageGuard, bind_input_change};
use super::{group, heading, row};
use crate::store::ide::{Ide, IdeEvent};
use crate::store::{Store, StoreEvent};
use crate::ui::workbench::confirm_switch;
use crate::ui::{tr, trf};

const LANGUAGES: [&str; 5] = ["python", "node", "php", "c", "cpp"];

#[derive(Debug, Clone, Deserialize, PartialEq)]
struct Capability {
    name: String,
    ready: bool,
    #[serde(default)]
    detail: Option<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
struct Candidate {
    id: String,
    language: String,
    path: String,
    source: String,
    platform: String,
    version: Option<String>,
    ready: bool,
    #[serde(default)]
    capabilities: Vec<Capability>,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
struct PowerShell {
    path: String,
    edition: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct Snapshot {
    workspace_id: String,
    selected: BTreeMap<String, Candidate>,
    candidates: Vec<Candidate>,
    #[serde(default)]
    powershell: Option<PowerShell>,
}

struct ProfileEditor {
    target_id: String,
    host_label: String,
    rainy_home: Entity<InputState>,
    default_workspace: Entity<InputState>,
    bookmarks: Entity<TextareaState>,
    env: Entity<TextareaState>,
    keepalive_interval: Entity<InputState>,
    keepalive_count_max: Entity<InputState>,
    forwards: Entity<TextareaState>,
    notes: Entity<TextareaState>,
}

pub struct RuntimePage {
    store: Entity<Store>,
    target_id: Option<String>,
    workspace: Option<String>,
    snapshot: Option<Snapshot>,
    language: &'static str,
    manual: Entity<InputState>,
    error: Option<SharedString>,
    guard: AsyncPageGuard,
    /// SSH hosts added here and the aliases of `~/.ssh/config`.
    ssh_hosts: Vec<ssh::Host>,
    /// Per-host SSH research profiles persisted in `desktop.json`.
    ssh_profiles: BTreeMap<String, SshHostProfile>,
    /// Currently expanded research profile editor for one SSH host.
    profile_editor: Option<ProfileEditor>,
    ssh_destination: Entity<InputState>,
    ssh_port: Entity<InputState>,
    /// Testing a host before it is added.
    ssh_busy: bool,
    _subscriptions: Vec<Subscription>,
}

impl RuntimePage {
    pub fn new(store: Entity<Store>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let manual = cx.new(|cx| InputState::new(window, cx));
        let placeholder = tr(cx, "settings.sshHostPlaceholder");
        let ssh_destination = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
        let ssh_port = cx.new(|cx| InputState::new(window, cx).placeholder("22"));
        let target_id = store.read(cx).target.as_ref().map(|target| target.id().to_owned());
        let submit = |this: &mut Self, _: &Entity<InputState>, event: &InputEvent, window: &mut Window, cx: &mut Context<Self>| match event {
            InputEvent::PressEnter { .. } => this.add_ssh_host(window, cx),
            InputEvent::Change => cx.notify(),
            _ => {}
        };
        let subscriptions = vec![
            bind_input_change(&manual, window, cx),
            cx.observe(&store, |this, store, cx| {
                let current = store.read(cx).target.as_ref().map(|target| target.id());
                if this.target_id.as_deref() != current {
                    this.target_id = current.map(str::to_owned);
                    cx.notify();
                }
            }),
            cx.subscribe_in(&store, window, |this, _, event: &StoreEvent, window, cx| match event {
                StoreEvent::PrefsChanged => {
                    let placeholder = tr(cx, "settings.sshHostPlaceholder");
                    this.ssh_destination.update(cx, |input, cx| input.set_placeholder(placeholder, window, cx));
                    cx.notify();
                }
                StoreEvent::HostReconnected(_) => {
                    this.guard.invalidate();
                    this.read_ssh_hosts(cx);
                    this.read(window, cx);
                }
                _ => {}
            }),
            cx.subscribe_in(&Ide::global(cx), window, |this, _, event: &IdeEvent, window, cx| {
                if *event == IdeEvent::WorkspaceChanged {
                    this.guard.invalidate();
                    this.read(window, cx);
                }
            }),
            cx.subscribe_in(&ssh_destination, window, submit),
            cx.subscribe_in(&ssh_port, window, submit),
        ];
        let mut page = Self {
            store,
            target_id,
            workspace: None,
            snapshot: None,
            language: "python",
            manual,
            error: None,
            guard: AsyncPageGuard::default(),
            ssh_hosts: Vec::new(),
            ssh_profiles: BTreeMap::new(),
            profile_editor: None,
            ssh_destination,
            ssh_port,
            ssh_busy: false,
            _subscriptions: subscriptions,
        };
        page.read_ssh_hosts(cx);
        page.read(window, cx);
        page
    }

    fn read_ssh_hosts(&mut self, cx: &mut Context<Self>) {
        let preferences = targets::read_preferences(&self.store.read(cx).paths.desktop_settings()).unwrap_or_default();
        self.ssh_hosts = ssh::hosts(&preferences);
        self.ssh_profiles = targets::ssh_host_profiles(&preferences);
        cx.notify();
    }

    fn toggle_profile_editor(&mut self, target_id: String, host_label: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.profile_editor.as_ref().is_some_and(|ed| ed.target_id == target_id) {
            self.profile_editor = None;
            cx.notify();
            return;
        }
        let existing = self.ssh_profiles.get(&target_id).cloned().unwrap_or_default();
        let home_val = existing.rainy_home.unwrap_or_default();
        let ws_val = existing.default_workspace.unwrap_or_default();
        let bm_val = existing.bookmarks.join("\n");
        let env_val = format_env_lines(&existing.env);
        let ka_int = existing.keepalive_interval.map(|v| v.to_string()).unwrap_or_default();
        let ka_cnt = existing.keepalive_count_max.map(|v| v.to_string()).unwrap_or_default();
        let fwd_val = format_forward_lines(&existing.forwards);
        let notes_val = existing.notes.unwrap_or_default();

        let rainy_home = cx.new(|cx| {
            let mut s = InputState::new(window, cx).placeholder(tr(cx, "ssh.profileRainyHomePlaceholder"));
            if !home_val.is_empty() {
                s.set_value(home_val, window, cx);
            }
            s
        });
        let default_workspace = cx.new(|cx| {
            let mut s = InputState::new(window, cx).placeholder(tr(cx, "ssh.profileDefaultWorkspacePlaceholder"));
            if !ws_val.is_empty() {
                s.set_value(ws_val, window, cx);
            }
            s
        });
        let bookmarks = cx.new(|cx| {
            let mut s = TextareaState::new(window, cx).auto_grow(2, 5).placeholder(tr(cx, "ssh.profileBookmarksPlaceholder"));
            if !bm_val.is_empty() {
                s.set_value(bm_val, window, cx);
            }
            s
        });
        let env = cx.new(|cx| {
            let mut s = TextareaState::new(window, cx).auto_grow(2, 6).placeholder(tr(cx, "ssh.profileEnvPlaceholder"));
            if !env_val.is_empty() {
                s.set_value(env_val, window, cx);
            }
            s
        });
        let keepalive_interval = cx.new(|cx| {
            let mut s = InputState::new(window, cx).placeholder("10");
            if !ka_int.is_empty() {
                s.set_value(ka_int, window, cx);
            }
            s
        });
        let keepalive_count_max = cx.new(|cx| {
            let mut s = InputState::new(window, cx).placeholder("3");
            if !ka_cnt.is_empty() {
                s.set_value(ka_cnt, window, cx);
            }
            s
        });
        let forwards = cx.new(|cx| {
            let mut s = TextareaState::new(window, cx).auto_grow(2, 5).placeholder(tr(cx, "ssh.profileForwardsPlaceholder"));
            if !fwd_val.is_empty() {
                s.set_value(fwd_val, window, cx);
            }
            s
        });
        let notes = cx.new(|cx| {
            let mut s = TextareaState::new(window, cx).auto_grow(2, 4).placeholder(tr(cx, "ssh.profileNotesPlaceholder"));
            if !notes_val.is_empty() {
                s.set_value(notes_val, window, cx);
            }
            s
        });

        self.profile_editor = Some(ProfileEditor {
            target_id,
            host_label,
            rainy_home,
            default_workspace,
            bookmarks,
            env,
            keepalive_interval,
            keepalive_count_max,
            forwards,
            notes,
        });
        cx.notify();
    }

    fn save_active_profile(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ed) = &self.profile_editor else { return };
        let target_id = ed.target_id.clone();
        let host_label = ed.host_label.clone();
        let rainy_home = non_empty(ed.rainy_home.read(cx).value().to_string());
        let default_workspace = non_empty(ed.default_workspace.read(cx).value().to_string());
        let bookmarks: Vec<String> = ed
            .bookmarks
            .read(cx)
            .value()
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect();
        let env = match parse_env_lines(&ed.env.read(cx).value()) {
            Ok(env) => env,
            Err(bad_line) => {
                let text = trf(cx, "ssh.profileInvalidEnv", &[("line", &bad_line)]);
                window.push_notification(Notification::error(text), cx);
                return;
            }
        };
        let forwards = match parse_forward_lines(&ed.forwards.read(cx).value()) {
            Ok(rules) => rules,
            Err(bad_line) => {
                let text = trf(cx, "ssh.profileInvalidForward", &[("line", &bad_line)]);
                window.push_notification(Notification::error(text), cx);
                return;
            }
        };
        let keepalive_interval = match parse_opt_u16(&ed.keepalive_interval.read(cx).value()) {
            Ok(val) => val,
            Err(key) => {
                window.push_notification(Notification::error(tr(cx, key)), cx);
                return;
            }
        };
        let keepalive_count_max = match parse_opt_u8(&ed.keepalive_count_max.read(cx).value()) {
            Ok(val) => val,
            Err(key) => {
                window.push_notification(Notification::error(tr(cx, key)), cx);
                return;
            }
        };
        let notes = non_empty(ed.notes.read(cx).value().to_string());

        let profile = SshHostProfile {
            rainy_home,
            default_workspace,
            bookmarks,
            env,
            keepalive_interval,
            keepalive_count_max,
            forwards,
            notes,
        };
        let settings = self.store.read(cx).paths.desktop_settings();
        match targets::save_ssh_host_profile(&settings, &target_id, &profile) {
            Ok(()) => {
                self.read_ssh_hosts(cx);
                self.profile_editor = None;
                let text = trf(cx, "ssh.profileSaved", &[("host", &host_label)]);
                window.push_notification(Notification::success(text), cx);
                cx.notify();
            }
            Err(error) => {
                window.push_notification(Notification::error(format!("{error:#}")), cx);
            }
        }
    }

    fn render_profile_editor(&self, ed: &ProfileEditor, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let field_label = |key: &str| div().text_xs().text_color(theme.muted_foreground).child(tr(cx, key));
        v_flex()
            .mt_1()
            .mb_2()
            .p_3()
            .gap_2()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.border)
            .bg(theme.secondary.opacity(0.4))
            .child(div().text_xs().font_semibold().child(trf(cx, "ssh.profileTitle", &[("host", &ed.host_label)])))
            .child(v_flex().gap_1().child(field_label("ssh.profileRainyHome")).child(Input::new(&ed.rainy_home).small()))
            .child(v_flex().gap_1().child(field_label("ssh.profileDefaultWorkspace")).child(Input::new(&ed.default_workspace).small()))
            .child(v_flex().gap_1().child(field_label("ssh.profileBookmarks")).child(Textarea::new(&ed.bookmarks).small()))
            .child(v_flex().gap_1().child(field_label("ssh.profileEnv")).child(Textarea::new(&ed.env).small()))
            .child(
                v_flex()
                    .gap_1()
                    .child(field_label("ssh.profileKeepalive"))
                    .child(
                        h_flex()
                            .gap_2()
                            .child(div().w(px(120.)).child(Input::new(&ed.keepalive_interval).small()))
                            .child(div().w(px(120.)).child(Input::new(&ed.keepalive_count_max).small())),
                    ),
            )
            .child(v_flex().gap_1().child(field_label("ssh.profileForwards")).child(Textarea::new(&ed.forwards).small()))
            .child(v_flex().gap_1().child(field_label("ssh.profileNotes")).child(Textarea::new(&ed.notes).small()))
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("ssh-profile-cancel")
                            .ghost()
                            .small()
                            .label(tr(cx, "chat.cancel"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.profile_editor = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("ssh-profile-save")
                            .primary()
                            .small()
                            .label(tr(cx, "ssh.profileSave"))
                            .on_click(cx.listener(|this, _, window, cx| this.save_active_profile(window, cx))),
                    ),
            )
            .into_any_element()
    }

    /// Test the typed host with one connection, then remember it (`rainy:runtime-ssh-add`).
    fn add_ssh_host(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let typed = self.ssh_destination.read(cx).value().trim().to_owned();
        if typed.is_empty() || self.ssh_busy {
            return;
        }
        let port_text = self.ssh_port.read(cx).value().trim().to_owned();
        let port = if port_text.is_empty() { Ok(None) } else { port_text.parse::<u32>().map(Some) };
        let destination = port.map_err(anyhow::Error::from).and_then(|port| ssh::parse_destination(&typed, port));
        let destination = match destination {
            Ok(destination) => destination,
            Err(_) if port_text.parse::<u32>().is_err() && !port_text.is_empty() => {
                window.push_notification(Notification::error(tr(cx, "ssh.portInvalid")), cx);
                return;
            }
            Err(error) => {
                window.push_notification(Notification::error(error.to_string()), cx);
                return;
            }
        };
        let store = self.store.read(cx);
        let Some(askpass) = store.askpass.clone() else { return };
        let settings = store.paths.desktop_settings();
        self.ssh_busy = true;
        cx.notify();
        let added = cx.background_spawn(async move {
            ssh::probe(&destination, &askpass.session(&ssh::askpass_scope(&destination)))?;
            ssh::save_host(&settings, &destination)?;
            anyhow::Ok(ssh::target(&destination).label().to_owned())
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = added.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.ssh_busy = false;
                match result {
                    Ok(label) => {
                        this.ssh_destination.update(cx, |input, cx| input.set_value("", window, cx));
                        this.ssh_port.update(cx, |input, cx| input.set_value("", window, cx));
                        this.read_ssh_hosts(cx);
                        window.push_notification(Notification::success(trf(cx, "settings.sshAdded", &[("name", &label)]).to_string()), cx);
                    }
                    Err(error) => window.push_notification(Notification::error(format!("{error:#}")), cx),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn remove_ssh_host(&mut self, target: ExecutionTarget, window: &mut Window, cx: &mut Context<Self>) {
        let settings = self.store.read(cx).paths.desktop_settings();
        match ssh::remove_host(&settings, target.id()) {
            Ok(()) => window.push_notification(trf(cx, "settings.sshRemoved", &[("name", &target.label())]).to_string(), cx),
            Err(error) => window.push_notification(Notification::error(format!("{error:#}")), cx),
        }
        self.read_ssh_hosts(cx);
    }

    /// SSH remote development: the hosts, switching to one, and adding one after a test connection.
    fn render_ssh(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let current_id = self.store.read(cx).target.as_ref().map(|target| target.id().to_owned());
        let mut body: Vec<AnyElement> = Vec::new();
        if self.ssh_hosts.is_empty() {
            body.push(div().text_xs().text_color(theme.muted_foreground).child(tr(cx, "settings.sshNone")).into_any_element());
        }
        for (index, host) in self.ssh_hosts.iter().enumerate() {
            let target_id = host.target.id().to_owned();
            let host_label = host.target.label().to_owned();
            let current = current_id.as_deref() == Some(target_id.as_str());
            let mut notes_parts: Vec<String> = Vec::new();
            if current {
                notes_parts.push(tr(cx, "settings.sshCurrent").to_string());
            } else if !host.saved {
                notes_parts.push(tr(cx, "settings.sshFromConfig").to_string());
            }
            if let Some(profile) = self.ssh_profiles.get(&target_id) {
                if let Some(home) = &profile.rainy_home {
                    notes_parts.push(format!("RAINY_HOME={home}"));
                }
                if !profile.bookmarks.is_empty() {
                    notes_parts.push(trf(cx, "ssh.profileSummaryBookmarks", &[("count", &profile.bookmarks.len())]).to_string());
                }
                if !profile.env.is_empty() {
                    notes_parts.push(trf(cx, "ssh.profileSummaryEnv", &[("count", &profile.env.len())]).to_string());
                }
                if !profile.forwards.is_empty() {
                    notes_parts.push(trf(cx, "ssh.profileSummaryForwards", &[("count", &profile.forwards.len())]).to_string());
                }
            }
            let note = (!notes_parts.is_empty()).then(|| notes_parts.join(" · "));
            let editing_this = self.profile_editor.as_ref().is_some_and(|ed| ed.target_id == target_id);
            let (switch_target, remove_target) = (host.target.clone(), host.target.clone());
            let (edit_id, edit_label) = (target_id.clone(), host_label.clone());
            let store = self.store.clone();
            body.push(
                h_flex()
                    .gap_2()
                    .py_1()
                    .items_center()
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(div().text_sm().truncate().child(host_label))
                            .when_some(note, |this, note| this.child(div().text_xs().text_color(theme.muted_foreground).truncate().child(note))),
                    )
                    .child(
                        Button::new(("ssh-profile", index))
                            .ghost()
                            .small()
                            .label(tr(cx, if editing_this { "ssh.profileHide" } else { "ssh.profileButton" }))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.toggle_profile_editor(edit_id.clone(), edit_label.clone(), window, cx);
                            })),
                    )
                    .when(!current, |this| {
                        this.child(Button::new(("ssh-switch", index)).outline().small().label(tr(cx, "settings.sshSwitch")).disabled(self.ssh_busy).on_click(
                            move |_, window, cx| confirm_switch(store.clone(), switch_target.clone(), window, cx),
                        ))
                    })
                    .when(!current && host.saved, |this| {
                        this.child(Button::new(("ssh-remove", index)).ghost().small().label(tr(cx, "settings.sshRemove")).disabled(self.ssh_busy).on_click(
                            cx.listener(move |this, _, window, cx| this.remove_ssh_host(remove_target.clone(), window, cx)),
                        ))
                    })
                    .into_any_element(),
            );
            if let Some(ed) = self.profile_editor.as_ref().filter(|ed| ed.target_id == target_id) {
                body.push(self.render_profile_editor(ed, cx));
            }
        }
        let label = |key: &str| div().text_xs().text_color(theme.muted_foreground).child(tr(cx, key));
        body.push(
            h_flex()
                .gap_2()
                .items_end()
                .child(v_flex().flex_1().gap_1().child(label("settings.sshHost")).child(Input::new(&self.ssh_destination).disabled(self.ssh_busy)))
                .child(v_flex().w(px(96.)).gap_1().child(label("settings.sshPort")).child(Input::new(&self.ssh_port).disabled(self.ssh_busy)))
                .child(
                    Button::new("ssh-add")
                        .outline()
                        .label(tr(cx, "settings.sshAdd"))
                        .loading(self.ssh_busy)
                        .disabled(self.ssh_busy || self.ssh_destination.read(cx).value().trim().is_empty())
                        .on_click(cx.listener(|this, _, window, cx| this.add_ssh_host(window, cx))),
                )
                .into_any_element(),
        );
        if self.ssh_busy {
            body.push(notice(cx, tr(cx, "settings.sshConnecting"), false));
        }
        card(cx, "settings.sshTitle", Some("settings.sshLead"), body)
    }

    fn read(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.workspace = Ide::global(cx).read(cx).workspace.as_ref().map(|workspace| workspace.workspace_id.clone());
        self.snapshot = None;
        if self.workspace.is_some() {
            self.run(json!({ "op": "status" }), None, window, cx);
        }
        cx.notify();
    }

    /// One `runtime` request; its answer is the project's new snapshot.
    fn run(&mut self, mut params: Value, success: Option<&'static str>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(workspace) = self.workspace.clone() else { return };
        params["workspaceId"] = json!(workspace);
        let reading = params["op"] == "status";
        let call = self.store.read(cx).call("runtime", params);
        let generation = if reading { self.guard.begin_read() } else { self.guard.begin_write() };
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = call.await;
            let _ = this.update_in(cx, |this, window, cx| {
                let current = if reading { this.guard.end_read(generation) } else { this.guard.end_write(generation) };
                if !current {
                    cx.notify();
                    return;
                }
                match result.map(serde_json::from_value::<Snapshot>) {
                    Ok(Ok(snapshot)) if Some(&snapshot.workspace_id) == this.workspace.as_ref() => {
                        this.snapshot = Some(snapshot);
                        this.error = None;
                        if let Some(key) = success {
                            window.push_notification(tr(cx, key).to_string(), cx);
                        }
                    }
                    Ok(Ok(_)) => {}
                    Ok(Err(error)) => this.error = Some(error.to_string().into()),
                    Err(error) if reading => this.error = Some(crate::ui::error_text(cx, &error)),
                    Err(error) => window.push_notification(crate::ui::error_notification(cx, &error), cx),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn render_target(&self, cx: &mut Context<Self>) -> AnyElement {
        let store = self.store.read(cx);
        let current = store.target.clone().unwrap_or_else(ExecutionTarget::windows);
        let handle = self.store.clone();
        let label: SharedString = current.label().to_owned().into();
        let current_id = current.id().to_owned();
        let ssh_choices: Vec<ExecutionTarget> = self.ssh_hosts.iter().map(|host| host.target.clone()).collect();
        let picker = Button::new("runtime-target").outline().small().label(label).disabled(self.guard.busy()).dropdown_menu(move |mut menu, _, _| {
            let mut choices = vec![ExecutionTarget::windows()];
            choices.extend(targets::wsl_distributions());
            choices.extend(ssh_choices.iter().cloned());
            for choice in choices {
                let checked = choice.id() == current_id;
                let handle = handle.clone();
                menu = menu.item(PopupMenuItem::new(choice.label().to_owned()).checked(checked).on_click(move |_, window, cx| {
                    if !checked {
                        confirm_switch(handle.clone(), choice.clone(), window, cx);
                    }
                }));
            }
            menu
        });
        group(cx, None, vec![row(cx, tr(cx, "settings.settingsExecutionTarget"), Some(tr(cx, "settings.settingsEnvironmentNote")), picker)]).into_any_element()
    }

    fn render_interpreters(&self, snapshot: &Snapshot, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = cx.theme().clone();
        let language = self.language;
        let selected = snapshot.selected.get(language).map(|candidate| candidate.path.clone());
        let candidates: Vec<Candidate> = snapshot.candidates.iter().filter(|candidate| candidate.language == language).cloned().collect();
        let language_picker = h_flex().gap_1().children(LANGUAGES.into_iter().map(|value| {
            let button = Button::new(SharedString::from(format!("runtime-language-{value}"))).small().label(value);
            let button = if value == language { button.primary() } else { button.outline() };
            button.on_click(cx.listener(move |this, _, _, cx| {
                this.language = value;
                cx.notify();
            }))
        }));
        let automatic = tr(cx, "settings.settingsAutomatic");
        let chosen: SharedString = selected.clone().map(Into::into).unwrap_or_else(|| automatic.clone());
        let options: Vec<(String, String, bool)> = candidates
            .iter()
            .map(|candidate| {
                let label = match &candidate.version {
                    Some(version) => format!("{} · {version}", candidate.path),
                    None => candidate.path.clone(),
                };
                (candidate.path.clone(), label, candidate.ready)
            })
            .collect();
        let view = cx.entity();
        let current = selected.clone();
        let interpreter = Button::new("runtime-interpreter").outline().small().label(chosen).disabled(self.guard.busy()).dropdown_menu(move |mut menu, _, _| {
            let select = |path: Option<String>, view: Entity<RuntimePage>| {
                move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                    let path = path.clone();
                    view.update(cx, |this, cx| this.run(json!({ "op": "select", "language": language, "path": path }), Some("settings.settingsApplied"), window, cx));
                }
            };
            menu = menu.item(PopupMenuItem::new(automatic.clone()).checked(current.is_none()).on_click(select(None, view.clone())));
            for (path, label, ready) in &options {
                menu = menu.item(PopupMenuItem::new(label.clone()).checked(current.as_ref() == Some(path)).disabled(!ready).on_click(select(Some(path.clone()), view.clone())));
            }
            menu
        });
        let mut items = vec![group(cx, None, vec![
            row(cx, tr(cx, "settings.settingsLanguage"), None, language_picker),
            row(cx, tr(cx, "settings.settingsRuntime"), None, interpreter),
        ])
        .into_any_element()];
        if let Some(shell) = &snapshot.powershell {
            let edition = tr(cx, if shell.edition == "core" { "settings.settingsShellCore" } else { "settings.settingsShellDesktop" });
            let mut body = vec![
                div().text_sm().child(edition).into_any_element(),
                div().text_xs().font_family(theme.mono_font_family.clone()).text_color(theme.muted_foreground).child(shell.path.clone()).into_any_element(),
            ];
            if shell.edition == "desktop" {
                body.push(div().text_xs().text_color(theme.muted_foreground).child(tr(cx, "settings.settingsShellDesktopHint")).into_any_element());
            }
            items.push(card(cx, "settings.settingsShell", None, body));
        }
        let probe_empty = self.manual.read(cx).value().trim().is_empty();
        items.push(
            v_flex()
                .gap_2()
                .child(h_flex().child(Button::new("runtime-discover").outline().small().label(tr(cx, "settings.settingsDiscoverEnvironments")).disabled(self.guard.busy()).on_click(
                    cx.listener(|this, _, window, cx| this.run(json!({ "op": "discover" }), None, window, cx)),
                )))
                .child(div().text_xs().text_color(theme.muted_foreground).child(tr(cx, "settings.settingsEnvironmentPath")))
                .child(
                    h_flex()
                        .gap_2()
                        .child(div().flex_1().child(Input::new(&self.manual).small()))
                        .child(Button::new("runtime-probe").outline().small().label(tr(cx, "settings.settingsProbeEnvironment")).disabled(self.guard.busy() || probe_empty).on_click(
                            cx.listener(move |this, _, window, cx| {
                                let path = this.manual.read(cx).value().trim().to_owned();
                                this.run(json!({ "op": "probe", "language": language, "path": path }), None, window, cx);
                            }),
                        )),
                )
                .into_any_element(),
        );
        for candidate in candidates {
            let ready = tr(cx, if candidate.ready { "settings.settingsReadyCapability" } else { "settings.settingsMissingCapability" });
            let detail = [candidate.version.clone().unwrap_or(candidate.platform.clone()), candidate.source.clone()].join(" · ");
            items.push(
                v_flex()
                    .id(SharedString::from(format!("candidate-{}", candidate.id)))
                    .gap_1()
                    .p_3()
                    .rounded(theme.radius)
                    .border_1()
                    .border_color(theme.border)
                    .child(
                        h_flex()
                            .gap_2()
                            .justify_between()
                            .child(div().text_sm().font_family(theme.mono_font_family.clone()).overflow_hidden().text_ellipsis().child(candidate.path.clone()))
                            .child(div().flex_shrink_0().text_xs().text_color(if candidate.ready { theme.muted_foreground } else { theme.danger }).child(ready)),
                    )
                    .child(div().text_xs().text_color(theme.muted_foreground).child(detail))
                    .when_some(candidate.error.clone(), |this, error| this.child(div().text_xs().text_color(theme.danger).child(error)))
                    .children(candidate.capabilities.iter().map(|capability| {
                        let state = tr(cx, if capability.ready { "settings.settingsReadyCapability" } else { "settings.settingsMissingCapability" });
                        let mut text = format!("{} · {state}", capability.name);
                        if let Some(detail) = &capability.detail {
                            text.push_str(&format!(" · {detail}"));
                        }
                        div().text_xs().text_color(theme.muted_foreground).child(text)
                    }))
                    .into_any_element(),
            );
        }
        items
    }
}

impl Render for RuntimePage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let target = self.render_target(cx);
        let remote = self.render_ssh(cx);
        let mut page = v_flex().gap_4().child(heading(cx, "settings.settingsRuntime", "settings.runtimeLead")).child(target).child(remote);
        if let Some(error) = &self.error {
            page = page.child(notice(cx, error.clone(), true));
        }
        page = if self.workspace.is_none() {
            page.child(notice(cx, tr(cx, "settings.settingsOpenProject"), false))
        } else {
            match self.snapshot.clone() {
                None => page.child(h_flex().child(Spinner::new().small())),
                Some(snapshot) => {
                    let items = self.render_interpreters(&snapshot, cx);
                    page.children(items)
                }
            }
        };
        page
    }
}

/// A size as 3.x's settings show it (`fileSizeText`): `512B`, `9.5KB`, `36MB`, `1.2GB`.
pub(super) fn file_size_text(bytes: u64) -> String {
    if bytes < 1024 {
        return format!("{bytes}B");
    }
    let mut value = bytes as f64 / 1024.0;
    for unit in ["KB", "MB"] {
        if value < 1024.0 {
            return scaled(value, unit);
        }
        value /= 1024.0;
    }
    scaled(value, "GB")
}

fn scaled(value: f64, unit: &str) -> String {
    if value < 10.0 { format!("{value:.1}{unit}") } else { format!("{}{unit}", value.round()) }
}

fn non_empty(raw: String) -> Option<String> {
    let trimmed = raw.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

fn parse_opt_u16(raw: &str) -> Result<Option<u16>, &'static str> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    trimmed.parse::<u16>().map(Some).map_err(|_| "ssh.profileInvalidPositiveInt")
}

fn parse_opt_u8(raw: &str) -> Result<Option<u8>, &'static str> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    trimmed.parse::<u8>().map(Some).map_err(|_| "ssh.profileInvalidPositiveInt")
}

fn format_env_lines(env: &BTreeMap<String, String>) -> String {
    env.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("\n")
}

fn parse_env_lines(raw: &str) -> Result<BTreeMap<String, String>, String> {
    let mut map = BTreeMap::new();
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let Some((key, value)) = trimmed.split_once('=') else {
            return Err(trimmed.to_owned());
        };
        let key = key.trim();
        if key.is_empty() || key.contains(char::is_whitespace) {
            return Err(trimmed.to_owned());
        }
        map.insert(key.to_owned(), value.to_owned());
    }
    Ok(map)
}

fn format_forward_lines(rules: &[PortForwardRule]) -> String {
    rules
        .iter()
        .map(|rule| {
            let prefix = if rule.auto_start { "*" } else { "" };
            match rule.label.as_deref().filter(|l| !l.trim().is_empty()) {
                Some(label) => format!("{prefix}{}:{} {label}", rule.local_port, rule.remote_port),
                None => format!("{prefix}{}:{}", rule.local_port, rule.remote_port),
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn parse_forward_lines(raw: &str) -> Result<Vec<PortForwardRule>, String> {
    let mut rules = Vec::new();
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let (auto_start, rest) = match trimmed.strip_prefix('*') {
            Some(after) => (true, after.trim_start()),
            None => (false, trimmed),
        };
        let mut parts = rest.splitn(2, char::is_whitespace);
        let spec = parts.next().unwrap_or_default();
        let label = parts
            .next()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_owned);
        let Some((local_s, remote_s)) = spec.split_once(':') else {
            return Err(trimmed.to_owned());
        };
        let (Ok(local), Ok(remote)) = (local_s.parse::<u16>(), remote_s.parse::<u16>()) else {
            return Err(trimmed.to_owned());
        };
        if local == 0 || remote == 0 {
            return Err(trimmed.to_owned());
        }
        let mut rule = PortForwardRule::new(local, remote);
        rule.label = label;
        rule.auto_start = auto_start;
        rules.push(rule);
    }
    Ok(rules)
}

#[cfg(test)]
mod tests {
    use ::core::prelude::v1::test;

    use super::*;

    #[test]
    fn sizes_read_as_in_3x() {
        assert_eq!(file_size_text(512), "512B");
        assert_eq!(file_size_text(9_728), "9.5KB");
        assert_eq!(file_size_text(38_191_707), "36MB");
        assert_eq!(file_size_text(100_595_019), "96MB");
        assert_eq!(file_size_text(558_773_087), "533MB");
        assert_eq!(file_size_text(1_288_490_189), "1.2GB");
    }

    #[test]
    fn env_lines_round_trip_and_reject_invalid() {
        let parsed = parse_env_lines("HF_ENDPOINT=https://hf-mirror.com\n# comment\nPIP_INDEX_URL=https://a/b?x=1\n").unwrap();
        assert_eq!(parsed.get("HF_ENDPOINT").map(String::as_str), Some("https://hf-mirror.com"));
        assert_eq!(parsed.get("PIP_INDEX_URL").map(String::as_str), Some("https://a/b?x=1"));
        assert_eq!(parse_env_lines(&format_env_lines(&parsed)).unwrap(), parsed);
        assert!(parse_env_lines("NO_EQUALS").is_err());
    }

    #[test]
    fn forward_lines_round_trip_and_reject_invalid() {
        let parsed = parse_forward_lines("*6006:6006 TensorBoard\n8888:8888 Jupyter Lab\n7860:7860\n").unwrap();
        assert_eq!(parsed.len(), 3);
        assert!(parsed[0].auto_start);
        assert_eq!(parsed[0].local_port, 6006);
        assert_eq!(parsed[0].label.as_deref(), Some("TensorBoard"));
        assert!(!parsed[1].auto_start);
        assert_eq!(parsed[1].label.as_deref(), Some("Jupyter Lab"));
        assert_eq!(parsed[2].label, None);
        let formatted = format_forward_lines(&parsed);
        assert_eq!(parse_forward_lines(&formatted).unwrap().len(), 3);
        assert!(parse_forward_lines("bad-spec").is_err());
        assert!(parse_forward_lines("0:6006").is_err());
    }
}
