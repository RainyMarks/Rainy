//! The cards under the model list: AI in the editor (`EditorAiSettings.tsx`), web search (`WebSearchSettings.tsx`) and
//! the network proxy for model requests (`ProxySettings.tsx`).

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, StyledExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use rainy_protocol::{ModelsStatus, UiPreferences};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{row, set_prefs};
use crate::store::Store;
use crate::ui::{tr, trf};

/// A bordered card with a title and an optional note.
pub(super) fn card(cx: &App, title: &str, note: Option<&str>, body: Vec<AnyElement>) -> AnyElement {
    let theme = cx.theme();
    v_flex()
        .gap_2()
        .p_4()
        .rounded(theme.radius_lg)
        .border_1()
        .border_color(theme.border)
        .bg(theme.group_box)
        .child(div().text_sm().font_semibold().child(tr(cx, title)))
        .when_some(note, |this, note| this.child(div().text_xs().text_color(theme.muted_foreground).child(tr(cx, note))))
        .children(body)
        .into_any_element()
}

/// A line of status or error text inside a card.
pub(super) fn notice(cx: &App, text: SharedString, error: bool) -> AnyElement {
    let theme = cx.theme();
    div()
        .px_3()
        .py_2()
        .rounded(theme.radius)
        .text_sm()
        .bg(if error { theme.danger.opacity(0.1) } else { theme.muted })
        .text_color(if error { theme.danger } else { theme.foreground })
        .child(text)
        .into_any_element()
}

/// Ghost-text suggestions in the editor and the model behind them.
pub fn editor_ai(prefs: &UiPreferences, status: Option<&ModelsStatus>, cx: &App) -> AnyElement {
    let models: Vec<(String, String)> = status
        .map(|status| {
            status
                .models
                .iter()
                .map(|model| {
                    let name = status
                        .presets
                        .iter()
                        .find(|preset| preset.model.provider == model.provider && preset.model.model == model.model)
                        .map(|preset| preset.name.clone())
                        .unwrap_or_else(|| format!("{}/{}", model.provider, model.model));
                    (model.provider.clone(), name)
                })
                .collect()
        })
        .unwrap_or_default();
    // A model removed since it was chosen falls back to the chat model, as the Host does.
    let chosen = models.iter().find(|(provider, _)| *provider == prefs.completion_model).map(|(_, name)| name.clone());
    let label: SharedString = chosen.map(Into::into).unwrap_or_else(|| tr(cx, "settings.editorAiSameModel"));
    let current = prefs.completion_model.clone();
    let same = tr(cx, "settings.editorAiSameModel");
    let picker = Button::new("completion-model").outline().small().label(label).dropdown_menu(move |mut menu, _, _| {
        menu = menu.item(PopupMenuItem::new(same.clone()).checked(current.is_empty()).on_click(|_, window, cx| {
            set_prefs(json!({ "completionModel": "" }), window, cx);
        }));
        for (provider, name) in &models {
            let provider = provider.clone();
            menu = menu.item(PopupMenuItem::new(name.clone()).checked(current == provider).on_click(move |_, window, cx| {
                set_prefs(json!({ "completionModel": provider }), window, cx);
            }));
        }
        menu
    });
    card(cx, "settings.editorAiTitle", None, vec![
        row(
            cx,
            tr(cx, "settings.editorAiSuggest"),
            Some(tr(cx, "settings.editorAiSuggestNote")),
            Switch::new("inline-completion").checked(prefs.inline_completion).on_click(|checked: &bool, window, cx| {
                set_prefs(json!({ "inlineCompletion": *checked }), window, cx);
            }),
        ),
        row(cx, tr(cx, "settings.editorAiModel"), Some(tr(cx, "settings.editorAiModelNote")), picker),
        div().text_xs().text_color(cx.theme().muted_foreground).child(tr(cx, "settings.editorAiEditNote")).into_any_element(),
    ])
}

/// Whether a service address is DeepSeek's official API, whose key web search uses.
fn is_deepseek(base_url: &str) -> bool {
    base_url.split("://").nth(1).and_then(|rest| rest.split(['/', ':']).next()).is_some_and(|host| host.eq_ignore_ascii_case("api.deepseek.com"))
}

/// Web search through DeepSeek's APIs: the switch, and whether a DeepSeek key is there to pay for it.
pub fn web_search(prefs: &UiPreferences, status: Option<&ModelsStatus>, cx: &App) -> AnyElement {
    let keyed = status.is_some_and(|status| {
        status.models.iter().any(|model| is_deepseek(&model.base_url) && status.credentials.contains(&model.provider))
    });
    let mut body = vec![row(
        cx,
        tr(cx, "settings.webSearchSwitch"),
        Some(tr(cx, "settings.webSearchSwitchNote")),
        Switch::new("web-search").checked(prefs.web_search).on_click(|checked: &bool, window, cx| {
            set_prefs(json!({ "webSearch": *checked }), window, cx);
        }),
    )];
    if status.is_some() && prefs.web_search {
        body.push(notice(cx, tr(cx, if keyed { "settings.webSearchReady" } else { "settings.webSearchNoKey" }), false));
    }
    card(cx, "settings.webSearchTitle", Some("settings.webSearchNote"), body)
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
struct ProxySettings {
    mode: String,
    #[serde(default)]
    url: Option<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
struct ProxyStatus {
    settings: ProxySettings,
    effective: Option<String>,
    system: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ProxyTest {
    ok: bool,
    url: String,
    #[serde(default)]
    status: Option<u16>,
    ms: f64,
    #[serde(default)]
    error: Option<String>,
}

/// The network proxy for model requests: follow the system, connect directly, or a typed address; with a test.
pub struct ProxyCard {
    store: Entity<Store>,
    status: Option<ProxyStatus>,
    mode: String,
    url: Entity<InputState>,
    error: Option<SharedString>,
    test: Option<ProxyTest>,
    testing: bool,
    saving: bool,
    _subscriptions: Vec<Subscription>,
}

impl ProxyCard {
    pub fn new(store: Entity<Store>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let url = cx.new(|cx| InputState::new(window, cx).placeholder("127.0.0.1:7890"));
        let subscriptions = vec![
            cx.subscribe(&url, |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.test = None;
                    cx.notify();
                }
            }),
            // The address in effect is the one for the default model's service, so it is read again when models change.
            cx.subscribe_in(&store, window, |this, _, event: &crate::store::StoreEvent, window, cx| {
                if let crate::store::StoreEvent::Host { name, .. } = event
                    && name == "models.changed"
                {
                    this.load(false, window, cx);
                }
            }),
        ];
        let mut card = Self { store, status: None, mode: "system".into(), url, error: None, test: None, testing: false, saving: false, _subscriptions: subscriptions };
        card.load(true, window, cx);
        card
    }

    /// Read `network.status`; `reset` also takes its settings into the controls.
    fn load(&mut self, reset: bool, window: &mut Window, cx: &mut Context<Self>) {
        let call = self.store.read(cx).call("network.status", Value::Null);
        cx.spawn_in(window, async move |this, cx| {
            let result = call.await;
            let _ = this.update_in(cx, |this, window, cx| {
                match result.map(serde_json::from_value::<ProxyStatus>) {
                    Ok(Ok(status)) => {
                        if reset {
                            this.mode = status.settings.mode.clone();
                            let url = status.settings.url.clone().unwrap_or_default();
                            this.url.update(cx, |state, cx| state.set_value(url, window, cx));
                        }
                        this.status = Some(status);
                    }
                    Ok(Err(error)) => this.error = Some(error.to_string().into()),
                    Err(error) => this.error = Some(crate::ui::error_text(cx, &error)),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn choice(&self, cx: &App) -> Value {
        if self.mode == "manual" { json!({ "mode": "manual", "url": self.url.read(cx).value().trim() }) } else { json!({ "mode": self.mode }) }
    }

    fn changed(&self, cx: &App) -> bool {
        let Some(status) = &self.status else { return false };
        status.settings.mode != self.mode || (self.mode == "manual" && status.settings.url.as_deref().unwrap_or_default() != self.url.read(cx).value().trim())
    }

    fn run_test(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let call = self.store.read(cx).call("network.test", self.choice(cx));
        self.testing = true;
        self.test = None;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = call.await;
            let _ = this.update(cx, |this, cx| {
                this.testing = false;
                this.test = Some(match result.map(serde_json::from_value::<ProxyTest>) {
                    Ok(Ok(test)) => test,
                    Ok(Err(error)) => ProxyTest { ok: false, url: String::new(), status: None, ms: 0., error: Some(error.to_string()) },
                    Err(error) => ProxyTest { ok: false, url: String::new(), status: None, ms: 0., error: Some(crate::ui::error_text(cx, &error).to_string()) },
                });
                cx.notify();
            });
        })
        .detach();
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let call = self.store.read(cx).call("network.set", self.choice(cx));
        self.saving = true;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = call.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.saving = false;
                match result.map(serde_json::from_value::<ProxyStatus>) {
                    Ok(Ok(status)) => {
                        let url = status.settings.url.clone().unwrap_or_default();
                        this.url.update(cx, |state, cx| state.set_value(url, window, cx));
                        this.status = Some(status);
                        this.error = None;
                        use gpui_kit::component::WindowExt as _;
                        window.push_notification(tr(cx, "settings.settingsSaved").to_string(), cx);
                    }
                    Ok(Err(error)) => this.error = Some(error.to_string().into()),
                    Err(error) => this.error = Some(crate::ui::error_text(cx, &error)),
                }
                cx.notify();
            });
        })
        .detach();
    }
}

impl Render for ProxyCard {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let loaded = self.status.is_some();
        let manual = self.mode == "manual";
        let empty = manual && self.url.read(cx).value().trim().is_empty();
        let description = self.status.as_ref().map(|status| match &status.effective {
            None => tr(cx, "settings.proxyDirect"),
            Some(url) => trf(cx, "settings.proxyInEffect", &[("url", url)]),
        });
        let current = self.mode.clone();
        let modes = vec![("system", tr(cx, "settings.proxySystem")), ("off", tr(cx, "settings.proxyOff")), ("manual", tr(cx, "settings.proxyManual"))];
        let chooser = h_flex().gap_1().children(modes.into_iter().map(|(value, label)| {
            let button = Button::new(SharedString::from(format!("proxy-{value}"))).small().label(label).disabled(!loaded || self.saving);
            let button = if current == value { button.primary() } else { button.outline() };
            button.on_click(cx.listener(move |this, _, _, cx| {
                this.mode = value.to_owned();
                this.test = None;
                cx.notify();
            }))
        }));
        let mut body = vec![row(cx, tr(cx, "settings.proxyMode"), description, chooser)];
        if let Some(error) = &self.error {
            body.push(notice(cx, error.clone(), true));
        }
        if manual {
            body.push(Input::new(&self.url).into_any_element());
        }
        if self.mode == "system"
            && let Some(status) = &self.status
        {
            let text = match &status.system {
                None => tr(cx, "settings.proxySystemNone"),
                Some(url) => trf(cx, "settings.proxySystemFound", &[("url", url)]),
            };
            body.push(div().text_xs().text_color(theme.muted_foreground).child(text).into_any_element());
        }
        body.push(
            h_flex()
                .gap_2()
                .child(
                    Button::new("proxy-test")
                        .outline()
                        .small()
                        .label(tr(cx, if self.testing { "settings.proxyTesting" } else { "settings.proxyTest" }))
                        .disabled(!loaded || self.testing || empty)
                        .on_click(cx.listener(|this, _, window, cx| this.run_test(window, cx))),
                )
                .child(
                    Button::new("proxy-save")
                        .primary()
                        .small()
                        .label(tr(cx, "settings.settingsSave"))
                        .disabled(!self.changed(cx) || self.saving || empty)
                        .on_click(cx.listener(|this, _, window, cx| this.save(window, cx))),
                )
                .into_any_element(),
        );
        if let Some(test) = &self.test {
            let text = if test.ok {
                trf(cx, "settings.proxyTestOk", &[("url", &test.url), ("status", &test.status.unwrap_or(0)), ("ms", &(test.ms.round() as u64))])
            } else {
                trf(cx, "settings.proxyTestFailed", &[("error", &test.error.clone().unwrap_or_default())])
            };
            body.push(notice(cx, text, !test.ok));
        }
        card(cx, "settings.proxyTitle", Some("settings.proxyNote"), body)
    }
}
