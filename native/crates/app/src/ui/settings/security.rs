//! Security (`SecuritySection.tsx`): the mode new chats start in, keeping the agent's changes, workspace trust,
//! and allow/wildcard/deny permission rules for the current project.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _, StyledExt as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use serde::Deserialize;
use serde_json::{Value, json};

use super::models_extra::notice;
use super::{group, heading, row, set_prefs};
use crate::store::ide::{Ide, IdeEvent};
use crate::store::{Store, StoreEvent};
use crate::ui::tr;

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct Rules {
    #[serde(default)]
    commands: Vec<String>,
    #[serde(default)]
    tools: Vec<String>,
    #[serde(default)]
    deny_commands: Vec<String>,
    #[serde(default)]
    deny_paths: Vec<String>,
    #[serde(default)]
    deny_tools: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct Status {
    default_mode: String,
    #[serde(default)]
    rules: Rules,
    #[serde(default)]
    trusted: bool,
    #[serde(default)]
    workspace_path: Option<String>,
    #[serde(default)]
    trusted_workspaces: Vec<String>,
}

const RULE_KINDS: [(&str, &str); 5] = [
    ("command", "native.securityRuleKindCommand"),
    ("tool", "native.securityRuleKindTool"),
    ("denyCommand", "native.securityRuleKindDenyCommand"),
    ("denyPath", "native.securityRuleKindDenyPath"),
    ("denyTool", "native.securityRuleKindDenyTool"),
];

pub struct SecurityPage {
    store: Entity<Store>,
    workspace: Option<String>,
    status: Option<Status>,
    error: Option<SharedString>,
    busy: bool,
    rule_kind: &'static str,
    rule_input: Entity<InputState>,
    _subscriptions: Vec<Subscription>,
}

impl SecurityPage {
    pub fn new(store: Entity<Store>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let placeholder = tr(cx, "native.securityRulePlaceholder");
        let rule_input = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
        let subscriptions = vec![
            cx.subscribe_in(&Ide::global(cx), window, |this, _, event: &IdeEvent, window, cx| {
                if *event == IdeEvent::WorkspaceChanged {
                    this.read(window, cx);
                }
            }),
            cx.subscribe_in(&store, window, |this, _, event: &StoreEvent, window, cx| match event {
                StoreEvent::Host { name, .. } if name == "security.changed" => this.read(window, cx),
                StoreEvent::PrefsChanged => {
                    let placeholder = tr(cx, "native.securityRulePlaceholder");
                    this.rule_input.update(cx, |state, cx| state.set_placeholder(placeholder, window, cx));
                }
                _ => {}
            }),
            cx.subscribe_in(&rule_input, window, |this, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    this.add_rule(window, cx);
                }
            }),
        ];
        let mut page = Self {
            store,
            workspace: None,
            status: None,
            error: None,
            busy: false,
            rule_kind: "denyCommand",
            rule_input,
            _subscriptions: subscriptions,
        };
        page.read(window, cx);
        page
    }

    fn read(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.workspace = Ide::global(cx).read(cx).workspace.as_ref().map(|workspace| workspace.workspace_id.clone());
        self.call("security.status", json!({ "workspaceId": self.workspace }), None, false, window, cx);
    }

    fn add_rule(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(workspace) = self.workspace.clone() else { return };
        let rule = self.rule_input.read(cx).value().trim().to_owned();
        if rule.is_empty() {
            window.push_notification(gpui_kit::component::notification::Notification::info(tr(cx, "native.securityRuleEmpty")), cx);
            return;
        }
        let kind = self.rule_kind;
        self.rule_input.update(cx, |state, cx| state.set_value("", window, cx));
        self.call("security.addRule", json!({ "workspaceId": workspace, "kind": kind, "rule": rule }), Some("settings.settingsApplied"), false, window, cx);
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
                        let prev = this.status.as_ref();
                        let rules = prev.filter(|_| keep_rules).map(|s| s.rules.clone());
                        let trusted = if keep_rules { prev.map_or(status.trusted, |s| s.trusted) } else { status.trusted };
                        let trusted_workspaces = if keep_rules && status.trusted_workspaces.is_empty() {
                            prev.map_or_else(Vec::new, |s| s.trusted_workspaces.clone())
                        } else {
                            status.trusted_workspaces
                        };
                        let workspace_path = status.workspace_path.or_else(|| prev.and_then(|s| s.workspace_path.clone()));
                        this.status = Some(Status {
                            default_mode: status.default_mode,
                            rules: rules.unwrap_or(status.rules),
                            trusted,
                            workspace_path,
                            trusted_workspaces,
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

    fn rule_kind_label(kind: &str) -> &'static str {
        RULE_KINDS.iter().find(|(k, _)| *k == kind).map_or("native.securityRuleKindCommand", |(_, label)| *label)
    }

    fn render_rule_rows(
        &self,
        workspace: &str,
        rules: Vec<(&'static str, String, bool)>,
        id_prefix: &'static str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        if rules.is_empty() {
            return div().px_4().py_2().text_xs().text_color(theme.muted_foreground).child(tr(cx, "native.securityNoRules")).into_any_element();
        }
        let rows: Vec<AnyElement> = rules
            .into_iter()
            .enumerate()
            .map(|(index, (kind, rule, is_deny))| {
                let shown = if kind == "tool" || kind == "denyTool" {
                    rule.trim_start_matches("web_fetch:").trim_start_matches("mcp__").replacen("__", ": ", 1)
                } else {
                    rule.clone()
                };
                let badge_bg = if is_deny { theme.danger.opacity(0.14) } else { theme.muted };
                let badge_fg = if is_deny { theme.danger } else { theme.muted_foreground };
                let workspace = workspace.to_owned();
                h_flex()
                    .px_4()
                    .py_2()
                    .gap_3()
                    .items_center()
                    .child(
                        div()
                            .flex_shrink_0()
                            .px_1p5()
                            .py_0p5()
                            .rounded(theme.radius)
                            .bg(badge_bg)
                            .text_xs()
                            .text_color(badge_fg)
                            .child(tr(cx, Self::rule_kind_label(kind))),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_sm()
                            .font_family(theme.mono_font_family.clone())
                            .child(shown),
                    )
                    .child(
                        Button::new((id_prefix, index))
                            .ghost()
                            .xsmall()
                            .label(tr(cx, "native.securityRemoveRule"))
                            .disabled(self.busy)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.call("security.removeRule", json!({ "workspaceId": workspace, "kind": kind, "rule": rule }), None, false, window, cx);
                            })),
                    )
                    .into_any_element()
            })
            .collect();
        group(cx, None, rows).into_any_element()
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

        // Workspace Trust section.
        page = page
            .child(div().text_sm().font_semibold().child(tr(cx, "native.securityTrustedTitle")))
            .child(div().text_xs().text_color(theme.muted_foreground).child(tr(cx, "native.securityTrustedDescription")));
        let mut trust_rows: Vec<AnyElement> = Vec::new();
        if let Some(workspace) = self.workspace.clone() {
            let trusted = status.trusted;
            let ws_path = status.workspace_path.clone().unwrap_or_else(|| workspace.clone());
            let toggle_ws = workspace.clone();
            trust_rows.push(
                h_flex()
                    .px_4()
                    .py_2p5()
                    .gap_3()
                    .items_center()
                    .child(
                        Icon::new(if trusted { IconName::Check } else { IconName::TriangleAlert })
                            .small()
                            .text_color(if trusted { theme.success } else { theme.warning }),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(div().text_sm().font_medium().child(tr(cx, if trusted { "native.securityCurrentTrusted" } else { "native.securityCurrentUntrusted" })))
                            .child(div().text_xs().font_family(theme.mono_font_family.clone()).text_color(theme.muted_foreground).truncate().child(ws_path)),
                    )
                    .child(
                        Button::new("trust-toggle-active")
                            .small()
                            .map(|b| if trusted { b.outline() } else { b.primary() })
                            .label(tr(cx, if trusted { "native.securityUntrustCurrent" } else { "native.securityTrustCurrent" }))
                            .disabled(self.busy)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.call(
                                    "settings.trustWorkspace",
                                    json!({ "workspaceId": toggle_ws, "trusted": !trusted }),
                                    Some("settings.settingsApplied"),
                                    false,
                                    window,
                                    cx,
                                );
                            })),
                    )
                    .into_any_element(),
            );
        }
        if status.trusted_workspaces.is_empty() {
            trust_rows.push(
                div().px_4().py_2().text_xs().text_color(theme.muted_foreground).child(tr(cx, "native.securityNoTrusted")).into_any_element(),
            );
        } else {
            for (index, path) in status.trusted_workspaces.iter().enumerate() {
                let path_val = path.clone();
                trust_rows.push(
                    h_flex()
                        .px_4()
                        .py_2()
                        .gap_3()
                        .items_center()
                        .child(div().flex_1().min_w_0().truncate().text_xs().font_family(theme.mono_font_family.clone()).child(path.clone()))
                        .child(
                            Button::new(("trusted-remove", index))
                                .ghost()
                                .xsmall()
                                .label(tr(cx, "native.securityUntrustCurrent"))
                                .disabled(self.busy)
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.call(
                                        "settings.trustWorkspace",
                                        json!({ "path": path_val, "trusted": false, "workspaceId": this.workspace }),
                                        None,
                                        false,
                                        window,
                                        cx,
                                    );
                                })),
                        )
                        .into_any_element(),
                );
            }
        }
        page = page.child(group(cx, None, trust_rows));

        // Permission Rules management section (Allow / Wildcard / Deny).
        page = page
            .child(div().text_sm().font_semibold().child(tr(cx, "native.securityRulesTitle")))
            .child(div().text_xs().text_color(theme.muted_foreground).child(tr(cx, "native.securityRulesDescription")));
        let Some(workspace) = self.workspace.clone() else {
            return page.child(notice(cx, tr(cx, "native.securityNoWorkspace"), false));
        };
        let selected_kind = self.rule_kind;
        let entity = cx.entity();
        let kind_picker = Button::new("security-rule-kind")
            .outline()
            .small()
            .dropdown_caret(true)
            .label(tr(cx, Self::rule_kind_label(selected_kind)))
            .disabled(self.busy)
            .dropdown_menu(move |mut menu, _, cx| {
                for (kind, label_key) in RULE_KINDS {
                    let entity = entity.clone();
                    menu = menu.item(PopupMenuItem::new(tr(cx, label_key)).checked(selected_kind == kind).on_click(move |_, _, cx| {
                        entity.update(cx, |this, cx| {
                            this.rule_kind = kind;
                            cx.notify();
                        });
                    }));
                }
                menu
            });
        let add_bar = h_flex()
            .gap_2()
            .items_center()
            .child(kind_picker)
            .child(div().flex_1().min_w_0().child(Input::new(&self.rule_input).small()))
            .child(
                Button::new("security-rule-add")
                    .primary()
                    .small()
                    .label(tr(cx, "native.securityAddRule"))
                    .disabled(self.busy)
                    .on_click(cx.listener(|this, _, window, cx| this.add_rule(window, cx))),
            );
        page = page.child(add_bar);

        let deny_rules: Vec<(&'static str, String, bool)> = status
            .rules
            .deny_commands
            .iter()
            .map(|r| ("denyCommand", r.clone(), true))
            .chain(status.rules.deny_paths.iter().map(|r| ("denyPath", r.clone(), true)))
            .chain(status.rules.deny_tools.iter().map(|r| ("denyTool", r.clone(), true)))
            .collect();
        let allow_rules: Vec<(&'static str, String, bool)> = status
            .rules
            .commands
            .iter()
            .map(|r| ("command", r.clone(), false))
            .chain(status.rules.tools.iter().map(|r| ("tool", r.clone(), false)))
            .collect();

        page.child(div().text_xs().font_semibold().text_color(theme.danger).child(tr(cx, "native.securityDenyRules")))
            .child(self.render_rule_rows(&workspace, deny_rules, "deny-rule-remove", cx))
            .child(div().text_xs().font_semibold().text_color(theme.muted_foreground).child(tr(cx, "native.securityAllowRules")))
            .child(self.render_rule_rows(&workspace, allow_rules, "allow-rule-remove", cx))
    }
}
