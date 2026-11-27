//! Bottom status bar (target, tunnels, git branch, remote metrics, LSP status, cursor, version) and startup phase screen.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::progress::Progress;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{ActiveTheme as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use rainy_carrier::ExecutionTarget;

use crate::store::Phase;
use crate::ui::settings::Section;
use crate::ui::{tr, trf};

use super::Workbench;
use super::layout::Side;

impl Workbench {
    pub(super) fn render_status_bar(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let version = env!("CARGO_PKG_VERSION");
        // The language service of the file shown, while it starts or after it failed; a working one says nothing.
        let active = self.ide.read(cx).active.clone();
        let language = active.and_then(|key| {
            let servers = crate::store::language::LanguageServers::global(cx);
            let servers = servers.read(cx);
            let (name, phase) = servers.phase_of(&key)?;
            let (label, reason, opens) = match phase {
                crate::store::language::ServerPhase::Starting => (trf(cx, "native.languageStarting", &[("language", &name)]), None, None),
                crate::store::language::ServerPhase::Failed(reason) if reason.as_ref() == "tools-missing" => (trf(cx, "native.languageToolsMissing", &[("language", &name)]), None, Some(Section::Tools)),
                crate::store::language::ServerPhase::Failed(reason) if reason.starts_with("program-missing:") => {
                    let program = &reason["program-missing:".len()..];
                    let label = trf(cx, "native.languageProgramMissing", &[("language", &name), ("program", &program)]);
                    (label, Some(trf(cx, "native.languageProgramMissingHint", &[("program", &program)])), Some(Section::Runtime))
                }
                crate::store::language::ServerPhase::Failed(reason) => (trf(cx, "native.languageFailed", &[("language", &name)]), Some(reason.clone()), None),
                crate::store::language::ServerPhase::Ready => return None,
            };
            Some(
                div()
                    .id("language-state")
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .child(label)
                    .when_some(opens, |this, section| this.cursor_pointer().on_click(cx.listener(move |this, _, window, cx| this.open_settings(section, window, cx))))
                    .when_some(reason, |this, reason| this.tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(reason.clone()).build(window, cx))),
            )
        });
        // The branch of the project's repository and its changed files; a click shows them.
        let changes = self.git.read(cx).change_count();
        let branch = self.ide.read(cx).workspace.as_ref().and(self.git.read(cx).branch()).map(|branch| {
            let label: SharedString = branch.map(SharedString::from).unwrap_or_else(|| tr(cx, "git.gitNoCommits"));
            let theme = cx.theme();
            Button::new("status-branch")
                .ghost()
                .xsmall()
                .min_w_0()
                .flex_shrink_1()
                .max_w(px(220.))
                .icon(gpui_kit::component::Icon::new(gpui_kit::assets::IconName::GitBranch))
                .label(label)
                .when(changes > 0, |this| {
                    this.child(div().flex_shrink_0().px_1().rounded(theme.radius).bg(theme.muted).text_color(theme.foreground).child(changes.min(999).to_string()))
                })
                .tooltip(tr(cx, "git.gitPanel"))
                .on_click(cx.listener(|this, _, window, cx| this.show_side(Side::Git, window, cx)))
        });
        let (dot, target) = self.render_target("target", cx);
        let is_ssh = self.store.read(cx).is_ssh();
        let is_remote = self.store.read(cx).is_remote();
        let theme = cx.theme();
        // What does not fit in a narrow window is cut: the branch, metrics and language state give way first.
        h_flex()
            .h(px(24.))
            .flex_shrink_0()
            .overflow_hidden()
            .px_3()
            .gap_2()
            .items_center()
            .text_xs()
            .text_color(theme.muted_foreground)
            .bg(theme.status_bar)
            .border_t_1()
            .border_color(theme.status_bar_border)
            .child(div().size(px(7.)).flex_shrink_0().rounded_full().bg(dot))
            .child(target)
            .when(is_ssh, |this| this.child(self.tunnels.clone()))
            .children(branch)
            .child(div().flex_1())
            .when(is_remote, |this| this.child(div().min_w_0().flex_shrink_1().overflow_hidden().child(self.metrics.clone())))
            .children(language)
            .child(div().flex_shrink_0().child(self.cursor.clone()))
            .child(div().flex_shrink_0().whitespace_nowrap().child(format!("Rainy {version}")))
    }

    pub(super) fn render_phase(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let phase = self.store.read(cx).phase.clone();
        if matches!(phase, Phase::Ready) {
            return None;
        }
        let on_wsl = matches!(self.store.read(cx).target, Some(ExecutionTarget::Wsl { .. } | ExecutionTarget::Ssh { .. }));
        let theme = cx.theme();
        match phase {
            Phase::Ready => None,
            Phase::Starting(step, fraction) => Some(
                v_flex()
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .gap_3()
                    .child(img("brand/rainy-mark.svg").size(px(56.)))
                    .child(h_flex().gap_2().items_center().text_sm().text_color(theme.muted_foreground).child(Spinner::new().small()).child(tr(cx, "native.starting")))
                    .when(step != tr(cx, "native.starting"), |this| this.child(div().text_xs().text_color(theme.muted_foreground).child(step)))
                    .when_some(fraction, |this, fraction| this.child(Progress::new("startup-progress").xsmall().w(px(240.)).value(fraction * 100.)))
                    .into_any_element(),
            ),
            Phase::Failed(error) => Some(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .items_center()
                    .justify_center()
                    .gap_3()
                    .p_8()
                    .child(img("brand/rainy-mark.svg").size(px(56.)).flex_shrink_0())
                    .child(
                        div()
                            .id("startup-error")
                            .w_full()
                            .max_w(px(640.))
                            .max_h(px(360.))
                            .min_h_0()
                            .overflow_y_scroll()
                            .text_sm()
                            .text_color(theme.danger)
                            .child(error),
                    )
                    .child(
                        h_flex()
                            .flex_shrink_0()
                            .gap_2()
                            .child(Button::new("retry").primary().label(tr(cx, "native.retry")).on_click(cx.listener(|this, _, _, cx| {
                                this.store.update(cx, |store, cx| store.restart(cx));
                            })))
                            .when(on_wsl, |this| {
                                this.child(Button::new("use-windows").outline().label(tr(cx, "native.useWindows")).on_click(cx.listener(
                                    |this, _, window, cx| {
                                        let handle = window.window_handle();
                                        this.store.update(cx, |store, cx| store.switch_target(ExecutionTarget::windows(), handle, cx));
                                    },
                                )))
                            })
                            .child(Button::new("quit").outline().label(tr(cx, "native.quit")).on_click(|_, _, cx| cx.quit())),
                    )
                    .into_any_element(),
            ),
        }
    }
}
