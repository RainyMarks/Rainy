//! Tools: the third-party tools Rainy can add. None comes with Rainy; each is downloaded when the user asks and can be
//! removed again: Strata for local models, and the optional components (PHP).

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::progress::Progress;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, StyledExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use rainy_carrier::modules::{ModuleError, Phase, State, Status};

use super::heading;
use super::models_extra::{card, notice};
use super::runtime::file_size_text;
use super::strata::StrataCard;
use crate::store::Store;
use crate::store::modules::OFFERED;
use crate::ui::{tr, trf};

pub struct ToolsPage {
    store: Entity<Store>,
    strata: Entity<StrataCard>,
    _subscriptions: Vec<Subscription>,
}

impl ToolsPage {
    pub fn new(store: Entity<Store>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let strata = cx.new(|cx| StrataCard::new(store.clone(), window, cx));
        let subscriptions = vec![cx.observe(&store, |_, _, cx| cx.notify())];
        store.update(cx, |store, cx| store.refresh_modules(cx));
        Self { store, strata, _subscriptions: subscriptions }
    }

    /// The optional components: each offered module with its size and state, and download, cancel or remove.
    fn render_modules(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let locale = crate::ui::locale(cx);
        let modules = &self.store.read(cx).modules;
        let (state, running, busy, statuses) = (modules.state.clone(), modules.running(), modules.busy, modules.statuses.clone());
        let mut body: Vec<AnyElement> = Vec::new();
        match statuses {
            None => body.push(h_flex().child(Spinner::new().small()).into_any_element()),
            // Development resources ship no descriptor.
            Some(Err(ModuleError::Unavailable)) => {
                body.push(div().text_xs().text_color(theme.muted_foreground).child(tr(cx, "native.modulesUnavailable")).into_any_element());
            }
            Some(Err(error)) => body.push(notice(cx, crate::boot::module_message(locale, &error).into(), true)),
            Some(Ok(statuses)) => {
                for status in statuses {
                    body.push(self.render_module(&status, &state, running, busy, cx));
                }
            }
        }
        let shown = |module: &Option<String>| module.as_deref().is_none_or(|id| OFFERED.contains(&id));
        if state.phase == Phase::Cancelled && shown(&state.module) {
            body.push(div().text_xs().text_color(theme.muted_foreground).child(tr(cx, "settings.modulesCancelled")).into_any_element());
        }
        if let (Phase::Error, Some(error)) = (state.phase, &state.error)
            && shown(&state.module)
        {
            body.push(notice(cx, crate::boot::module_message(locale, error).into(), true));
        }
        card(cx, "settings.modulesTitle", Some("settings.modulesNote"), body)
    }

    fn render_module(&self, status: &Status, state: &State, running: bool, busy: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let Some(id) = OFFERED.into_iter().find(|offered| *offered == status.id) else { return div().into_any_element() };
        let (name, hint) = ("settings.modulePhp", "settings.modulePhpHint");
        let active = running && state.module.as_deref() == Some(status.id.as_str());
        let percentage = if state.total_bytes > 0 { state.completed_bytes.saturating_mul(100) / state.total_bytes } else { 0 };
        let line = if active && state.phase == Phase::Installing {
            tr(cx, "settings.modulesInstalling")
        } else if active {
            trf(cx, "settings.modulesProgress", &[("progress", &percentage)])
        } else if status.installed {
            trf(cx, "settings.modulesInstalled", &[("size", &file_size_text(status.unpacked_bytes))])
        } else {
            tr(cx, "settings.modulesNotInstalled")
        };
        let store = self.store.clone();
        let action = if active {
            Button::new(SharedString::from(format!("module-cancel-{id}"))).outline().small().label(tr(cx, "settings.modulesCancel")).on_click(move |_, _, cx| {
                store.update(cx, |store, _| store.cancel_module());
            })
        } else if status.installed {
            Button::new(SharedString::from(format!("module-remove-{id}")))
                .outline()
                .small()
                .label(tr(cx, "settings.modulesRemove"))
                .disabled(busy || running)
                .on_click(move |_, _, cx| store.update(cx, |store, cx| store.remove_module(id, cx)))
        } else {
            Button::new(SharedString::from(format!("module-download-{id}")))
                .primary()
                .small()
                .label(trf(cx, "settings.modulesDownload", &[("size", &file_size_text(status.download_bytes))]))
                .disabled(busy || running)
                .on_click(move |_, _, cx| store.update(cx, |store, cx| store.install_module(id, cx)))
        };
        v_flex()
            .gap_2()
            .child(
                h_flex()
                    .gap_3()
                    .items_center()
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(
                                h_flex()
                                    .gap_1()
                                    .text_sm()
                                    .child(div().font_semibold().child(tr(cx, name)))
                                    .child(div().text_color(theme.muted_foreground).child(format!("· {}", tr(cx, hint)))),
                            )
                            .child(div().text_xs().text_color(theme.muted_foreground).child(line)),
                    )
                    .child(action),
            )
            .when(active && state.phase == Phase::Downloading, |this| {
                this.child(Progress::new(SharedString::from(format!("module-progress-{id}"))).xsmall().value(percentage as f32))
            })
            .into_any_element()
    }
}

impl Render for ToolsPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex().gap_4().child(heading(cx, "settings.navTools", "settings.toolsLead")).child(self.strata.clone()).child(self.render_modules(cx))
    }
}
