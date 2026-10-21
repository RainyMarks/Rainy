//! The settings window: a section list on the left and the chosen section on the right (`SettingsDialog.tsx`).

mod appearance;
mod extensions;
mod general;
mod memory;
mod models;
mod models_extra;
mod runtime;
mod security;
mod shortcuts;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, IconName, Sizable as _, StyledExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use serde_json::Value;

use crate::store::Store;
use crate::ui::actions::run;
use crate::ui::{tr, trf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    General,
    Appearance,
    Models,
    Extensions,
    Runtime,
    Memory,
    Security,
    Shortcuts,
}

const SECTIONS: [(Section, &str); 7] = [
    (Section::General, "settings.navGeneral"),
    (Section::Appearance, "settings.navAppearance"),
    (Section::Models, "settings.navModels"),
    (Section::Extensions, "settings.navExtensions"),
    (Section::Runtime, "settings.navRuntime"),
    (Section::Memory, "settings.navMemory"),
    (Section::Security, "settings.navSecurity"),
];

/// Asks the workbench to close the settings window.
pub struct CloseSettings;

pub struct SettingsView {
    store: Entity<Store>,
    section: Section,
    models: Entity<models::ModelsPage>,
    extensions: Option<Entity<extensions::ExtensionsPage>>,
    runtime: Option<Entity<runtime::RuntimePage>>,
    memory: Option<Entity<memory::MemoryPage>>,
    security: Option<Entity<security::SecurityPage>>,
    focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<CloseSettings> for SettingsView {}

impl SettingsView {
    pub fn new(store: Entity<Store>, section: Section, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let models = cx.new(|cx| models::ModelsPage::new(store.clone(), window, cx));
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        let subscriptions = vec![cx.observe(&store, |_, _, cx| cx.notify())];
        Self { store, section, models, extensions: None, runtime: None, memory: None, security: None, focus, _subscriptions: subscriptions }
    }

    fn render_nav(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let version = self.store.read(cx).info.as_ref().map(|info| info.version.clone()).unwrap_or_default();
        let shortcuts_active = self.section == Section::Shortcuts;
        let theme = cx.theme();
        v_flex()
            .w(px(220.))
            .h_full()
            .flex_shrink_0()
            .p_3()
            .gap_1()
            .bg(theme.sidebar)
            .border_r_1()
            .border_color(theme.border)
            .child(div().px_2().py_2().text_lg().font_semibold().child(tr(cx, "settings.title")))
            .children(SECTIONS.iter().map(|(section, key)| {
                let section = *section;
                let active = self.section == section;
                div()
                    .id(ElementId::Name(format!("settings-nav-{key}").into()))
                    .px_2()
                    .py_1p5()
                    .rounded(theme.radius)
                    .text_sm()
                    .cursor_pointer()
                    .when(active, |this| this.bg(theme.list_active).font_semibold())
                    .when(!active, |this| this.hover(|style| style.bg(theme.list_hover)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.section = section;
                        cx.notify();
                    }))
                    .child(tr(cx, key))
            }))
            .child(div().flex_1())
            .child(
                div()
                    .id("settings-nav-shortcuts")
                    .px_2()
                    .py_1p5()
                    .rounded(theme.radius)
                    .text_sm()
                    .cursor_pointer()
                    .when(shortcuts_active, |this| this.bg(theme.list_active).font_semibold())
                    .when(!shortcuts_active, |this| this.hover(|style| style.bg(theme.list_hover)))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.section = Section::Shortcuts;
                        cx.notify();
                    }))
                    .child(tr(cx, "settings.shortcutsTitle")),
            )
            .child(
                h_flex()
                    .gap_2()
                    .px_2()
                    .items_center()
                    .child(img("brand/rainy-mark.svg").size(px(28.)))
                    .child(
                        v_flex()
                            .child(div().text_sm().font_semibold().child("Rainy"))
                            .child(div().text_xs().text_color(theme.muted_foreground).child(trf(cx, "settings.appVersion", &[("version", &version)]))),
                    ),
            )
    }
}

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let nav = self.render_nav(cx);
        let page = match self.section {
            Section::General => general::render(&self.store, window, cx).into_any_element(),
            Section::Appearance => appearance::render(&self.store, window, cx).into_any_element(),
            Section::Models => self.models.clone().into_any_element(),
            // Pages that read the Host are made when first shown and kept while the window is open.
            Section::Extensions => {
                let store = self.store.clone();
                self.extensions.get_or_insert_with(|| cx.new(|cx| extensions::ExtensionsPage::new(store, window, cx))).clone().into_any_element()
            }
            Section::Runtime => {
                let store = self.store.clone();
                self.runtime.get_or_insert_with(|| cx.new(|cx| runtime::RuntimePage::new(store, window, cx))).clone().into_any_element()
            }
            Section::Memory => {
                let store = self.store.clone();
                self.memory.get_or_insert_with(|| cx.new(|cx| memory::MemoryPage::new(store, window, cx))).clone().into_any_element()
            }
            Section::Security => {
                let store = self.store.clone();
                self.security.get_or_insert_with(|| cx.new(|cx| security::SecurityPage::new(store, window, cx))).clone().into_any_element()
            }
            Section::Shortcuts => shortcuts::render(cx).into_any_element(),
        };
        let theme = cx.theme();
        div()
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|_, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    cx.emit(CloseSettings);
                }
            }))
            .child(
                div()
                    .id("settings-mask")
                    .absolute()
                    .inset_0()
                    .bg(theme.overlay)
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(CloseSettings))),
            )
            .child(
                h_flex()
                    .id("settings-panel")
                    .relative()
                    .w(px(1040.))
                    .max_w(relative(0.94))
                    .h(relative(0.88))
                    .max_h(px(820.))
                    .rounded(theme.radius_lg)
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.background)
                    .shadow_lg()
                    .overflow_hidden()
                    // Clicks inside the panel do not reach the mask.
                    .on_click(|_, _, cx| cx.stop_propagation())
                    .child(nav)
                    .child(
                        div()
                            .id("settings-content")
                            .flex_1()
                            .h_full()
                            .overflow_y_scroll()
                            .child(v_flex().px_8().py_6().gap_4().max_w(px(760.)).child(page)),
                    )
                    .child(
                        div().absolute().top_3().right_3().child(
                            Button::new("settings-close")
                                .ghost()
                                .small()
                                .icon(IconName::Close)
                                .tooltip(tr(cx, "settings.close"))
                                .on_click(cx.listener(|_, _, _, cx| cx.emit(CloseSettings))),
                        ),
                    ),
            )
    }
}

/// Section heading and its lead line.
pub(super) fn heading(cx: &App, title: &str, lead: &str) -> impl IntoElement {
    v_flex()
        .gap_1()
        .child(div().text_xl().font_semibold().child(tr(cx, title)))
        .child(div().text_sm().text_color(cx.theme().muted_foreground).child(tr(cx, lead)))
}

/// A titled group of rows.
pub(super) fn group(cx: &App, title: Option<&str>, rows: Vec<AnyElement>) -> impl IntoElement {
    let theme = cx.theme();
    v_flex()
        .gap_2()
        .when_some(title, |this, title| this.child(div().mt_2().text_sm().font_semibold().child(tr(cx, title))))
        .child(
            v_flex()
                .rounded(theme.radius_lg)
                .border_1()
                .border_color(theme.border)
                .bg(theme.group_box)
                .children(rows.into_iter().enumerate().map(|(index, row)| {
                    div().when(index > 0, |this| this.border_t_1().border_color(theme.border)).child(row)
                })),
        )
}

/// One setting: title and description on the left, the control on the right.
pub(super) fn row(cx: &App, title: SharedString, description: Option<SharedString>, control: impl IntoElement) -> AnyElement {
    let theme = cx.theme();
    h_flex()
        .px_4()
        .py_3()
        .gap_4()
        .items_center()
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_0p5()
                .child(div().text_sm().child(title))
                .when_some(description, |this, description| {
                    this.child(div().text_xs().text_color(theme.muted_foreground).child(description))
                }),
        )
        .child(control)
        .into_any_element()
}

/// Mutually exclusive choices as a row of buttons; `change` gets the chosen id.
pub(super) fn segmented(
    id: &str,
    items: Vec<(&'static str, SharedString)>,
    current: &str,
    change: impl Fn(&'static str, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let change = std::rc::Rc::new(change);
    h_flex().gap_1().children(items.into_iter().map(|(value, label)| {
        let change = change.clone();
        let button = Button::new(ElementId::Name(format!("{id}-{value}").into())).small().label(label);
        let button = if current == value { button.primary() } else { button.outline() };
        button.on_click(move |_, window, cx| change(value, window, cx))
    }))
}

/// Store changed preferences (`prefs.set`); the Host answers every window with `prefs.changed`.
pub(super) fn set_prefs(patch: Value, window: &mut Window, cx: &mut App) {
    run("prefs.set", patch, window.window_handle(), cx);
}
