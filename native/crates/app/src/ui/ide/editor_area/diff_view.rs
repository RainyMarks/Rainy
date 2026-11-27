//! Diff tab rendering: interactive per-hunk Accept/Revert review and side-by-side Diff view.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::diff::Diff;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Selectable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::store::ide::split_key;
use crate::ui::{tr, trf};

use super::{EditorArea, centered};

impl EditorArea {
    pub(super) fn render_diff(&self, key: String, cx: &mut Context<Self>) -> AnyElement {
        use crate::ui::ide::hunk_diff::HunkLineKind;

        let theme = cx.theme().clone();
        let ide = self.ide.read(cx);
        let tab = ide.diffs.get(&key);
        let state = tab.and_then(|tab| tab.state.clone());
        let error = tab.and_then(|tab| tab.error.clone());
        let hunks = tab.map(|tab| tab.hunks.clone()).unwrap_or_default();
        let hunk_count = hunks.len();
        let loaded = state.is_some() || tab.is_some_and(|tab| !tab.status.is_empty());
        let hunk_mode = self.diff_hunk_mode;
        let (show, reload) = (self.ide.clone(), self.ide.clone());
        let (shown, reloaded) = (key.clone(), key.clone());
        let (_, path) = split_key(&key);
        let body = match (state, error) {
            (_, Some(error)) => centered(div().child(format!("{} {}", tr(cx, "ide.ideDiffUnavailable"), error)), &theme).into_any_element(),
            (Some(_), None) if hunk_mode => {
                if hunks.is_empty() {
                    centered(div().child(tr(cx, "native.diffNoChanges")), &theme).into_any_element()
                } else {
                    let ide_handle = self.ide.clone();
                    let diff_key = key.clone();
                    let added_bg = theme.success.opacity(if theme.is_dark() { 0.14 } else { 0.12 });
                    let removed_bg = theme.danger.opacity(if theme.is_dark() { 0.14 } else { 0.12 });
                    let added_fg = if theme.is_dark() { theme.success } else { Hsla { l: theme.success.l.min(0.32), ..theme.success } };
                    let removed_fg = if theme.is_dark() { theme.danger } else { Hsla { l: theme.danger.l.min(0.35), ..theme.danger } };
                    let accept_label = tr(cx, "native.diffAcceptHunk");
                    let revert_label = tr(cx, "native.diffRevertHunk");
                    let card_radius = theme.radius;
                    let card_border = theme.border;
                    let card_bg = theme.background;
                    let header_bg = theme.muted.opacity(0.45);
                    let muted_fg = theme.muted_foreground;
                    let text_fg = theme.foreground;
                    let mono_font = theme.mono_font_family.clone();
                    v_flex()
                        .id("diff-hunks")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scrollbar()
                        .p_3()
                        .gap_3()
                        .children(hunks.into_iter().map(move |hunk| {
                            let idx = hunk.index;
                            let accept_ide = ide_handle.clone();
                            let revert_ide = ide_handle.clone();
                            let accept_key = diff_key.clone();
                            let revert_key = diff_key.clone();
                            let accept_label = accept_label.clone();
                            let revert_label = revert_label.clone();
                            let mono = mono_font.clone();
                            v_flex()
                                .rounded(card_radius)
                                .border_1()
                                .border_color(card_border)
                                .bg(card_bg)
                                .overflow_hidden()
                                .child(
                                    h_flex()
                                        .px_2p5()
                                        .py_1()
                                        .gap_2()
                                        .items_center()
                                        .bg(header_bg)
                                        .border_b_1()
                                        .border_color(card_border)
                                        .child(
                                            div()
                                                .flex_1()
                                                .min_w_0()
                                                .overflow_hidden()
                                                .text_ellipsis()
                                                .whitespace_nowrap()
                                                .font_family(mono.clone())
                                                .text_xs()
                                                .text_color(muted_fg)
                                                .child(hunk.header),
                                        )
                                        .child(
                                            h_flex()
                                                .flex_shrink_0()
                                                .gap_1p5()
                                                .text_xs()
                                                .font_family(mono.clone())
                                                .when(hunk.added > 0, |this| this.child(div().text_color(added_fg).child(format!("+{}", hunk.added))))
                                                .when(hunk.removed > 0, |this| this.child(div().text_color(removed_fg).child(format!("-{}", hunk.removed)))),
                                        )
                                        .child(
                                            Button::new(("hunk-accept", idx))
                                                .outline()
                                                .xsmall()
                                                .icon(IconName::Check)
                                                .label(accept_label)
                                                .on_click(move |_, window, cx| {
                                                    let key = accept_key.clone();
                                                    accept_ide.update(cx, |ide, cx| ide.apply_diff_hunk(key, idx, true, window, cx));
                                                }),
                                        )
                                        .child(
                                            Button::new(("hunk-revert", idx))
                                                .ghost()
                                                .xsmall()
                                                .icon(IconName::Undo)
                                                .label(revert_label)
                                                .on_click(move |_, window, cx| {
                                                    let key = revert_key.clone();
                                                    revert_ide.update(cx, |ide, cx| ide.apply_diff_hunk(key, idx, false, window, cx));
                                                }),
                                        ),
                                )
                                .child(
                                    v_flex()
                                        .py_1()
                                        .font_family(mono)
                                        .text_xs()
                                        .children(hunk.lines.into_iter().map(move |line| {
                                            let (bg, mark, fg) = match line.kind {
                                                HunkLineKind::Added => (Some(added_bg), "+", added_fg),
                                                HunkLineKind::Removed => (Some(removed_bg), "-", removed_fg),
                                                HunkLineKind::Context => (None, " ", text_fg),
                                            };
                                            let old_no = line.old_line.map(|n| n.to_string()).unwrap_or_default();
                                            let new_no = line.new_line.map(|n| n.to_string()).unwrap_or_default();
                                            h_flex()
                                                .w_full()
                                                .px_2()
                                                .gap_2()
                                                .items_start()
                                                .when_some(bg, |this, bg| this.bg(bg))
                                                .child(div().w(px(34.)).flex_shrink_0().text_right().text_color(muted_fg.opacity(0.7)).child(old_no))
                                                .child(div().w(px(34.)).flex_shrink_0().text_right().text_color(muted_fg.opacity(0.7)).child(new_no))
                                                .child(div().w(px(12.)).flex_shrink_0().text_color(fg).child(mark))
                                                .child(div().flex_1().min_w_0().whitespace_nowrap().overflow_hidden().text_color(text_fg).child(line.text))
                                        })),
                                )
                        }))
                        .into_any_element()
                }
            }
            (Some(state), None) => div().flex_1().min_h_0().child(Diff::new(&state)).into_any_element(),
            (None, None) if loaded => centered(div().child(tr(cx, "native.diffNoChanges")), &theme).into_any_element(),
            (None, None) => centered(Spinner::new().small(), &theme).into_any_element(),
        };
        v_flex()
            .size_full()
            .child(
                h_flex()
                    .flex_shrink_0()
                    .h(px(34.))
                    .px_3()
                    .gap_2()
                    .items_center()
                    .text_sm()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(Icon::new(gpui_kit::assets::IconName::GitCompare).small().text_color(theme.muted_foreground))
                    .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().child(path.to_owned()))
                    .when(hunk_count > 0, |this| {
                        this.child(
                            div()
                                .flex_shrink_0()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(trf(cx, "native.diffHunkCount", &[("count", &hunk_count)])),
                        )
                    })
                    .child(
                        Button::new("diff-mode-hunks")
                            .ghost()
                            .xsmall()
                            .selected(hunk_mode)
                            .label(tr(cx, "native.diffHunks"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.diff_hunk_mode = true;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("diff-mode-side")
                            .ghost()
                            .xsmall()
                            .selected(!hunk_mode)
                            .label(tr(cx, "native.diffSideBySide"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.diff_hunk_mode = false;
                                cx.notify();
                            })),
                    )
                    .child(Button::new("diff-refresh").ghost().xsmall().icon(IconName::RefreshCw).tooltip(tr(cx, "ide.ideRefresh")).on_click(move |_, window, cx| {
                        let key = reloaded.clone();
                        reload.update(cx, |ide, cx| ide.load_diff(key, window, cx));
                    }))
                    .child(Button::new("diff-open").outline().xsmall().label(tr(cx, "git.gitOpenFile")).on_click(move |_, window, cx| {
                        let key = shown.clone();
                        show.update(cx, |ide, cx| ide.show_file(key, window, cx));
                    })),
            )
            .child(body)
            .into_any_element()
    }
}
