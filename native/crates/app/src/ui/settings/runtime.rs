//! Runtime (`RuntimeSection.tsx`): the execution environment, SSH hosts and the interpreters the current project uses.

use std::collections::BTreeMap;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use rainy_carrier::{ExecutionTarget, ssh, targets};
use serde::Deserialize;
use serde_json::{Value, json};

use super::models_extra::{card, notice};
use super::{group, heading, row};
use crate::store::Store;
use crate::store::ide::{Ide, IdeEvent};
use crate::ui::{tr, trf};
use crate::ui::workbench::confirm_switch;

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

pub struct RuntimePage {
    store: Entity<Store>,
    workspace: Option<String>,
    snapshot: Option<Snapshot>,
    language: &'static str,
    manual: Entity<InputState>,
    error: Option<SharedString>,
    busy: bool,
    /// SSH hosts added here and the aliases of `~/.ssh/config`.
    ssh_hosts: Vec<ssh::Host>,
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
        let submit = |this: &mut Self, _: &Entity<InputState>, event: &InputEvent, window: &mut Window, cx: &mut Context<Self>| {
            if let InputEvent::PressEnter { .. } = event {
                this.add_ssh_host(window, cx);
            }
        };
        let subscriptions = vec![
            cx.observe(&store, |_, _, cx| cx.notify()),
            cx.subscribe_in(&Ide::global(cx), window, |this, _, event: &IdeEvent, window, cx| {
                if *event == IdeEvent::WorkspaceChanged {
                    this.read(window, cx);
                }
            }),
            cx.subscribe_in(&ssh_destination, window, submit),
            cx.subscribe_in(&ssh_port, window, submit),
        ];
        let mut page = Self {
            store,
            workspace: None,
            snapshot: None,
            language: "python",
            manual,
            error: None,
            busy: false,
            ssh_hosts: Vec::new(),
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
        cx.notify();
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
                window.push_notification(Notification::error(rainy_carrier::locale::localized("SSH 端口必须是 1 到 65535 之间的整数。", "The SSH port must be a whole number from 1 to 65535.")), cx);
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
            ssh::probe(&destination, &askpass.session())?;
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
        let theme = cx.theme();
        let current_id = self.store.read(cx).target.as_ref().map(|target| target.id().to_owned());
        let mut body: Vec<AnyElement> = Vec::new();
        if self.ssh_hosts.is_empty() {
            body.push(div().text_xs().text_color(theme.muted_foreground).child(tr(cx, "settings.sshNone")).into_any_element());
        }
        for (index, host) in self.ssh_hosts.iter().enumerate() {
            let current = current_id.as_deref() == Some(host.target.id());
            let note = if current {
                Some(tr(cx, "settings.sshCurrent"))
            } else if host.saved {
                None
            } else {
                Some(tr(cx, "settings.sshFromConfig"))
            };
            let (switch_target, remove_target) = (host.target.clone(), host.target.clone());
            let store = self.store.clone();
            body.push(
                h_flex()
                    .gap_3()
                    .py_1()
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(div().text_sm().child(host.target.label().to_owned()))
                            .when_some(note, |this, note| this.child(div().text_xs().text_color(theme.muted_foreground).child(note))),
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
        self.busy = !reading;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = call.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = false;
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
        let picker = Button::new("runtime-target").outline().small().label(label).disabled(self.busy).dropdown_menu(move |mut menu, _, _| {
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
        let interpreter = Button::new("runtime-interpreter").outline().small().label(chosen).disabled(self.busy).dropdown_menu(move |mut menu, _, _| {
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
                .child(h_flex().child(Button::new("runtime-discover").outline().small().label(tr(cx, "settings.settingsDiscoverEnvironments")).disabled(self.busy).on_click(
                    cx.listener(|this, _, window, cx| this.run(json!({ "op": "discover" }), None, window, cx)),
                )))
                .child(div().text_xs().text_color(theme.muted_foreground).child(tr(cx, "settings.settingsEnvironmentPath")))
                .child(
                    h_flex()
                        .gap_2()
                        .child(div().flex_1().child(Input::new(&self.manual).small()))
                        .child(Button::new("runtime-probe").outline().small().label(tr(cx, "settings.settingsProbeEnvironment")).disabled(self.busy || probe_empty).on_click(
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
}
