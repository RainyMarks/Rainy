//! The settings window: a section list on the left and the chosen section on the right (`SettingsDialog.tsx`).

mod appearance;
mod extensions;
mod external_agents;
mod general;
mod memory;
mod models;
mod models_extra;
mod prompts;
mod runtime;
mod security;
mod shortcuts;
mod strata;
mod tools;
mod widgets;

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use gpui_kit::base::{GlobalState, TextSelection, box_shadow};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, IconName, Sizable as _, StyledExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use rainy_carrier::shell::ShellIntegration;
use serde_json::Value;

use crate::store::update::UpdatePhase;
use crate::store::{Store, StoreEvent};
use crate::ui::actions::run;
use crate::ui::motion::OnBounds as _;
use crate::ui::{tr, trf};
use widgets::{SlidingBounds, enter_progress, spring_slide};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    General,
    Appearance,
    Models,
    Prompts,
    Tools,
    Extensions,
    Runtime,
    Memory,
    Security,
    Shortcuts,
}

const SECTIONS: [(Section, &str); 9] = [
    (Section::General, "settings.navGeneral"),
    (Section::Appearance, "settings.navAppearance"),
    (Section::Models, "settings.navModels"),
    (Section::Prompts, "settings.navPrompts"),
    (Section::Tools, "settings.navTools"),
    (Section::Extensions, "settings.navExtensions"),
    (Section::Runtime, "settings.navRuntime"),
    (Section::Memory, "settings.navMemory"),
    (Section::Security, "settings.navSecurity"),
];

static NEXT_OPEN_GENERATION: AtomicU64 = AtomicU64::new(1);

/// Asks the workbench to close the settings window.
pub struct CloseSettings;

/// Asks the settings window to show another section.
pub struct ShowSection(pub Section);

/// Only the store fields that affect the stateless General/Appearance pages, so unrelated chat streaming never
/// triggers a SettingsView re-render.
#[derive(Debug, Clone, PartialEq, Eq)]
struct GeneralFingerprint {
    update_phase: UpdatePhase,
    update_automatic: bool,
    update_busy: bool,
    shell_available: bool,
    shell_busy: bool,
    shell_choice: ShellIntegration,
    diagnostics_exporting: bool,
}

impl GeneralFingerprint {
    fn of(store: &Store) -> Self {
        Self {
            update_phase: store.updates.phase.clone(),
            update_automatic: store.updates.automatic,
            update_busy: store.updates.busy(),
            shell_available: store.shell.available,
            shell_busy: store.shell.busy,
            shell_choice: store.shell.choice,
            diagnostics_exporting: store.diagnostics.exporting,
        }
    }
}

pub struct SettingsView {
    store: Entity<Store>,
    section: Section,
    open_generation: u64,
    section_generation: u64,
    general_fingerprint: GeneralFingerprint,
    models: Entity<models::ModelsPage>,
    prompts: Option<Entity<prompts::PromptsPage>>,
    extensions: Option<Entity<extensions::ExtensionsPage>>,
    tools: Option<Entity<tools::ToolsPage>>,
    runtime: Option<Entity<runtime::RuntimePage>>,
    memory: Option<Entity<memory::MemoryPage>>,
    security: Option<Entity<security::SecurityPage>>,
    focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<CloseSettings> for SettingsView {}

impl SettingsView {
    pub fn new(store: Entity<Store>, section: Section, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let open_generation = NEXT_OPEN_GENERATION.fetch_add(1, Ordering::Relaxed);
        // Clear any selection in the underlying chat before opening the modal settings layer.
        TextSelection::clear(window, cx);
        TextSelection::end(window, cx);
        let models = cx.new(|cx| models::ModelsPage::new(store.clone(), window, cx));
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        let general_fingerprint = GeneralFingerprint::of(store.read(cx));
        let subscriptions = vec![
            cx.subscribe(&store, |_, _, event: &StoreEvent, cx| {
                if matches!(event, StoreEvent::PrefsChanged | StoreEvent::HostReconnected(_) | StoreEvent::Update(_)) {
                    cx.notify();
                }
            }),
            cx.observe(&store, |this, store, cx| {
                let next = GeneralFingerprint::of(store.read(cx));
                if next != this.general_fingerprint {
                    this.general_fingerprint = next;
                    if matches!(this.section, Section::General | Section::Appearance) {
                        cx.notify();
                    }
                }
            }),
            cx.subscribe(&models, |this, _, ShowSection(section), cx| {
                this.switch_section(*section, cx);
            }),
            cx.on_release_in(window, |_, window, cx| {
                TextSelection::clear(window, cx);
                TextSelection::end(window, cx);
            }),
        ];
        Self {
            store,
            section,
            open_generation,
            section_generation: 0,
            general_fingerprint,
            models,
            prompts: None,
            tools: None,
            extensions: None,
            runtime: None,
            memory: None,
            security: None,
            focus,
            _subscriptions: subscriptions,
        }
    }

    fn switch_section(&mut self, next: Section, cx: &mut Context<Self>) {
        if self.section != next {
            self.section = next;
            self.section_generation = self.section_generation.wrapping_add(1);
            cx.notify();
        }
    }

    /// Switch the active settings section while preserving already-open page state and drafts.
    #[allow(dead_code)]
    pub fn set_section(&mut self, section: Section, window: &mut Window, cx: &mut Context<Self>) {
        self.switch_section(section, cx);
        self.focus.focus(window, cx);
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        TextSelection::clear(window, cx);
        TextSelection::end(window, cx);
        cx.emit(CloseSettings);
    }

    fn render_nav(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let version = env!("CARGO_PKG_VERSION");
        let shortcuts_active = self.section == Section::Shortcuts;
        let active_index = SECTIONS.iter().position(|(section, _)| *section == self.section);
        let theme = cx.theme().clone();
        let bounds = SlidingBounds::shared(("settings-nav-bounds", self.open_generation), window, cx);
        let target = active_index.and_then(|index| bounds.borrow().vertical_target(index));
        let slide = spring_slide(SharedString::from(format!("settings-nav-bar-{}", self.open_generation)), target, window, cx);
        let has_slide = slide.is_some();
        let container_bounds = bounds.clone();
        let list = v_flex()
            .relative()
            .gap_1()
            .on_bounds(move |rect, window, _| {
                if container_bounds.borrow_mut().record_container(rect) {
                    window.request_animation_frame();
                }
            })
            .when_some(slide, |this, (top, height)| {
                let inset = px(6.);
                let bar_h = (height - inset * 2.).max(px(12.));
                this.child(
                    div()
                        .absolute()
                        .left(px(0.))
                        .top(top + inset)
                        .w(px(2.5))
                        .h(bar_h)
                        .rounded_full()
                        .bg(theme.primary),
                )
            })
            .children(SECTIONS.iter().enumerate().map(|(index, (section, key))| {
                let section = *section;
                let active = self.section == section;
                let item_bounds = bounds.clone();
                div()
                    .id(ElementId::Name(format!("settings-nav-{key}").into()))
                    .relative()
                    .pl_3()
                    .pr_2()
                    .py_1p5()
                    .rounded(theme.radius)
                    .text_sm()
                    .cursor_pointer()
                    .on_bounds(move |rect, window, _| {
                        if item_bounds.borrow_mut().record_item(index, rect) {
                            window.request_animation_frame();
                        }
                    })
                    .when(active, |this| {
                        this.bg(theme.list_active)
                            .font_semibold()
                            .when(!has_slide, |this| {
                                this.child(
                                    div()
                                        .absolute()
                                        .left(px(0.))
                                        .top(px(6.))
                                        .bottom(px(6.))
                                        .w(px(2.5))
                                        .rounded_full()
                                        .bg(theme.primary),
                                )
                            })
                    })
                    .when(!active, |this| this.hover(|style| style.bg(theme.list_hover)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.switch_section(section, cx);
                    }))
                    .child(tr(cx, key))
            }));
        v_flex()
            .w(px(220.))
            .h_full()
            .flex_shrink_0()
            .p_3()
            .gap_2()
            .bg(theme.sidebar)
            .border_r_1()
            .border_color(theme.border)
            .child(div().flex_none().px_2().py_1p5().text_lg().font_semibold().child(tr(cx, "settings.title")))
            .child(
                div()
                    .id("settings-nav-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(list),
            )
            .child(
                v_flex()
                    .flex_none()
                    .gap_2()
                    .pt_2()
                    .border_t_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .id("settings-nav-shortcuts")
                            .relative()
                            .pl_3()
                            .pr_2()
                            .py_1p5()
                            .rounded(theme.radius)
                            .text_sm()
                            .cursor_pointer()
                            .when(shortcuts_active, |this| {
                                this.bg(theme.list_active).font_semibold().child(
                                    div()
                                        .absolute()
                                        .left(px(0.))
                                        .top(px(6.))
                                        .bottom(px(6.))
                                        .w(px(2.5))
                                        .rounded_full()
                                        .bg(theme.primary),
                                )
                            })
                            .when(!shortcuts_active, |this| this.hover(|style| style.bg(theme.list_hover)))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.switch_section(Section::Shortcuts, cx);
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
                    ),
            )
    }
}

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let open_delta = enter_progress(format!("settings-open-{}", self.open_generation), Duration::from_millis(150), window, cx);
        let page_delta = enter_progress(
            format!("settings-section-{}-{}", self.open_generation, self.section_generation),
            Duration::from_millis(120),
            window,
            cx,
        );
        let nav = self.render_nav(window, cx);
        let page = match self.section {
            Section::General => general::render(&self.store, window, cx).into_any_element(),
            Section::Appearance => appearance::render(&self.store, window, cx).into_any_element(),
            Section::Models => self.models.clone().into_any_element(),
            Section::Prompts => {
                let page = match &self.prompts {
                    Some(page) => page.clone(),
                    None => {
                        let store = self.store.clone();
                        let page = cx.new(|cx| prompts::PromptsPage::new(store, window, cx));
                        // Opening a file in the editor closes the settings.
                        self._subscriptions.push(cx.subscribe_in(&page, window, |this, _, _: &CloseSettings, window, cx| this.close(window, cx)));
                        self.prompts = Some(page.clone());
                        page
                    }
                };
                page.into_any_element()
            }
            // Pages that read the Host are made when first shown and kept while the window is open.
            Section::Tools => {
                let store = self.store.clone();
                self.tools.get_or_insert_with(|| cx.new(|cx| tools::ToolsPage::new(store, window, cx))).clone().into_any_element()
            }
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
        let overlay = Hsla { a: theme.overlay.a * open_delta, ..theme.overlay };
        let shadow_alpha = open_delta * open_delta * open_delta;
        let shadows = vec![
            box_shadow(px(0.), px(10.), px(24.), px(-4.), hsla(0., 0., 0., 0.16 * shadow_alpha)),
            box_shadow(px(0.), px(4.), px(8.), px(-2.), hsla(0., 0., 0., 0.08 * shadow_alpha)),
        ];
        div()
            .id("settings-root")
            .occlude()
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    this.close(window, cx);
                }
            }))
            .child(
                div()
                    .id("settings-mask")
                    .occlude()
                    .absolute()
                    .inset_0()
                    .bg(overlay)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, window, cx| {
                            GlobalState::suppress_text_selection(cx);
                            cx.stop_propagation();
                            this.close(window, cx);
                        }),
                    ),
            )
            .child(
                h_flex()
                    .id("settings-panel")
                    .occlude()
                    .relative()
                    .mt(px(8.0 * (1.0 - open_delta)))
                    .w(px(1040.))
                    .max_w(relative(0.94))
                    .h(relative(0.88))
                    .max_h(px(820.))
                    .rounded(theme.radius_lg)
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.background)
                    .shadow(shadows)
                    .overflow_hidden()
                    // Bubble phase: let inputs/textareas inside focus and select text first, then stop the press from
                    // reaching the window-level TextSelectionLayer or the backdrop mask.
                    .on_mouse_down(MouseButton::Left, |_, _, cx| {
                        GlobalState::suppress_text_selection(cx);
                        cx.stop_propagation();
                    })
                    .child(nav)
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .child(
                                h_flex()
                                    .flex_none()
                                    .w_full()
                                    .justify_end()
                                    .px_3()
                                    .pt_2p5()
                                    .pb_0()
                                    .child(
                                        div().id("settings-close-wrap").occlude().child(
                                            Button::new("settings-close")
                                                .ghost()
                                                .small()
                                                .icon(IconName::Close)
                                                .tooltip(tr(cx, "settings.close"))
                                                .on_click(cx.listener(|this, _, window, cx| this.close(window, cx))),
                                        ),
                                    ),
                            )
                            .child(
                                div()
                                    // Each section keeps its own scroll position; another starts at its top.
                                    .id(ElementId::Name(format!("settings-content-{:?}", self.section).into()))
                                    .flex_1()
                                    .min_w_0()
                                    .min_h_0()
                                    .overflow_y_scroll()
                                    .child(
                                        v_flex()
                                            .w_full()
                                            .px_8()
                                            .pt_1()
                                            .pb_6()
                                            .gap_4()
                                            .max_w(px(760.))
                                            .opacity(0.65 + 0.35 * page_delta)
                                            .mt(px(4.0 * (1.0 - page_delta)))
                                            .child(page),
                                    ),
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

/// Mutually exclusive choices as a segmented pill bar with a spring-sliding active capsule; `change` gets the chosen id.
pub(super) fn segmented(
    id: &str,
    items: Vec<(&'static str, SharedString)>,
    current: &str,
    window: &mut Window,
    cx: &mut App,
    change: impl Fn(&'static str, &mut Window, &mut App) + 'static,
) -> AnyElement {
    let theme = cx.theme().clone();
    let change = std::rc::Rc::new(change);
    let active_index = items.iter().position(|(value, _)| *value == current);
    let bounds = SlidingBounds::shared(ElementId::Name(format!("seg-bounds-{id}").into()), window, cx);
    let target = active_index.and_then(|index| bounds.borrow().horizontal_target(index));
    let slide = spring_slide(SharedString::from(format!("seg-pill-{id}")), target, window, cx);
    let has_slide = slide.is_some();
    let container_bounds = bounds.clone();
    h_flex()
        .relative()
        .flex_none()
        .p_0p5()
        .gap_0p5()
        .rounded(theme.radius)
        .border_1()
        .border_color(theme.border)
        .bg(theme.muted)
        .on_bounds(move |rect, window, _| {
            if container_bounds.borrow_mut().record_container(rect) {
                window.request_animation_frame();
            }
        })
        .when_some(slide, |this, (left, width)| {
            this.child(
                div()
                    .absolute()
                    .top(px(2.))
                    .bottom(px(2.))
                    .left(left)
                    .w(width)
                    .rounded(theme.radius - px(2.))
                    .bg(theme.primary),
            )
        })
        .children(items.into_iter().enumerate().map(|(index, (value, label))| {
            let change = change.clone();
            let selected = current == value;
            let item_bounds = bounds.clone();
            div()
                .id(ElementId::Name(format!("{id}-{value}").into()))
                .relative()
                .px_2p5()
                .py_1()
                .rounded(theme.radius - px(2.))
                .text_xs()
                .font_medium()
                .whitespace_nowrap()
                .cursor_pointer()
                .on_bounds(move |rect, window, _| {
                    if item_bounds.borrow_mut().record_item(index, rect) {
                        window.request_animation_frame();
                    }
                })
                .when(selected, |this| {
                    this.text_color(theme.primary_foreground)
                        .when(!has_slide, |this| this.bg(theme.primary))
                })
                .when(!selected, |this| {
                    this.text_color(theme.muted_foreground)
                        .hover(|style| style.text_color(theme.foreground))
                })
                .on_mouse_down(MouseButton::Left, |_, _, cx| {
                    GlobalState::suppress_text_selection(cx);
                })
                .on_click(move |_, window, cx| {
                    if !selected {
                        change(value, window, cx);
                    }
                })
                .child(label)
        }))
        .into_any_element()
}

/// Store changed preferences (`prefs.set`); the Host answers every window with `prefs.changed`.
pub(crate) fn set_prefs(patch: Value, window: &mut Window, cx: &mut App) {
    run("prefs.set", patch, window.window_handle(), cx);
}
