//! The Strata local model card under the model settings (3.x `StrataSettings.tsx`): the engine's state, the model files
//! and run settings, and start, stop and connect. It starts collapsed; its header keeps the engine's state visible.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _, StyledExt as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use rainy_carrier::strata::{Phase, Settings, Status};

use super::models_extra::notice;
use crate::store::Store;
use crate::ui::{tr, trf};

const CONTEXTS: [u32; 5] = [8192, 32768, 65536, 131072, 262144];
const KV_CACHES: [&str; 3] = ["int8", "q4_0", "k8v4"];

/// The form as typed.
#[derive(Clone, PartialEq)]
struct Form {
    model_path: String,
    mtp_path: String,
    context: String,
    port: String,
    kv_cache: String,
    vram: String,
    budget: String,
}

impl Form {
    fn of(settings: &Settings) -> Self {
        Self {
            model_path: settings.model_path.clone(),
            mtp_path: settings.mtp_path.clone(),
            context: settings.context_window.to_string(),
            port: settings.port.to_string(),
            kv_cache: settings.kv_cache.clone(),
            vram: settings.vram_reserve_mib.to_string(),
            budget: settings.resident_budget_gib.map(|budget| budget.to_string()).unwrap_or_default(),
        }
    }

    /// The settings it describes, when every field is valid.
    fn settings(&self) -> Option<Settings> {
        let budget = self.budget.trim();
        let settings = Settings {
            model_path: self.model_path.trim().to_owned(),
            mtp_path: self.mtp_path.trim().to_owned(),
            context_window: self.context.parse().ok()?,
            port: self.port.trim().parse().ok()?,
            kv_cache: self.kv_cache.clone(),
            vram_reserve_mib: self.vram.trim().parse().ok()?,
            resident_budget_gib: if budget.is_empty() { None } else { Some(budget.parse().ok()?) },
        };
        settings.check().is_ok().then_some(settings)
    }
}

#[derive(Clone, Copy)]
enum Field {
    Model,
    Mtp,
}

pub struct StrataCard {
    store: Entity<Store>,
    open: bool,
    advanced: bool,
    model_path: Entity<InputState>,
    mtp_path: Entity<InputState>,
    port: Entity<InputState>,
    vram: Entity<InputState>,
    budget: Entity<InputState>,
    context: String,
    kv_cache: String,
    /// The settings the form was last filled from.
    shown: Option<Settings>,
    _subscriptions: Vec<Subscription>,
}

impl StrataCard {
    pub fn new(store: Entity<Store>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = |window: &mut Window, cx: &mut Context<Self>| cx.new(|cx| InputState::new(window, cx));
        let subscriptions = vec![cx.observe(&store, |_, _, cx| cx.notify())];
        let defaults = Settings::default();
        Self {
            model_path: input(window, cx),
            mtp_path: input(window, cx),
            port: input(window, cx),
            vram: input(window, cx),
            budget: input(window, cx),
            context: defaults.context_window.to_string(),
            kv_cache: defaults.kv_cache,
            store,
            open: false,
            advanced: false,
            shown: None,
            _subscriptions: subscriptions,
        }
    }

    fn form(&self, cx: &App) -> Form {
        let text = |state: &Entity<InputState>| state.read(cx).value().to_string();
        Form {
            model_path: text(&self.model_path),
            mtp_path: text(&self.mtp_path),
            context: self.context.clone(),
            port: text(&self.port),
            kv_cache: self.kv_cache.clone(),
            vram: text(&self.vram),
            budget: text(&self.budget),
        }
    }

    fn fill(&mut self, form: Form, window: &mut Window, cx: &mut Context<Self>) {
        for (state, value) in [(&self.model_path, form.model_path), (&self.mtp_path, form.mtp_path), (&self.port, form.port), (&self.vram, form.vram), (&self.budget, form.budget)] {
            state.update(cx, |state, cx| state.set_value(value, window, cx));
        }
        self.context = form.context;
        self.kv_cache = form.kv_cache;
    }

    /// Take saved settings into the form unless the user has edited it since it was last filled.
    fn sync(&mut self, status: &Status, window: &mut Window, cx: &mut Context<Self>) {
        if self.shown.as_ref() == Some(&status.settings) {
            return;
        }
        let untouched = self.shown.as_ref().is_none_or(|shown| self.form(cx) == Form::of(shown));
        if untouched {
            self.fill(Form::of(&status.settings), window, cx);
        }
        self.shown = Some(status.settings.clone());
    }

    fn toggle(&mut self, cx: &mut Context<Self>) {
        self.open = !self.open;
        if self.open {
            self.store.update(cx, |store, cx| store.refresh_strata(true, cx));
        }
        cx.notify();
    }

    fn choose(&mut self, field: Field, files: bool, window: &mut Window, cx: &mut Context<Self>) {
        let chosen = cx.prompt_for_paths(PathPromptOptions { files, directories: !files, multiple: false, prompt: None });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = chosen.await else { return };
            let Some(path) = paths.into_iter().next() else { return };
            let _ = this.update_in(cx, |this, window, cx| {
                let state = match field {
                    Field::Model => &this.model_path,
                    Field::Mtp => &this.mtp_path,
                };
                state.update(cx, |state, cx| state.set_value(path.to_string_lossy().into_owned(), window, cx));
                cx.notify();
            });
        })
        .detach();
    }

    /// Show how an operation ended: `success` names the message for a completed one.
    fn finish<T: 'static>(task: Task<Result<T, String>>, success: impl Fn(&T) -> Option<&'static str> + 'static, window: &mut Window, cx: &mut Context<Self>) {
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |_, window, cx| match result {
                Ok(value) => {
                    if let Some(key) = success(&value) {
                        window.push_notification(tr(cx, key).to_string(), cx);
                    }
                }
                Err(message) if !message.is_empty() => window.push_notification(Notification::error(message), cx),
                Err(_) => {}
            });
        })
        .detach();
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(settings) = self.form(cx).settings() else { return };
        let task = self.store.update(cx, |store, cx| store.save_strata(settings, cx));
        // The saved settings (a profile may have filled some in) replace the form.
        self.shown = None;
        Self::finish(task, |_| Some("settings.strataSaved"), window, cx);
    }

    fn start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let task = self.store.update(cx, |store, cx| store.start_strata(cx));
        Self::finish(task, |_| None, window, cx);
    }

    fn stop(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let task = self.store.update(cx, |store, cx| store.stop_strata(cx));
        Self::finish(task, |status: &Status| (status.phase == Phase::Stopped).then_some("settings.strataStopped"), window, cx);
    }

    fn connect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let task = self.store.update(cx, |store, cx| store.connect_strata(cx));
        Self::finish(task, |_| Some("settings.strataConnected"), window, cx);
    }

    fn phase_key(phase: Phase) -> &'static str {
        match phase {
            Phase::Unconfigured => "settings.strataUnconfigured",
            Phase::Stopped => "settings.strataStoppedState",
            Phase::Preparing => "settings.strataPreparing",
            Phase::Starting => "settings.strataStarting",
            Phase::Running => "settings.strataRunning",
            Phase::Stopping => "settings.strataStopping",
            Phase::External => "settings.strataExternal",
            Phase::Error => "settings.strataError",
        }
    }

    fn header(&self, phase: Option<Phase>, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        h_flex()
            .id("strata-header")
            .justify_between()
            .items_center()
            .cursor_pointer()
            .child(div().text_sm().font_semibold().child(tr(cx, "settings.strataTitle")))
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .when_some(phase, |this, phase| this.child(tr(cx, Self::phase_key(phase))))
                    .child(Icon::new(if self.open { IconName::ChevronUp } else { IconName::ChevronDown }).small()),
            )
            .on_click(cx.listener(|this, _, _, cx| this.toggle(cx)))
            .into_any_element()
    }

    fn field(&self, label: &'static str, state: &Entity<InputState>, locked: bool, cx: &App) -> AnyElement {
        v_flex()
            .gap_1()
            .child(div().text_xs().text_color(cx.theme().muted_foreground).child(tr(cx, label)))
            .child(Input::new(state).disabled(locked))
            .into_any_element()
    }

    fn render_form(&self, status: &Status, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = cx.theme().clone();
        let store = self.store.read(cx);
        let pending = store.strata.pending;
        let loading = store.strata.loading;
        let saved_context = store.models.as_ref().and_then(|models| models.models.iter().find(|model| model.provider == "rainy-strata")).map(|model| model.context_window as u64);
        let module = store.modules.statuses.as_ref().and_then(|statuses| statuses.as_ref().ok()).and_then(|statuses| statuses.iter().find(|status| status.id == "strata").cloned());
        let module_busy = store.modules.busy || store.modules.running();
        let form = self.form(cx);
        let dirty = form != Form::of(&status.settings);
        let valid = form.settings().is_some();
        let preparing = matches!(status.phase, Phase::Preparing | Phase::Starting) || pending == Some("start");
        let active = preparing || matches!(status.phase, Phase::Running | Phase::Stopping);
        let locked = pending.is_some() || active;
        let can_stop = preparing || (status.phase == Phase::Running && status.server.as_ref().is_some_and(|server| server.owned));
        let muted = |text: SharedString| div().text_xs().text_color(theme.muted_foreground).child(text).into_any_element();
        let mut body = Vec::new();

        let runtime_line = if status.runtime.available {
            let version = status.runtime.version.as_deref().map(|version| format!(" · {version}")).unwrap_or_default();
            muted(format!("{}{version}", tr(cx, "settings.strataRuntimeReady")).into())
        } else {
            notice(cx, tr(cx, "settings.strataRuntimeMissing"), true)
        };
        let store_handle = self.store.clone();
        body.push(
            h_flex()
                .gap_2()
                .items_center()
                .child(div().flex_1().child(runtime_line))
                .when(!status.runtime.available, |this| {
                    this.when_some(module.filter(|module| !module.installed), |this, module| {
                        let size = crate::ui::settings::runtime::file_size_text(module.download_bytes);
                        let store = store_handle.clone();
                        this.child(
                            Button::new("strata-download")
                                .primary()
                                .small()
                                .label(trf(cx, "settings.modulesDownload", &[("size", &size)]))
                                .disabled(module_busy)
                                .on_click(move |_, _, cx| store.update(cx, |store, cx| store.install_module("strata", cx))),
                        )
                    })
                })
                .child(
                    Button::new("strata-refresh")
                        .outline()
                        .small()
                        .label(tr(cx, "settings.settingsRefresh"))
                        .disabled(loading || pending.is_some())
                        .on_click(cx.listener(|this, _, _, cx| this.store.update(cx, |store, cx| store.refresh_strata(true, cx)))),
                )
                .into_any_element(),
        );
        body.push(muted(tr(cx, "settings.strataSupported")));
        body.push(self.field("settings.strataModelPath", &self.model_path, locked, cx));
        body.push(
            h_flex()
                .gap_2()
                .flex_wrap()
                .child(Button::new("strata-gguf").outline().small().label(tr(cx, "settings.strataChooseGguf")).disabled(locked).on_click(cx.listener(|this, _, window, cx| this.choose(Field::Model, true, window, cx))))
                .child(Button::new("strata-folder").outline().small().label(tr(cx, "settings.strataChooseDirectory")).disabled(locked).on_click(cx.listener(|this, _, window, cx| this.choose(Field::Model, false, window, cx))))
                .child(Button::new("strata-profile").outline().small().label(tr(cx, "settings.strataChooseProfile")).disabled(locked).on_click(cx.listener(|this, _, window, cx| this.choose(Field::Model, true, window, cx))))
                .when(!status.profiles.is_empty(), |this| {
                    let profiles = status.profiles.clone();
                    let current = form.model_path.clone();
                    let entity = cx.entity().downgrade();
                    let label = profiles.iter().find(|profile| profile.path == current).map(|profile| SharedString::from(profile.label.clone())).unwrap_or_else(|| tr(cx, "settings.strataProfileNone"));
                    this.child(Button::new("strata-profiles").outline().small().label(label).disabled(locked).dropdown_menu(move |mut menu, _, _| {
                        for profile in &profiles {
                            let (path, entity) = (profile.path.clone(), entity.clone());
                            menu = menu.item(PopupMenuItem::new(profile.label.clone()).checked(current == profile.path).on_click(move |_, window, cx| {
                                let path = path.clone();
                                let _ = entity.update(cx, |this, cx| this.model_path.update(cx, |state, cx| state.set_value(path, window, cx)));
                            }));
                        }
                        menu
                    }))
                })
                .into_any_element(),
        );
        body.push(self.field("settings.strataMtpPath", &self.mtp_path, locked, cx));
        body.push(
            h_flex()
                .gap_2()
                .child(Button::new("strata-mtp-file").outline().small().label(tr(cx, "settings.strataChooseMtpFile")).disabled(locked).on_click(cx.listener(|this, _, window, cx| this.choose(Field::Mtp, true, window, cx))))
                .child(Button::new("strata-mtp-folder").outline().small().label(tr(cx, "settings.strataChooseMtp")).disabled(locked).on_click(cx.listener(|this, _, window, cx| this.choose(Field::Mtp, false, window, cx))))
                .into_any_element(),
        );
        let context_label: SharedString = form.context.parse::<u64>().map(|value| group(value)).unwrap_or_else(|_| form.context.clone()).into();
        let mut contexts: Vec<String> = CONTEXTS.iter().map(u32::to_string).collect();
        if !contexts.contains(&form.context) {
            contexts.push(form.context.clone());
        }
        let current_context = form.context.clone();
        let context_picker = Button::new("strata-context").outline().small().label(context_label).disabled(locked).dropdown_menu({
            let entity = cx.entity().downgrade();
            move |mut menu, _, _| {
                for value in &contexts {
                    let (value, entity) = (value.clone(), entity.clone());
                    let label = value.parse::<u64>().map(group).unwrap_or_else(|_| value.clone());
                    menu = menu.item(PopupMenuItem::new(label).checked(current_context == value).on_click(move |_, _, cx| {
                        let value = value.clone();
                        let _ = entity.update(cx, |this, cx| {
                            this.context = value;
                            cx.notify();
                        });
                    }));
                }
                menu
            }
        });
        body.push(
            h_flex()
                .gap_4()
                .items_end()
                .child(v_flex().gap_1().child(div().text_xs().text_color(theme.muted_foreground).child(tr(cx, "settings.strataContext"))).child(context_picker))
                .child(div().w(px(160.)).child(self.field("settings.strataPort", &self.port, locked, cx)))
                .into_any_element(),
        );
        body.push(
            h_flex()
                .id("strata-advanced")
                .gap_1()
                .items_center()
                .cursor_pointer()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(Icon::new(if self.advanced { IconName::ChevronDown } else { IconName::ChevronRight }).xsmall())
                .child(tr(cx, "settings.strataAdvanced"))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.advanced = !this.advanced;
                    cx.notify();
                }))
                .into_any_element(),
        );
        if self.advanced {
            let current_kv = form.kv_cache.clone();
            let kv_picker = Button::new("strata-kv").outline().small().label(SharedString::from(current_kv.clone())).disabled(locked).dropdown_menu({
                let entity = cx.entity().downgrade();
                move |mut menu, _, _| {
                    for value in KV_CACHES {
                        let entity = entity.clone();
                        menu = menu.item(PopupMenuItem::new(value).checked(current_kv == value).on_click(move |_, _, cx| {
                            let _ = entity.update(cx, |this, cx| {
                                this.kv_cache = value.to_owned();
                                cx.notify();
                            });
                        }));
                    }
                    menu
                }
            });
            body.push(
                h_flex()
                    .gap_4()
                    .items_end()
                    .child(v_flex().gap_1().child(div().text_xs().text_color(theme.muted_foreground).child(tr(cx, "settings.strataKvCache"))).child(kv_picker))
                    .child(div().w(px(160.)).child(self.field("settings.strataVramReserve", &self.vram, locked, cx)))
                    .child(div().w(px(240.)).child(self.field("settings.strataResidentBudget", &self.budget, locked, cx)))
                    .into_any_element(),
            );
        }
        if dirty {
            body.push(muted(tr(cx, "settings.strataUnsaved")));
        }
        if !valid {
            body.push(notice(cx, tr(cx, "settings.strataInvalid"), true));
        }
        if status.model.as_ref().is_some_and(|model| model.needs_preparation) {
            body.push(muted(tr(cx, "settings.strataPreparingNote")));
        }
        if let Some(progress) = &status.progress {
            body.push(notice(cx, progress.clone().into(), false));
        }
        if let Some(error) = &status.error {
            body.push(notice(cx, error.clone().into(), true));
        }
        if let Some(server) = status.server.as_ref().filter(|server| server.loaded) {
            body.push(muted(trf(cx, "settings.strataHealth", &[("model", &server.model), ("context", &group(server.context_window))])));
            if let Some(saved) = saved_context.filter(|saved| *saved != server.context_window) {
                body.push(notice(cx, trf(cx, "settings.strataContextStale", &[("saved", &group(saved)), ("actual", &group(server.context_window))]), false));
            }
        }
        if status.server.as_ref().is_some_and(|server| !server.owned) {
            body.push(muted(tr(cx, "settings.strataExternalNote")));
        }
        if status.server.as_ref().is_some_and(|server| server.authentication_required) {
            body.push(notice(cx, tr(cx, "settings.strataAuthentication"), false));
        }
        let connectable = status.server.as_ref().is_some_and(|server| server.loaded && !server.authentication_required) && matches!(status.phase, Phase::Running | Phase::External);
        body.push(
            h_flex()
                .gap_2()
                .flex_wrap()
                .child(Button::new("strata-save").outline().small().label(tr(cx, "settings.strataSave")).disabled(locked || !dirty || !valid).on_click(cx.listener(|this, _, window, cx| this.save(window, cx))))
                .child(
                    Button::new("strata-start")
                        .outline()
                        .small()
                        .label(tr(cx, "settings.strataStart"))
                        .disabled(locked || dirty || form.model_path.trim().is_empty() || !status.runtime.available || status.phase == Phase::External)
                        .on_click(cx.listener(|this, _, window, cx| this.start(window, cx))),
                )
                .child(
                    Button::new("strata-stop")
                        .outline()
                        .small()
                        .label(tr(cx, if preparing { "settings.strataCancelStart" } else { "settings.strataStop" }))
                        .disabled(!can_stop || (pending.is_some() && pending != Some("start")))
                        .on_click(cx.listener(|this, _, window, cx| this.stop(window, cx))),
                )
                .child(
                    Button::new("strata-connect")
                        .primary()
                        .small()
                        .label(tr(cx, "settings.strataConnect"))
                        .disabled(pending.is_some() || dirty || !connectable)
                        .on_click(cx.listener(|this, _, window, cx| this.connect(window, cx))),
                )
                .into_any_element(),
        );
        body
    }
}

/// A count with thousands separators, as 3.x showed it (`toLocaleString('en-US')`).
fn group(value: u64) -> String {
    let digits = value.to_string();
    let mut grouped = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
}

impl Render for StrataCard {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (status, loading, error, available) = {
            let store = self.store.read(cx);
            (store.strata.status.clone(), store.strata.loading, store.strata.error.clone(), store.strata.manager.is_some())
        };
        if let Some(status) = &status {
            self.sync(status, window, cx);
        }
        let theme = cx.theme().clone();
        let mut body = vec![self.header(status.as_ref().map(|status| status.phase), cx)];
        if self.open {
            if !available {
                body.push(match error {
                    Some(error) => notice(cx, error.into(), true),
                    None => notice(cx, tr(cx, "settings.strataLoading"), false),
                });
            } else {
                if let Some(error) = error {
                    body.push(notice(cx, error.into(), true));
                }
                match &status {
                    Some(status) => body.extend(self.render_form(status, cx)),
                    None if loading => body.push(notice(cx, tr(cx, "settings.strataLoading"), false)),
                    None => body.push(
                        Button::new("strata-retry")
                            .outline()
                            .small()
                            .label(tr(cx, "settings.settingsRetry"))
                            .on_click(cx.listener(|this, _, _, cx| this.store.update(cx, |store, cx| store.refresh_strata(true, cx))))
                            .into_any_element(),
                    ),
                }
            }
        }
        v_flex().gap_3().p_4().rounded(theme.radius_lg).border_1().border_color(theme.border).bg(theme.group_box).children(body)
    }
}

#[cfg(test)]
mod tests {
    use ::core::prelude::v1::test;

    use super::*;

    #[test]
    fn forms_and_counts() {
        let form = Form::of(&Settings::default());
        assert_eq!(form.settings(), Some(Settings::default()));
        assert_eq!(Form { port: "80".into(), ..form.clone() }.settings(), None);
        assert_eq!(Form { budget: "12.5".into(), ..form }.settings().and_then(|settings| settings.resident_budget_gib), Some(12.5));
        assert_eq!((group(32768), group(262144), group(999)), ("32,768".to_owned(), "262,144".to_owned(), "999".to_owned()));
    }
}
