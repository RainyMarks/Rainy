//! Empty-chat views (IDE hero, Agent greeting, Optical look greeting) and unit tests for the chat pane.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Sizable as _, StyledExt as _, h_flex, v_flex};
use gpui_kit::*;

use super::ChatPane;
use crate::i18n::{self, Locale};
use crate::ui::tr;

/// A window shorter than this leaves the optical look's usage overview out, so the composer stays in view (the page
/// without the map is about 420 pixels tall).
const OPTICS_CARD_HEIGHT: f32 = 500.;

impl ChatPane {
    pub(super) fn render_empty(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let muted = cx.theme().muted_foreground;
        v_flex()
            .flex_1()
            .items_center()
            .justify_center()
            .gap_2()
            .child(img("brand/rainy-mark.svg").size(px(48.)))
            .child(div().text_xl().font_semibold().child(tr(cx, "chat.heroTitle")))
            .child(div().text_sm().text_color(muted).child(tr(cx, "chat.heroCredit")))
    }

    /// Agent mode's new chat: a greeting and the project above the composer, in the middle of the window.
    pub(super) fn render_greeting(&mut self, locale: Locale, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if crate::ui::optics::enabled(cx) {
            return self.render_optics_greeting(locale, window, cx);
        }
        // The look was turned off: its page clock and overview go (the clock would still wake with the window).
        (self.optics_shown, self.backdrop, self.usage) = (false, None, None);
        let ide = crate::store::ide::Ide::global(cx).read(cx);
        let (project, path): (SharedString, SharedString) = match &ide.workspace {
            Some(workspace) => (workspace.title.clone().into(), workspace.path.clone().into()),
            None => (tr(cx, "agent.chooseProject"), tr(cx, "native.projects")),
        };
        let project = crate::ui::agent::with_project_menu(
            Button::new("greeting-project").ghost().small().icon(Icon::new(IconName::FolderOpen)).label(project).dropdown_caret(true).tooltip(path),
            cx,
        );
        let suggestions = (!self.suggestions.is_empty() && self.trigger.is_some()).then(|| self.render_suggestions(cx));
        let composer = self.render_composer_wide(locale, window, cx);
        let footer = self.render_composer_footer(locale, false, cx);
        v_flex()
            .id("greeting")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .items_center()
            .justify_center()
            .px_4()
            .child(
                v_flex()
                    .w_full()
                    .max_w(px(crate::ui::agent::COLUMN))
                    .px_1()
                    .child(
                        v_flex()
                            .items_center()
                            .gap_2()
                            .pb_4()
                            .child(img("brand/rainy-mark.svg").size(px(36.)))
                            .child(div().text_2xl().font_semibold().text_center().child(tr(cx, "agent.greeting")))
                            .child(project),
                    )
                    .children(suggestions)
                    .child(composer)
                    .child(footer),
            )
            .into_any_element()
    }

    /// The optical look's new chat: the field of characters behind the page, the greeting with the user's name and
    /// the project, the usage overview, the pet on the composer and the glow behind it. The overview gives way
    /// in a short window, so the composer stays in view.
    fn render_optics_greeting(&mut self, locale: Locale, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        use crate::ui::optics::{self, backdrop::Backdrop, look::Look, pet, usage::UsageCard};
        let backdrop = self.backdrop.get_or_insert_with(|| cx.new(|cx| Backdrop::new(window, cx))).clone();
        // The page came (back): a new chat after a conversation, Agent mode after the IDE, the look just turned on.
        if !std::mem::replace(&mut self.optics_shown, true) {
            backdrop.update(cx, |backdrop, cx| backdrop.wake(window, cx));
        }
        let store = self.store.clone();
        let usage = self.usage.get_or_insert_with(|| cx.new(|_| UsageCard::new(store))).clone();
        let ide = crate::store::ide::Ide::global(cx).read(cx);
        let (project, path): (SharedString, SharedString) = match &ide.workspace {
            Some(workspace) => (workspace.title.clone().into(), workspace.path.clone().into()),
            None => (tr(cx, "agent.chooseProject"), tr(cx, "native.projects")),
        };
        let project = crate::ui::agent::with_project_menu(
            Button::new("greeting-project").ghost().small().icon(Icon::new(IconName::FolderOpen)).label(project).dropdown_caret(true).tooltip(path),
            cx,
        );
        let title: SharedString = match optics::user_name() {
            Some(name) => i18n::format(locale, "optics.greeting", &[("name", &name)]).into(),
            None => i18n::text(locale, "optics.greetingPlain").into(),
        };
        let suggestions = (!self.suggestions.is_empty() && self.trigger.is_some()).then(|| self.render_suggestions(cx));
        let composer = self.render_composer_wide(locale, window, cx);
        let look = Look::of(cx.theme());
        let card = (window.viewport_size().height >= px(OPTICS_CARD_HEIGHT)).then(|| div().px_3().pb_2().child(usage));
        // The pet stands on the composer's top edge, unless the suggestion list is there; either keeps the greeting
        // clear of the glow (`optics::greeting_clearance`).
        let companions = suggestions.is_none().then(|| h_flex().h(px(pet::row_height())).px_3().pr_8().justify_end().items_end().child(pet::render(&backdrop, window, cx)));
        // Auto margins centre the column while it fits and let it scroll from its top when it does not. The column is
        // laid out from the bottom up, so the glow behind the composer is painted first and the pet, a suggestion
        // list, the overview and the greeting are painted over its light, not under it.
        let page = v_flex()
            .id("greeting")
            .size_full()
            .overflow_y_scroll()
            .items_center()
            .px_4()
            .py_6()
            .child(
                v_flex()
                    .flex_col_reverse()
                    .my_auto()
                    .flex_shrink_0()
                    .w_full()
                    .max_w(px(crate::ui::agent::COLUMN))
                    .px_1()
                    .child(div().relative().child(optics::glow(&look)).child(composer))
                    .children(suggestions.map(|list| div().min_h(px(pet::row_height())).flex().flex_col().justify_end().child(list)))
                    .children(companions)
                    .children(card)
                    .child(
                        h_flex()
                            .px_3()
                            .pb(px(optics::GREETING_GAP))
                            .gap_3()
                            .items_center()
                            .justify_between()
                            .child(
                                h_flex()
                                    .min_w_0()
                                    .gap_2()
                                    .items_center()
                                    .child(img("brand/rainy-mark.svg").flex_shrink_0().size(px(26.)))
                                    .child(div().min_w_0().text_xl().font_semibold().overflow_hidden().text_ellipsis().whitespace_nowrap().child(title)),
                            )
                            .child(project),
                    ),
            );
        div()
            .id("optics-page")
            .relative()
            .flex_1()
            .min_h_0()
            .overflow_hidden()
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, window, cx| {
                if let Some(backdrop) = this.backdrop.clone() {
                    backdrop.update(cx, |backdrop, cx| backdrop.pointer_moved(event.position, window, cx));
                }
            }))
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                if !*hovered && let Some(backdrop) = this.backdrop.clone() {
                    backdrop.update(cx, |backdrop, _| backdrop.pointer_left());
                }
            }))
            .on_mouse_down(MouseButton::Left, cx.listener(|this, event: &MouseDownEvent, window, cx| {
                if let Some(backdrop) = this.backdrop.clone() {
                    backdrop.update(cx, |backdrop, cx| backdrop.ripple(event.position, window, cx));
                }
            }))
            .child(div().absolute().inset_0().child(backdrop))
            .child(page)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use ::core::prelude::v1::test;
    use std::collections::{HashMap, VecDeque};

    use rainy_protocol::ExternalAgentStatus;
    use rainy_protocol::reasoning::Level;

    use crate::i18n::Locale;
    use crate::ui::chat::drafts::{merge_images, merge_text, touch_and_evict_draft};
    use crate::ui::chat::model_card::{
        CardState, THINKING, external_default_selection, external_model_display, external_ref_from_status, short_model_name, thinking_info, thinking_label,
    };
    use crate::ui::chat::suggestions::{antigravity_fallback, apply_reference_completion};

    #[test]
    fn names_models_the_way_their_vendors_do() {
        assert_eq!(short_model_name("claude-opus-5-5", Some("Claude Opus 5.5")), "Opus 5.5");
        assert_eq!(short_model_name("deepseek-flash", Some("DeepSeek V4.1 Flash")), "DeepSeek V4.1 Flash");
        assert_eq!(short_model_name("claude-haiku-4-5-20251001", None), "Haiku 4.5");
        assert_eq!(short_model_name("gpt-6.1-sol", None), "GPT-6.1 Sol");
        assert_eq!(short_model_name("openai/gpt-5.4-mini", None), "GPT-5.4 Mini");
        assert_eq!(short_model_name("gpt-oss-120b", None), "GPT-OSS 120b");
        assert_eq!(short_model_name("glm-5.3", None), "GLM-5.3");
        assert_eq!(short_model_name("deepseek-v4-pro", None), "DeepSeek V4 Pro");
        assert_eq!(short_model_name("Qwen3.8-27B", None), "Qwen3.8 27b");
        assert_eq!(short_model_name("gemini-3.1-pro-preview", None), "Gemini 3.1 Pro Preview");
        assert_eq!(short_model_name("house-model", None), "house-model");
    }

    #[test]
    fn labels_every_level() {
        for (level, key, info) in THINKING {
            assert_ne!(thinking_label(Locale::Zh, level, false), key, "{level:?}");
            assert_ne!(thinking_label(Locale::En, level, false), key, "{level:?}");
            assert_ne!(thinking_info(Locale::Zh, level, false), info, "{level:?}");
            assert_ne!(thinking_info(Locale::En, level, false), info, "{level:?}");
        }
        assert_eq!(thinking_label(Locale::Zh, Level::XHigh, false), "超高");
        // A model that only switches reasoning names its level On.
        assert_eq!(thinking_label(Locale::Zh, Level::High, true), "开启");
        assert_eq!(thinking_label(Locale::Zh, Level::Off, true), "关闭");
    }

    #[test]
    fn an_unsent_message_goes_in_front_of_what_was_typed_since() {
        assert_eq!(merge_text("你好".into(), "".into()), "你好");
        assert_eq!(merge_text("你好".into(), "  \n".into()), "你好");
        assert_eq!(merge_text("你好".into(), "再见".into()), "你好\n再见");
        assert!(merge_images(Vec::new(), Vec::new()).is_empty());
    }

    #[test]
    fn antigravity_offers_swarm_and_teamwork_until_it_lists_its_own() {
        let fallback = antigravity_fallback(Locale::En);
        assert_eq!(fallback.iter().map(|command| command.name.as_str()).collect::<Vec<_>>(), vec!["swarm", "teamwork"]);
        assert!(fallback.iter().all(|command| command.hint.is_some() && command.description.is_some()));
    }

    #[test]
    fn unprobed_external_agent_defaults_to_default_sentinel() {
        let status = ExternalAgentStatus {
            kind: "codex".into(),
            label: "Codex".into(),
            enabled: true,
            installed: true,
            binary: None,
            args: Vec::new(),
            binary_path: Some("/usr/bin/codex".into()),
            version: None,
            models: Vec::new(),
            default_model: None,
            default_thinking: None,
            install_hint: None,
            login_hint: None,
            error: None,
            commands: Vec::new(),
            quota: None,
        };
        let sel = external_default_selection(&status, None);
        assert_eq!(sel.provider, "external:codex");
        assert_eq!(sel.model, "default");
        let backend = external_ref_from_status(&status);
        assert_eq!(external_model_display(&sel.model, Some(&status), &backend, Locale::En), "Codex");
    }

    #[test]
    fn reference_completion_guards_utf8_and_bounds() {
        let text = "请检查 @src/测试";
        let start = text.find('@').unwrap();
        let (next, line, ch) = apply_reference_completion(text, start, text.len(), "src/main.rs", false).unwrap();
        assert_eq!(next, "请检查 @src/main.rs ");
        assert_eq!(line, 0);
        assert_eq!(ch, "请检查 @src/main.rs ".chars().count() as u32);
        // Inverted or out-of-bounds or mid-UTF-8 boundaries return None without panicking.
        assert!(apply_reference_completion(text, text.len(), start, "a", false).is_none());
        assert!(apply_reference_completion(text, start, text.len() + 5, "a", false).is_none());
        assert!(apply_reference_completion(text, 1, text.len(), "a", false).is_none());
        assert!(apply_reference_completion(text, start, text.len() - 1, "a", false).is_none());
    }

    #[test]
    fn draft_lru_evicts_oldest_session_and_keeps_new_chat() {
        let mut drafts: HashMap<Option<String>, &str> = HashMap::new();
        let mut order: VecDeque<Option<String>> = VecDeque::new();
        drafts.insert(None, "new");
        touch_and_evict_draft(&mut drafts, &mut order, None, 2);
        drafts.insert(Some("a".into()), "A");
        touch_and_evict_draft(&mut drafts, &mut order, Some("a".into()), 2);
        drafts.insert(Some("b".into()), "B");
        touch_and_evict_draft(&mut drafts, &mut order, Some("b".into()), 2);
        assert_eq!(drafts.len(), 2);
        assert!(drafts.contains_key(&None));
        assert!(!drafts.contains_key(&Some("a".into())));
        assert!(drafts.contains_key(&Some("b".into())));
    }

    #[test]
    fn model_card_state_resets_when_session_changes() {
        let mut state = CardState {
            session: Some("s1".into()),
            list: true,
            orchestrate: true,
            drag: Some(2),
            quota_asked: Some("codex".into()),
            pending: None,
        };
        assert!(!state.sync_session(&Some("s1".into())));
        assert!(state.list && state.orchestrate && state.drag == Some(2));
        assert!(state.sync_session(&Some("s2".into())));
        assert_eq!(state.session.as_deref(), Some("s2"));
        assert!(!state.list && !state.orchestrate && state.drag.is_none() && state.quota_asked.is_none());
    }
}
