//! Security (`SecuritySection.tsx`): the mode new chats start in, keeping the agent's changes, and the commands and
//! tools the current project allows without asking.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, StyledExt as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::*;
use serde::Deserialize;
use serde_json::{Value, json};

use super::models_extra::notice;
use super::{group, heading, row, set_prefs};
use crate::store::Store;
use crate::store::ide::{Ide, IdeEvent};
use crate::ui::tr;

#[derive(Debug, Clone, Deserialize, PartialEq)]
struct Rules {
    commands: Vec<String>,
    tools: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct Status {
    default_mode: String,
    rules: Rules,
}

pub struct SecurityPage {
    store: Entity<Store>,
    workspace: Option<String>,
    status: Option<Status>,
    error: Option<SharedString>,
    busy: bool,
    _subscriptions: Vec<Subscription>,
}

impl SecurityPage {
    pub fn new(store: Entity<Store>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let subscriptions = vec![cx.subscribe_in(&Ide::global(cx), window, |this, _, event: &IdeEvent, window, cx| {
            if *event == IdeEvent::WorkspaceChanged {
                this.read(window, cx);
            }
        })];
        let mut page = Self { store, workspace: None, status: None, error: None, busy: false, _subscriptions: subscriptions };
        page.read(window, cx);
        page
    }

    fn read(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.workspace = Ide::global(cx).read(cx).workspace.as_ref().map(|workspace| workspace.workspace_id.clone());
        self.status = None;
        self.call("security.status", json!({ "workspaceId": self.workspace }), None, false, window, cx);
    }

    /// One security request; `keep_rules` keeps the listed rules, for answers that do not carry this project's.
    fn call(&mut self, method: &str, params: Value, success: Option<&'static str>, keep_rules: bool, window: &mut Window, cx: &mut Context<Self>) {
        let call = self.store.read(cx).call(method, params);
        let workspace = self.workspace.clone();
        let reading = method == "security.status";
        self.busy = !reading;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = call.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                if this.workspace != workspace {
                    return;
                }
                match result.map(serde_json::from_value::<Status>) {
                    Ok(Ok(status)) => {
                        let rules = this.status.as_ref().filter(|_| keep_rules).map(|status| status.rules.clone());
                        this.status = Some(match rules {
                            Some(rules) => Status { rules, ..status },
                            None => status,
                        });
                        this.error = None;
                        if let Some(key) = success {
                            window.push_notification(tr(cx, key).to_string(), cx);
                        }
                    }
                    Ok(Err(error)) => this.error = Some(error.to_string().into()),
                    Err(error) if reading => this.error = Some(crate::ui::error_text(cx, &error)),
                    Err(error) => window.push_notification(crate::ui::error_notification(cx, &error), cx),
                }
                cx.notify();
            });
        })
        .detach();
    }
}

impl Render for SecurityPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let mut page = v_flex().gap_4().child(heading(cx, "settings.settingsSecurity", "settings.settingsSecurityNote"));
        if let Some(error) = &self.error {
            page = page.child(notice(cx, error.clone(), true));
        }
        let Some(status) = self.status.clone() else {
            return page.child(h_flex().child(Spinner::new().small()));
        };
        let prefs = self.store.read(cx).prefs.clone();
        let current = status.default_mode.clone();
        let modes = [("read-only", "settings.securityModeReadOnly"), ("ask", "settings.securityModeAsk"), ("auto", "settings.securityModeAuto")];
        let chooser = h_flex().gap_1().children(modes.into_iter().map(|(mode, key)| {
            let button = Button::new(SharedString::from(format!("mode-{mode}"))).small().label(tr(cx, key)).disabled(self.busy);
            let button = if current == mode { button.primary() } else { button.outline() };
            button.on_click(cx.listener(move |this, _, window, cx| {
                // Changing the default mode answers without this project's rules.
                this.call("security.setDefault", json!({ "mode": mode }), Some("settings.settingsApplied"), true, window, cx);
            }))
        }));
        page = page.child(group(cx, None, vec![
            row(cx, tr(cx, "settings.securityDefaultMode"), Some(tr(cx, "settings.securityDefaultModeNote")), chooser),
            row(
                cx,
                tr(cx, "settings.securityAutoKeep"),
                Some(tr(cx, "settings.securityAutoKeepNote")),
                Switch::new("auto-keep").checked(prefs.auto_keep_changes).on_click(|checked: &bool, window, cx| {
                    set_prefs(json!({ "autoKeepChanges": *checked }), window, cx);
                }),
            ),
        ]));
        page = page
            .child(div().text_sm().font_semibold().child(tr(cx, "settings.securityRules")))
            .child(div().text_xs().text_color(theme.muted_foreground).child(tr(cx, "settings.securityRulesNote")));
        let Some(workspace) = self.workspace.clone() else {
            return page.child(notice(cx, tr(cx, "settings.settingsOpenProject"), false));
        };
        let rules: Vec<(&str, String)> =
            status.rules.commands.iter().map(|rule| ("command", rule.clone())).chain(status.rules.tools.iter().map(|rule| ("tool", rule.clone()))).collect();
        if rules.is_empty() {
            return page.child(div().text_sm().text_color(theme.muted_foreground).child(tr(cx, "settings.securityNoRules")));
        }
        let rows: Vec<AnyElement> = rules
            .into_iter()
            .enumerate()
            .map(|(index, (kind, rule))| {
                let shown = if kind == "tool" { rule.trim_start_matches("web_fetch:").trim_start_matches("mcp__").replacen("__", ": ", 1) } else { rule.clone() };
                let note = if kind == "command" {
                    "settings.securityRuleCommand"
                } else if rule.starts_with("web_fetch:") {
                    "settings.securityRuleWeb"
                } else {
                    "settings.securityRuleTool"
                };
                let workspace = workspace.clone();
                h_flex()
                    .px_4()
                    .py_2()
                    .gap_3()
                    .items_center()
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(div().text_sm().font_family(theme.mono_font_family.clone()).child(shown))
                            .child(div().text_xs().text_color(theme.muted_foreground).child(tr(cx, note))),
                    )
                    .child(Button::new(("rule-remove", index)).ghost().small().label(tr(cx, "settings.securityRemoveRule")).disabled(self.busy).on_click(cx.listener(
                        move |this, _, window, cx| {
                            this.call("security.removeRule", json!({ "workspaceId": workspace, "kind": kind, "rule": rule }), None, false, window, cx);
                        },
                    )))
                    .into_any_element()
            })
            .collect();
        page.child(group(cx, None, rows))
    }
}
