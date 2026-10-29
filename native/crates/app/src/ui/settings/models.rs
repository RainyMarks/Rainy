//! Models: saved models and the default, the model form with presets, discovery and the probe, and the global prompt
//! (`ModelsSection.tsx`, `GlobalPromptSettings.tsx`).

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputState, Textarea, TextareaState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::tag::Tag;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Selectable as _, Sizable as _, StyledExt as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::switch::Switch;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use rainy_protocol::{ModelSetup, ModelsStatus};
use serde_json::{Value, json};

use super::{heading, segmented};
use crate::store::{Store, StoreEvent};
use crate::ui::chat::thinking_label;
use crate::ui::{tr, trf};

const LOCAL_CONTEXT: u64 = 100_000;
const API_CONTEXT: u64 = 1_000_000;
const APIS: [&str; 3] = ["openai-completions", "openai-responses", "anthropic-messages"];
const THINKING_FORMATS: [&str; 3] = ["openai", "deepseek", "qwen"];
const OUTPUT_FIELDS: [&str; 2] = ["max_tokens", "max_completion_tokens"];

fn key_of(provider: &str, model: &str) -> String {
    format!("{provider}/{model}")
}

/// What the last discovery or probe found.
enum Outcome {
    Discovered(Vec<(String, Option<f64>)>),
    Probed { stream: bool, tool_call: bool, text: Option<String> },
}

pub struct ModelsPage {
    store: Entity<Store>,
    provider: Entity<InputState>,
    model: Entity<InputState>,
    base_url: Entity<InputState>,
    context: Entity<InputState>,
    output: Entity<InputState>,
    api_key: Entity<InputState>,
    prompt: Entity<TextareaState>,
    local: bool,
    api: &'static str,
    thinking: String,
    thinking_format: &'static str,
    output_field: &'static str,
    /// Whether the model takes images; the form always sends it, so a saved profile says it outright.
    images: bool,
    show_compatibility: bool,
    /// The saved model the form edits (`provider/model`).
    editing: Option<String>,
    /// The preset loaded into the form and not saved yet.
    draft_preset: Option<String>,
    removing: Option<String>,
    outcome: Option<Outcome>,
    failure: Option<SharedString>,
    busy: bool,
    loaded: bool,
    proxy: Entity<super::models_extra::ProxyCard>,
    strata: Entity<super::strata::StrataCard>,
    _subscriptions: Vec<Subscription>,
}

impl ModelsPage {
    pub fn new(store: Entity<Store>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = |window: &mut Window, cx: &mut Context<Self>| cx.new(|cx| InputState::new(window, cx));
        let api_key = cx.new(|cx| InputState::new(window, cx).masked(true));
        let placeholder = tr(cx, "settings.settingsGlobalPromptPlaceholder");
        let prompt = cx.new(|cx| TextareaState::new(window, cx).auto_grow(3, 10).placeholder(placeholder));
        let subscriptions = vec![cx.subscribe_in(&store, window, |this, _, event: &StoreEvent, window, cx| {
            if *event == StoreEvent::PrefsChanged {
                this.load_saved(window, cx);
            }
            cx.notify();
        })];
        let proxy = cx.new(|cx| super::models_extra::ProxyCard::new(store.clone(), window, cx));
        let strata = cx.new(|cx| super::strata::StrataCard::new(store.clone(), window, cx));
        let mut page = Self {
            proxy,
            strata,
            provider: input(window, cx),
            model: input(window, cx),
            base_url: input(window, cx),
            context: input(window, cx),
            output: input(window, cx),
            api_key,
            prompt,
            store,
            local: true,
            api: "openai-completions",
            thinking: "off".into(),
            thinking_format: "openai",
            output_field: "max_tokens",
            images: false,
            show_compatibility: false,
            editing: None,
            draft_preset: None,
            removing: None,
            outcome: None,
            failure: None,
            busy: false,
            loaded: false,
            _subscriptions: subscriptions,
        };
        page.load_saved(window, cx);
        page
    }

    fn status(&self, cx: &App) -> Option<ModelsStatus> {
        self.store.read(cx).models.clone()
    }

    /// The first time models are known, the form shows the default model, and the prompt the saved prompt.
    fn load_saved(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.loaded {
            return;
        }
        let Some(status) = self.status(cx) else { return };
        self.loaded = true;
        if let Some(prompt) = &status.global_prompt {
            let text = prompt.text.clone();
            self.prompt.update(cx, |state, cx| state.set_value(text, window, cx));
        }
        let selected = status.selected.as_ref().and_then(|selected| {
            status.models.iter().find(|model| model.provider == selected.provider && model.model == selected.model).cloned()
        });
        match selected {
            Some(model) => self.load(&model, Some(key_of(&model.provider, &model.model)), None, window, cx),
            None => self.load_empty(window, cx),
        }
    }

    fn set_input(&self, input: &Entity<InputState>, value: String, window: &mut Window, cx: &mut Context<Self>) {
        input.update(cx, |state, cx| state.set_value(value, window, cx));
    }

    fn load(&mut self, model: &ModelSetup, editing: Option<String>, preset: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        self.set_input(&self.provider.clone(), model.provider.clone(), window, cx);
        self.set_input(&self.model.clone(), model.model.clone(), window, cx);
        self.set_input(&self.base_url.clone(), model.base_url.clone(), window, cx);
        self.set_input(&self.context.clone(), format!("{}", model.context_window as u64), window, cx);
        let output = model.max_tokens.map(|tokens| format!("{}", tokens as u64)).unwrap_or_default();
        self.set_input(&self.output.clone(), output, window, cx);
        self.set_input(&self.api_key.clone(), String::new(), window, cx);
        self.local = model.local;
        let api = model.api.clone().unwrap_or_else(|| if model.local { "openai-completions".into() } else { "openai-responses".into() });
        self.api = APIS.iter().copied().find(|value| *value == api).unwrap_or("openai-completions");
        self.thinking = model.thinking.clone().unwrap_or_else(|| "off".into());
        self.thinking_format = THINKING_FORMATS.iter().copied().find(|value| Some(*value) == model.thinking_format.as_deref()).unwrap_or("openai");
        self.output_field = OUTPUT_FIELDS.iter().copied().find(|value| Some(*value) == model.max_tokens_field.as_deref()).unwrap_or("max_tokens");
        // The Host reports the setting in effect for saved models and presets; a new model starts without images.
        self.images = model.images.unwrap_or(false);
        self.editing = editing;
        self.draft_preset = preset;
        self.outcome = None;
        self.removing = None;
        self.failure = None;
        cx.notify();
    }

    fn load_empty(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let empty = ModelSetup {
            provider: String::new(),
            base_url: String::new(),
            model: String::new(),
            context_window: LOCAL_CONTEXT as f64,
            max_tokens: None,
            local: true,
            api: Some("openai-completions".into()),
            thinking: Some("off".into()),
            thinking_format: None,
            max_tokens_field: None,
            images: None,
        };
        self.load(&empty, None, None, window, cx);
    }

    fn value(&self, input: &Entity<InputState>, cx: &App) -> String {
        input.read(cx).value().trim().to_owned()
    }

    /// The connection fields discovery needs.
    fn connection(&self, cx: &App) -> Result<Value, SharedString> {
        let (provider, base_url) = (self.value(&self.provider, cx), self.value(&self.base_url, cx));
        if provider.is_empty() || base_url.is_empty() {
            return Err(tr(cx, "settings.settingsRequiredDiscovery"));
        }
        let mut value = json!({ "provider": provider, "baseURL": base_url, "api": self.api });
        let key = self.api_key.read(cx).value().to_string();
        if !key.trim().is_empty() {
            value["apiKey"] = json!(key);
        }
        Ok(value)
    }

    /// The profile the form describes now, for the reasoning levels its model offers.
    fn form_profile(&self, cx: &App) -> ModelSetup {
        ModelSetup {
            provider: self.value(&self.provider, cx),
            base_url: self.value(&self.base_url, cx),
            model: self.value(&self.model, cx),
            context_window: 0.,
            max_tokens: None,
            local: self.local,
            api: Some(self.api.to_owned()),
            thinking: Some(self.thinking.clone()),
            thinking_format: (self.api == "openai-completions").then(|| self.thinking_format.to_owned()),
            max_tokens_field: None,
            images: None,
        }
    }

    /// The whole form as `ModelSetupInput`.
    fn setup(&self, cx: &App) -> Result<Value, SharedString> {
        let mut value = self.connection(cx).map_err(|_| tr(cx, "settings.settingsRequiredModel"))?;
        let (model, context, output) = (self.value(&self.model, cx), self.value(&self.context, cx), self.value(&self.output, cx));
        if model.is_empty() || context.is_empty() {
            return Err(tr(cx, "settings.settingsRequiredModel"));
        }
        let positive = |text: &str| text.parse::<u64>().ok().filter(|value| *value > 0);
        let Some(context) = positive(&context) else { return Err(tr(cx, "settings.settingsInvalidNumber")) };
        let output = if output.is_empty() { None } else { Some(positive(&output).ok_or_else(|| tr(cx, "settings.settingsInvalidNumber"))?) };
        value["model"] = json!(model);
        value["contextWindow"] = json!(context);
        value["local"] = json!(self.local);
        value["thinking"] = json!(self.thinking);
        value["images"] = json!(self.images);
        if let Some(output) = output {
            value["maxTokens"] = json!(output);
        }
        if self.api == "openai-completions" {
            value["thinkingFormat"] = json!(self.thinking_format);
            value["maxTokensField"] = json!(self.output_field);
        }
        Ok(value)
    }

    /// Run one Host request at a time; `done` gets the result, a failure shows under the form.
    fn request(&mut self, method: &'static str, params: Value, done: impl FnOnce(&mut Self, Value, &mut Window, &mut Context<Self>) + 'static, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.busy = true;
        self.failure = None;
        cx.notify();
        let request = self.store.read(cx).call(method, params);
        cx.spawn_in(window, async move |this, cx| {
            let result = request.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                match result {
                    Ok(value) => done(this, value, window, cx),
                    Err(error) => this.failure = Some(crate::ui::error_text(cx, &error)),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn accept_status(&mut self, value: Value, cx: &mut Context<Self>) {
        if let Ok(status) = serde_json::from_value::<ModelsStatus>(value) {
            self.store.update(cx, |store, cx| {
                store.models = Some(status);
                cx.notify();
            });
        }
    }

    fn refresh_status(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let request = self.store.read(cx).call("models.status", Value::Null);
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(value) = request.await {
                let _ = this.update(cx, |this, cx| this.accept_status(value, cx));
            }
        })
        .detach();
    }

    fn notify_success(&self, key: &str, window: &mut Window, cx: &mut App) {
        window.push_notification(Notification::success(tr(cx, key)), cx);
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut setup = match self.setup(cx) {
            Ok(setup) => setup,
            Err(error) => {
                self.failure = Some(error);
                cx.notify();
                return;
            }
        };
        if let Some(editing) = &self.editing {
            setup["previousProvider"] = json!(editing.split('/').next().unwrap_or_default());
        }
        self.request("models.configure", setup, |this, saved, window, cx| {
            let provider = saved.get("provider").and_then(Value::as_str).unwrap_or_default();
            let model = saved.get("model").and_then(Value::as_str).unwrap_or_default();
            this.editing = Some(key_of(provider, model));
            this.draft_preset = None;
            this.outcome = None;
            let key = this.api_key.clone();
            key.update(cx, |state, cx| state.set_value("", window, cx));
            this.refresh_status(window, cx);
            this.notify_success("settings.settingsSaved", window, cx);
        }, window, cx);
    }

    fn select(&mut self, model: &ModelSetup, thinking: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let mut params = json!({ "provider": model.provider, "model": model.model });
        if let Some(thinking) = thinking {
            params["thinking"] = json!(thinking);
        }
        self.removing = None;
        self.request("models.select", params, |this, status, window, cx| {
            this.accept_status(status, cx);
            this.notify_success("settings.settingsApplied", window, cx);
        }, window, cx);
    }

    fn remove(&mut self, model: &ModelSetup, window: &mut Window, cx: &mut Context<Self>) {
        let key = key_of(&model.provider, &model.model);
        if self.removing.as_deref() != Some(key.as_str()) {
            self.removing = Some(key);
            cx.notify();
            return;
        }
        self.removing = None;
        self.request("models.remove", json!({ "provider": model.provider }), move |this, status, window, cx| {
            this.accept_status(status, cx);
            if this.editing.as_deref() == Some(key.as_str()) {
                this.load_empty(window, cx);
            }
        }, window, cx);
    }

    fn render_list(&self, status: &ModelsStatus, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let name_of = |key: &str| {
            status.presets.iter().find(|preset| key_of(&preset.model.provider, &preset.model.model) == key).map_or_else(|| key.to_owned(), |preset| preset.name.clone())
        };
        let mut rows: Vec<AnyElement> = Vec::new();
        for model in &status.models {
            let key = key_of(&model.provider, &model.model);
            let default = status.selected.as_ref().is_some_and(|selected| key_of(&selected.provider, &selected.model) == key);
            let kind = tr(cx, if model.local { "settings.settingsLocalModel" } else { "settings.settingsApiModel" });
            let detail = trf(cx, "settings.modelContext", &[("kind", &kind), ("context", &(model.context_window as u64))]);
            // The default model's level, as chats start with it; only a model with levels to choose from shows it.
            let reasoning = model.reasoning();
            let toggle = reasoning.toggle();
            let current = reasoning.choose(status.selected.as_ref().and_then(|selected| selected.thinking.as_deref()).or(model.thinking.as_deref()));
            let (select_model, edit_model, remove_model) = (model.clone(), model.clone(), model.clone());
            let edit_key = key.clone();
            let thinking_model = model.clone();
            let page = cx.weak_entity();
            let locale = self.store.read(cx).locale();
            let actions = h_flex()
                .gap_1()
                .when_some(current.filter(|_| default && reasoning.adjustable()), |this, current| {
                    this.child(
                        Button::new(ElementId::Name(format!("thinking-{key}").into()))
                            .small()
                            .outline()
                            .label(thinking_label(locale, current, toggle))
                            .tooltip(tr(cx, "chat.thinkingTitle"))
                            .dropdown_caret(true)
                            .dropdown_menu(move |mut menu, _, _| {
                                for level in reasoning.levels() {
                                    let (page, model) = (page.clone(), thinking_model.clone());
                                    menu = menu.item(PopupMenuItem::new(thinking_label(locale, level, toggle)).checked(level == current).on_click(move |_, window, cx| {
                                        let _ = page.update(cx, |this, cx| this.select(&model, Some(level.as_str().to_owned()), window, cx));
                                    }));
                                }
                                menu
                            }),
                    )
                })
                .when(!default, |this| {
                    this.child(
                        Button::new(ElementId::Name(format!("default-{key}").into()))
                            .small()
                            .outline()
                            .label(tr(cx, "settings.modelUseDefault"))
                            .disabled(self.busy)
                            .on_click(cx.listener(move |this, _, window, cx| this.select(&select_model, None, window, cx))),
                    )
                })
                .child(
                    Button::new(ElementId::Name(format!("edit-{key}").into()))
                        .small()
                        .ghost()
                        .label(tr(cx, "settings.modelEdit"))
                        .on_click(cx.listener(move |this, _, window, cx| this.load(&edit_model, Some(edit_key.clone()), None, window, cx))),
                )
                .child(
                    Button::new(ElementId::Name(format!("remove-{key}").into()))
                        .small()
                        .ghost()
                        .label(tr(cx, if self.removing.as_deref() == Some(key.as_str()) { "settings.modelRemoveConfirm" } else { "settings.modelRemove" }))
                        .disabled(self.busy)
                        .on_click(cx.listener(move |this, _, window, cx| this.remove(&remove_model, window, cx))),
                );
            rows.push(self.item(name_of(&key).into(), detail, default, status.credentials.contains(&model.provider), false, self.editing.as_deref() == Some(key.as_str()), actions.into_any_element(), cx));
        }
        // Presets whose provider is not saved yet: one profile is kept per provider.
        for preset in status.presets.iter().filter(|preset| !status.models.iter().any(|model| model.provider == preset.model.provider)) {
            let kind = tr(cx, if preset.model.local { "settings.settingsLocalModel" } else { "settings.settingsApiModel" });
            let detail = trf(cx, "settings.modelContext", &[("kind", &kind), ("context", &(preset.model.context_window as u64))]);
            let (model, name) = (preset.model.clone(), preset.name.clone());
            let action = Button::new(ElementId::Name(format!("preset-{}", preset.name).into()))
                .small()
                .outline()
                .label(tr(cx, "settings.modelLoadPreset"))
                .on_click(cx.listener(move |this, _, window, cx| this.load(&model, None, Some(name.clone()), window, cx)));
            rows.push(self.item(preset.name.clone().into(), detail, false, false, true, self.draft_preset.as_deref() == Some(preset.name.as_str()), action.into_any_element(), cx));
        }
        v_flex()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.border)
            .bg(theme.group_box)
            .when(status.models.is_empty(), |this| {
                this.child(div().px_4().py_3().text_sm().text_color(theme.muted_foreground).child(tr(cx, "settings.modelsEmpty")))
            })
            .children(rows.into_iter().enumerate().map(|(index, row)| div().when(index > 0, |this| this.border_t_1().border_color(theme.border)).child(row)))
            .into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn item(&self, title: SharedString, detail: SharedString, default: bool, key_stored: bool, preset: bool, current: bool, actions: AnyElement, cx: &App) -> AnyElement {
        let theme = cx.theme();
        h_flex()
            .px_4()
            .py_2p5()
            .gap_3()
            .items_center()
            .when(current, |this| this.bg(theme.list_active))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_0p5()
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(div().text_sm().font_medium().child(title))
                            .when(default, |this| this.child(Tag::success().small().child(tr(cx, "settings.modelDefault"))))
                            .when(key_stored, |this| this.child(Tag::secondary().small().child(tr(cx, "settings.modelKeyStored"))))
                            .when(preset, |this| this.child(Tag::secondary().small().child(tr(cx, "settings.modelPreset")))),
                    )
                    .child(div().text_xs().text_color(theme.muted_foreground).child(detail)),
            )
            .child(actions)
            .into_any_element()
    }

    fn field(&self, label: SharedString, input: &Entity<InputState>, cx: &App) -> impl IntoElement {
        v_flex().gap_1().flex_1().min_w(px(200.)).child(div().text_xs().text_color(cx.theme().muted_foreground).child(label)).child(Input::new(input).small())
    }

    fn choice(&self, id: &str, label: SharedString, current: &str, items: Vec<(String, SharedString)>, change: impl Fn(&mut Self, String) + 'static, cx: &mut Context<Self>) -> impl IntoElement {
        let shown = items.iter().find(|(value, _)| value == current).map_or_else(|| SharedString::from(current.to_owned()), |(_, label)| label.clone());
        let page = cx.weak_entity();
        let change = std::rc::Rc::new(change);
        let current = current.to_owned();
        v_flex().gap_1().flex_1().min_w(px(200.)).child(div().text_xs().text_color(cx.theme().muted_foreground).child(label)).child(
            Button::new(ElementId::Name(id.to_owned().into())).small().outline().label(shown).dropdown_caret(true).dropdown_menu(move |mut menu, _, _| {
                for (value, label) in &items {
                    let (page, change, value) = (page.clone(), change.clone(), value.clone());
                    menu = menu.item(PopupMenuItem::new(label.clone()).checked(*value == current).on_click(move |_, _, cx| {
                        let value = value.clone();
                        let change = change.clone();
                        let _ = page.update(cx, |this, cx| {
                            change(this, value);
                            cx.notify();
                        });
                    }));
                }
                menu
            }),
        )
    }

    fn render_form(&self, status: Option<&ModelsStatus>, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let provider = self.value(&self.provider, cx);
        let key_stored = status.is_some_and(|status| status.credentials.contains(&provider));
        let key_label = if !key_stored {
            if self.local { "settings.settingsApiKey" } else { "settings.settingsApiKeyRequired" }
        } else if status.and_then(|status| status.keys_encrypted) == Some(true) {
            "settings.settingsApiKeyEncrypted"
        } else {
            "settings.settingsApiKeyStored"
        };
        let title = match (&self.editing, &self.draft_preset) {
            (Some(editing), _) => trf(cx, "settings.modelEditTitle", &[("name", editing)]),
            (None, Some(preset)) => trf(cx, "settings.settingsPreset", &[("name", preset)]),
            _ => tr(cx, "settings.settingsAddModel"),
        };
        // The levels of the model the form describes now, saved or not: the same table the Host and the composer use.
        let reasoning = self.form_profile(cx).reasoning();
        let locale = self.store.read(cx).locale();
        let levels: Vec<(String, SharedString)> = reasoning.levels().map(|level| (level.as_str().to_owned(), thinking_label(locale, level, reasoning.toggle()).into())).collect();
        let shown_thinking = reasoning.choose(Some(&self.thinking)).map_or_else(|| self.thinking.clone(), |level| level.as_str().to_owned());
        let local = self.local;
        let mut form = v_flex()
            .gap_3()
            .p_4()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.border)
            .bg(theme.group_box)
            .child(div().text_sm().font_semibold().child(title))
            .child(segmented(
                "model-kind",
                vec![("local", tr(cx, "settings.settingsLocalModel")), ("api", tr(cx, "settings.settingsApiModel"))],
                if local { "local" } else { "api" },
                {
                    let page = cx.weak_entity();
                    move |value, window, cx| {
                        let _ = page.update(cx, |this, cx| {
                            let local = value == "local";
                            if this.local != local {
                                this.local = local;
                                this.api = if local { "openai-completions" } else { "openai-responses" };
                                if this.editing.is_none() && this.draft_preset.is_none() {
                                    let context = this.context.clone();
                                    let value = format!("{}", if local { LOCAL_CONTEXT } else { API_CONTEXT });
                                    context.update(cx, |state, cx| state.set_value(value, window, cx));
                                }
                                cx.notify();
                            }
                        });
                    }
                },
            ))
            .child(h_flex().gap_3().flex_wrap().child(self.field(tr(cx, "settings.settingsProvider"), &self.provider, cx)).child(self.field(tr(cx, "settings.settingsModelId"), &self.model, cx)))
            .child(self.field(tr(cx, "settings.settingsBaseUrl"), &self.base_url, cx))
            .child(h_flex().gap_3().flex_wrap().child(self.field(tr(cx, "settings.settingsContext"), &self.context, cx)).child(self.field(tr(cx, "settings.settingsOutput"), &self.output, cx)))
            .child(self.field(tr(cx, key_label), &self.api_key, cx))
            .child(
                h_flex()
                    .gap_3()
                    .items_center()
                    .child(Switch::new("model-images").checked(self.images).on_click(cx.listener(|this, checked: &bool, _, cx| {
                        this.images = *checked;
                        cx.notify();
                    })))
                    .child(
                        v_flex()
                            .flex_1()
                            .child(div().text_sm().child(tr(cx, "settings.modelImages")))
                            .child(div().text_xs().text_color(theme.muted_foreground).child(tr(cx, "settings.modelImagesHint"))),
                    ),
            )
            .child(
                div()
                    .id("compatibility")
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.show_compatibility = !this.show_compatibility;
                        cx.notify();
                    }))
                    .child(format!("{} {}", if self.show_compatibility { "▾" } else { "▸" }, tr(cx, "settings.settingsCompatibility"))),
            );
        if self.show_compatibility {
            let apis: Vec<(String, SharedString)> = APIS.iter().map(|api| ((*api).to_owned(), SharedString::from(*api))).collect();
            let formats: Vec<(String, SharedString)> = THINKING_FORMATS.iter().map(|value| ((*value).to_owned(), SharedString::from(*value))).collect();
            let fields: Vec<(String, SharedString)> = OUTPUT_FIELDS.iter().map(|value| ((*value).to_owned(), SharedString::from(*value))).collect();
            let thinking: AnyElement = if reasoning.adjustable() {
                self.choice("model-thinking", tr(cx, "settings.settingsThinking"), &shown_thinking, levels, |this, value| this.thinking = value, cx).into_any_element()
            } else {
                let note = if reasoning.reasons() { "chat.thinkingFixed" } else { "chat.thinkingNone" };
                v_flex()
                    .gap_1()
                    .flex_1()
                    .min_w(px(200.))
                    .child(div().text_xs().text_color(theme.muted_foreground).child(tr(cx, "settings.settingsThinking")))
                    .child(div().text_sm().text_color(theme.muted_foreground).child(tr(cx, note)))
                    .into_any_element()
            };
            let mut row = h_flex()
                .gap_3()
                .flex_wrap()
                .child(self.choice("model-api", tr(cx, "settings.settingsProtocol"), self.api, apis, |this, value| {
                    this.api = APIS.iter().copied().find(|api| *api == value).unwrap_or("openai-completions");
                }, cx))
                .child(thinking);
            if self.api == "openai-completions" {
                row = row
                    .child(self.choice("model-thinking-format", tr(cx, "settings.settingsThinkingFormat"), self.thinking_format, formats, |this, value| {
                        this.thinking_format = THINKING_FORMATS.iter().copied().find(|format| *format == value).unwrap_or("openai");
                    }, cx))
                    .child(self.choice("model-output-field", tr(cx, "settings.settingsOutputField"), self.output_field, fields, |this, value| {
                        this.output_field = OUTPUT_FIELDS.iter().copied().find(|field| *field == value).unwrap_or("max_tokens");
                    }, cx));
            }
            form = form.child(row);
        }
        form = form.child(
            h_flex()
                .gap_2()
                .child(Button::new("model-save").primary().small().label(tr(cx, "settings.settingsSaveModel")).loading(self.busy).on_click(cx.listener(|this, _, window, cx| this.save(window, cx))))
                .child(Button::new("model-discover").outline().small().label(tr(cx, "settings.settingsDiscoverModels")).disabled(self.busy).on_click(cx.listener(|this, _, window, cx| {
                    match this.connection(cx) {
                        Ok(params) => this.request("models.discover", params, |this, value, _, _| {
                            let models = value.as_array().cloned().unwrap_or_default().into_iter().filter_map(|model| {
                                Some((model.get("id")?.as_str()?.to_owned(), model.get("contextWindow").and_then(Value::as_f64)))
                            }).collect();
                            this.outcome = Some(Outcome::Discovered(models));
                        }, window, cx),
                        Err(error) => {
                            this.failure = Some(error);
                            cx.notify();
                        }
                    }
                })))
                .child(Button::new("model-probe").outline().small().label(tr(cx, "settings.settingsProbeModel")).disabled(self.busy).on_click(cx.listener(|this, _, window, cx| {
                    match this.setup(cx) {
                        Ok(params) => this.request("models.probe", params, |this, value, _, _| {
                            this.outcome = Some(Outcome::Probed {
                                stream: value.get("stream").and_then(Value::as_bool).unwrap_or(false),
                                tool_call: value.get("toolCall").and_then(Value::as_bool).unwrap_or(false),
                                text: value.get("text").and_then(Value::as_str).map(str::to_owned),
                            });
                        }, window, cx),
                        Err(error) => {
                            this.failure = Some(error);
                            cx.notify();
                        }
                    }
                }))),
        );
        if let Some(failure) = &self.failure {
            form = form.child(div().text_sm().text_color(theme.danger).child(failure.clone()));
        }
        match &self.outcome {
            Some(Outcome::Probed { stream, tool_call, text }) => {
                let verdict = |passed: bool| tr(cx, if passed { "settings.settingsPass" } else { "settings.settingsNotVerified" });
                let mut message = trf(cx, "settings.settingsProbeResult", &[("stream", &verdict(*stream)), ("tools", &verdict(*tool_call))]).to_string();
                if let Some(text) = text.as_ref().filter(|text| !text.is_empty()) {
                    message.push('\n');
                    message.push_str(&trf(cx, "settings.settingsProbeReply", &[("text", text)]));
                }
                form = form.child(div().p_3().rounded(theme.radius).bg(theme.muted).text_sm().whitespace_normal().child(message));
            }
            Some(Outcome::Discovered(models)) if models.is_empty() => {
                form = form.child(div().p_3().rounded(theme.radius).bg(theme.muted).text_sm().child(tr(cx, "settings.settingsNoModels")));
            }
            Some(Outcome::Discovered(models)) => {
                let current = self.value(&self.model, cx);
                form = form.child(
                    v_flex()
                        .gap_2()
                        .p_3()
                        .rounded(theme.radius)
                        .bg(theme.muted)
                        .child(div().text_sm().child(trf(cx, "settings.settingsDiscovered", &[("count", &models.len())])))
                        .child(h_flex().gap_1().flex_wrap().children(models.iter().map(|(id, context)| {
                            let (id, context) = (id.clone(), *context);
                            Button::new(ElementId::Name(format!("found-{id}").into()))
                                .xsmall()
                                .outline()
                                .selected(current == id)
                                .label(id.clone())
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    let (model, context_input) = (this.model.clone(), this.context.clone());
                                    model.update(cx, |state, cx| state.set_value(id.clone(), window, cx));
                                    if let (Some(context), true) = (context, this.editing.is_none()) {
                                        context_input.update(cx, |state, cx| state.set_value(format!("{}", context as u64), window, cx));
                                    }
                                    cx.notify();
                                }))
                        }))),
                );
            }
            None => {}
        }
        form.into_any_element()
    }

    fn render_prompt(&self, status: Option<&ModelsStatus>, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let max = status.and_then(|status| status.global_prompt.as_ref().map(|prompt| prompt.max_chars)).unwrap_or(4000);
        let count = self.prompt.read(cx).value().chars().count();
        v_flex()
            .gap_2()
            .p_4()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.border)
            .bg(theme.group_box)
            .child(div().text_sm().font_semibold().child(tr(cx, "settings.settingsGlobalPrompt")))
            .child(div().text_xs().text_color(theme.muted_foreground).child(tr(cx, "settings.settingsGlobalPromptNote")))
            .child(Textarea::new(&self.prompt))
            .child(
                h_flex()
                    .justify_between()
                    .items_center()
                    .child(div().text_xs().text_color(if count > max { theme.danger } else { theme.muted_foreground }).child(trf(cx, "settings.settingsGlobalPromptCount", &[("count", &count), ("max", &max)])))
                    .child(Button::new("prompt-save").small().outline().label(tr(cx, "chat.save")).disabled(self.busy || count > max).on_click(cx.listener(|this, _, window, cx| {
                        let text = this.prompt.read(cx).value().to_string();
                        this.request("prompt.global", json!({ "text": text }), |this, status, window, cx| {
                            this.accept_status(status, cx);
                            this.notify_success("settings.settingsSaved", window, cx);
                        }, window, cx);
                    }))),
            )
            .into_any_element()
    }
}

impl Render for ModelsPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.load_saved(window, cx);
        let status = self.status(cx);
        let list = status.as_ref().map(|status| self.render_list(status, cx));
        let form = self.render_form(status.as_ref(), cx);
        let prompt = self.render_prompt(status.as_ref(), cx);
        let prefs = self.store.read(cx).prefs.clone();
        let editor_ai = super::models_extra::editor_ai(&prefs, status.as_ref(), cx);
        let web_search = super::models_extra::web_search(&prefs, status.as_ref(), cx);
        let unreadable = status.as_ref().and_then(|status| status.keys_unreadable.clone());
        let theme = cx.theme();
        v_flex()
            .gap_4()
            .child(heading(cx, "settings.settingsModels", "settings.modelsLead"))
            .when_some(unreadable, |this, text| this.child(div().p_3().rounded(theme.radius).border_1().border_color(theme.danger).text_sm().child(text)))
            .child(
                h_flex()
                    .justify_between()
                    .items_center()
                    .child(div().text_sm().font_semibold().child(tr(cx, "settings.settingsDefaultModel")))
                    .child(Button::new("model-add").small().outline().label(tr(cx, "settings.settingsAddModel")).on_click(cx.listener(|this, _, window, cx| this.load_empty(window, cx)))),
            )
            .children(list)
            .child(form)
            .child(self.strata.clone())
            .child(editor_ai)
            .child(web_search)
            .child(prompt)
            .child(self.proxy.clone())
    }
}
