//! Runtime (`RuntimeSection.tsx`): the execution environment, and the interpreters the current project uses.

use std::collections::BTreeMap;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use rainy_carrier::{ExecutionTarget, targets};
use serde::Deserialize;
use serde_json::{Value, json};

use super::models_extra::{card, notice};
use super::{group, heading, row};
use crate::store::Store;
use crate::store::ide::{Ide, IdeEvent};
use crate::ui::tr;
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
    _subscriptions: Vec<Subscription>,
}

impl RuntimePage {
    pub fn new(store: Entity<Store>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let manual = cx.new(|cx| InputState::new(window, cx));
        let subscriptions = vec![cx.subscribe_in(&Ide::global(cx), window, |this, _, event: &IdeEvent, window, cx| {
            if *event == IdeEvent::WorkspaceChanged {
                this.read(window, cx);
            }
        })];
        let mut page = Self { store, workspace: None, snapshot: None, language: "python", manual, error: None, busy: false, _subscriptions: subscriptions };
        page.read(window, cx);
        page
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
                    Err(error) if reading => this.error = Some(error.message.into()),
                    Err(error) => window.push_notification(Notification::error(error.message), cx),
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
        let picker = Button::new("runtime-target").outline().small().label(label).disabled(self.busy).dropdown_menu(move |mut menu, _, _| {
            let mut choices = vec![ExecutionTarget::windows()];
            choices.extend(targets::wsl_distributions());
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
        let mut page = v_flex().gap_4().child(heading(cx, "settings.settingsRuntime", "settings.runtimeLead")).child(target);
        if let Some(error) = &self.error {
            page = page.child(notice(cx, error.clone(), true));
        }
        if self.workspace.is_none() {
            return page.child(notice(cx, tr(cx, "settings.settingsOpenProject"), false));
        }
        match self.snapshot.clone() {
            None => page.child(h_flex().child(Spinner::new().small())),
            Some(snapshot) => {
                let items = self.render_interpreters(&snapshot, cx);
                page.children(items)
            }
        }
    }
}
