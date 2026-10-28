//! Search and replace across the project (`SearchView.tsx`).

use std::collections::HashSet;
use std::ops::Range;
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Icon, IconName, Selectable as _, Sizable as _, Theme, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use rainy_protocol::{TextReplaceResult, TextSearchFile, TextSearchMatch, TextSearchResult};
use serde_json::{Value, json};

use crate::store::ide::{Ide, IdeEvent};
use crate::store::{Store, StoreEvent};
use crate::ui::ide::file_icon;
use crate::ui::{tr, trf};

/// The height of every line of the results; the list only builds the lines on screen, so they must all be this tall.
const ROW_HEIGHT: f32 = 24.;

/// One line of the results: a file's header, or one of its matches (file index, match index).
#[derive(Clone, Copy)]
enum Row {
    File(usize),
    Match(usize, usize),
}

pub struct SearchView {
    ide: Entity<Ide>,
    query: Entity<InputState>,
    replacement: Entity<InputState>,
    include: Entity<InputState>,
    exclude: Entity<InputState>,
    case_sensitive: bool,
    whole_word: bool,
    regex: bool,
    show_replace: bool,
    show_filters: bool,
    result: Option<Rc<TextSearchResult>>,
    /// The lines of `result` that are shown: every file, and the matches of those not collapsed.
    rows: Vec<Row>,
    scroll: UniformListScrollHandle,
    error: Option<SharedString>,
    searching: bool,
    collapsed: HashSet<(String, String)>,
    /// Bumped by every search, so an older answer arriving late is dropped.
    generation: u64,
    debounce: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

/// The byte range of a UTF-16 span of `text`.
fn utf16_range(text: &str, start: u32, length: u32) -> std::ops::Range<usize> {
    let (mut units, mut from, mut to) = (0u32, text.len(), text.len());
    for (index, c) in text.char_indices() {
        if units == start {
            from = index;
        }
        if units == start + length {
            to = index;
            break;
        }
        units += c.len_utf16() as u32;
    }
    from.min(to)..to
}

impl SearchView {
    pub fn new(ide: Entity<Ide>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let query = cx.new(|cx| InputState::new(window, cx).placeholder(tr(cx, "search.searchPlaceholder")));
        let replacement = cx.new(|cx| InputState::new(window, cx).placeholder(tr(cx, "search.searchReplacePlaceholder")));
        let include = cx.new(|cx| InputState::new(window, cx).placeholder(tr(cx, "search.searchIncludePlaceholder")));
        let exclude = cx.new(|cx| InputState::new(window, cx).placeholder(tr(cx, "search.searchExcludePlaceholder")));
        let mut subscriptions = Vec::new();
        for input in [&query, &include, &exclude] {
            subscriptions.push(cx.subscribe_in(input, window, |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::Change => this.schedule(window, cx),
                InputEvent::PressEnter { .. } => this.search(window, cx),
                _ => {}
            }));
        }
        subscriptions.push(cx.subscribe_in(&ide, window, |this, _, event: &IdeEvent, window, cx| {
            match event {
                IdeEvent::WorkspaceChanged => {
                    this.set_result(None);
                    this.error = None;
                    cx.notify();
                }
                // Results follow what was just written.
                IdeEvent::Saved if this.result.is_some() => this.schedule(window, cx),
                _ => {}
            }
        }));
        // The fields' hints follow the interface language.
        subscriptions.push(cx.subscribe_in(&Store::global(cx), window, |this, _, event: &StoreEvent, window, cx| {
            if matches!(event, StoreEvent::PrefsChanged) {
                this.set_placeholders(window, cx);
            }
        }));
        Self {
            ide,
            query,
            replacement,
            include,
            exclude,
            case_sensitive: false,
            whole_word: false,
            regex: false,
            show_replace: false,
            show_filters: false,
            result: None,
            rows: Vec::new(),
            scroll: UniformListScrollHandle::new(),
            error: None,
            searching: false,
            collapsed: HashSet::new(),
            generation: 0,
            debounce: None,
            _subscriptions: subscriptions,
        }
    }

    /// The search field, to put the cursor in.
    pub fn query(&self) -> Entity<InputState> {
        self.query.clone()
    }

    /// Show the replace field.
    pub fn show_replace(&mut self, cx: &mut Context<Self>) {
        self.show_replace = true;
        cx.notify();
    }

    fn set_placeholders(&self, window: &mut Window, cx: &mut Context<Self>) {
        let fields = [
            (&self.query, "search.searchPlaceholder"),
            (&self.replacement, "search.searchReplacePlaceholder"),
            (&self.include, "search.searchIncludePlaceholder"),
            (&self.exclude, "search.searchExcludePlaceholder"),
        ];
        for (input, key) in fields {
            let placeholder = tr(cx, key);
            input.update(cx, |state, cx| state.set_placeholder(placeholder, window, cx));
        }
    }

    fn set_result(&mut self, result: Option<TextSearchResult>) {
        self.result = result.map(Rc::new);
        self.rebuild_rows();
    }

    fn rebuild_rows(&mut self) {
        self.rows.clear();
        let Some(result) = &self.result else { return };
        for (index, file) in result.files.iter().enumerate() {
            self.rows.push(Row::File(index));
            if self.collapsed.is_empty() || !self.collapsed.contains(&(file.root_id.clone(), file.path.clone())) {
                self.rows.extend((0..file.matches.len()).map(|found| Row::Match(index, found)));
            }
        }
    }

    fn options(&self, cx: &App) -> Value {
        json!({
            "pattern": self.query.read(cx).value().to_string(),
            "regex": self.regex,
            "caseSensitive": self.case_sensitive,
            "wholeWord": self.whole_word,
            "include": self.include.read(cx).value().to_string(),
            "exclude": self.exclude.read(cx).value().to_string(),
        })
    }

    /// Search a moment after the last keystroke.
    fn schedule(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.debounce = Some(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(300)).await;
            let _ = this.update_in(cx, |this, window, cx| this.search(window, cx));
        }));
    }

    fn search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.debounce = None;
        self.generation += 1;
        let generation = self.generation;
        let workspace = self.ide.read(cx).workspace.as_ref().map(|workspace| workspace.workspace_id.clone());
        let mut params = self.options(cx);
        if workspace.is_none() || params["pattern"].as_str().is_none_or(str::is_empty) {
            self.set_result(None);
            self.error = None;
            self.searching = false;
            cx.notify();
            return;
        }
        params["workspaceId"] = json!(workspace);
        let call = Store::global(cx).read(cx).call("search.text", params);
        self.searching = true;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = call.await;
            let _ = this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                this.searching = false;
                match result.map(serde_json::from_value::<TextSearchResult>) {
                    Ok(Ok(result)) => {
                        this.set_result(Some(result));
                        this.error = None;
                    }
                    Ok(Err(error)) => this.error = Some(error.to_string().into()),
                    Err(error) => this.error = Some(crate::ui::error_text(cx, &error)),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn toggle(&mut self, flag: fn(&mut Self) -> &mut bool, window: &mut Window, cx: &mut Context<Self>) {
        let value = flag(self);
        *value = !*value;
        self.search(window, cx);
    }

    /// Ask, then replace every match on disk. Files with unsaved edits are left alone, so nothing typed is lost.
    fn confirm_replace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(result) = self.result.clone() else { return };
        let ide = self.ide.read(cx);
        let clean: Vec<&TextSearchFile> = result.files.iter().filter(|file| !ide.is_dirty(&key_of(file))).collect();
        if clean.is_empty() {
            return;
        }
        let files: Vec<Value> = clean.iter().map(|file| json!({ "rootId": file.root_id, "path": file.path })).collect();
        let matches: u32 = clean.iter().map(|file| file.matches.len() as u32).sum();
        let replacement = self.replacement.read(cx).value().to_string();
        let mut params = self.options(cx);
        params["workspaceId"] = json!(ide.workspace.as_ref().map(|workspace| workspace.workspace_id.clone()));
        params["replacement"] = json!(replacement);
        params["files"] = json!(files);
        let shown = if replacement.is_empty() { tr(cx, "search.searchReplaceEmpty").to_string() } else { replacement };
        let description = trf(cx, "search.searchReplaceBody", &[("matches", &matches), ("files", &files.len()), ("replacement", &shown)]);
        let view = cx.entity();
        window.open_alert_dialog(cx, move |alert, _, cx| {
            let (view, params) = (view.clone(), params.clone());
            alert
                .title(tr(cx, "search.searchReplaceTitle"))
                .description(description.clone())
                .ok_text(tr(cx, "search.searchReplaceAll"))
                .cancel_text(tr(cx, "search.cancel"))
                .show_cancel(true)
                .on_ok(move |_, window, cx| {
                    let params = params.clone();
                    view.update(cx, |view, cx| view.replace(params, window, cx));
                    true
                })
        });
    }

    fn replace(&mut self, params: Value, window: &mut Window, cx: &mut Context<Self>) {
        let call = Store::global(cx).read(cx).call("search.replace", params);
        cx.spawn_in(window, async move |this, cx| {
            let result = call.await;
            let _ = this.update_in(cx, |this, window, cx| {
                match result.map(serde_json::from_value::<TextReplaceResult>) {
                    Ok(Ok(result)) => {
                        let count: u32 = result.files.iter().map(|file| file.replaced).sum();
                        window.push_notification(trf(cx, "search.searchReplaced", &[("count", &count), ("files", &result.files.len())]).to_string(), cx);
                        for failed in &result.failed {
                            let text = trf(cx, "search.searchReplaceFailed", &[("path", &failed.path), ("reason", &failed.message)]);
                            window.push_notification(Notification::error(text), cx);
                        }
                    }
                    Ok(Err(error)) => window.push_notification(Notification::error(error.to_string()), cx),
                    Err(error) => window.push_notification(crate::ui::error_notification(cx, &error), cx),
                }
                // Clean editors follow the files on disk; the results are searched again.
                this.ide.update(cx, |ide, cx| ide.check_files(cx));
                this.search(window, cx);
            });
        })
        .detach();
    }

    /// The results' lines in `range`, built only while they are on screen.
    fn render_rows(&self, range: Range<usize>, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let Some(result) = self.result.clone() else { return Vec::new() };
        let theme = cx.theme().clone();
        let highlight = HighlightStyle { background_color: Some(theme.warning.opacity(0.35)), ..Default::default() };
        range
            .filter_map(|row| match *self.rows.get(row)? {
                Row::File(index) => Some(self.render_file(row, result.files.get(index)?, &theme, cx)),
                Row::Match(index, found) => {
                    let file = result.files.get(index)?;
                    Some(self.render_match(row, file, file.matches.get(found)?, highlight, &theme))
                }
            })
            .collect()
    }

    fn render_file(&self, row: usize, file: &TextSearchFile, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let id = (file.root_id.clone(), file.path.clone());
        let collapsed = self.collapsed.contains(&id);
        let (name, directory) = match file.path.rsplit_once('/') {
            Some((directory, name)) => (name.to_owned(), directory.to_owned()),
            None => (file.path.clone(), String::new()),
        };
        h_flex()
            .id(("search-file", row))
            .w_full()
            .h(px(ROW_HEIGHT))
            .px_2()
            .gap_1()
            .items_center()
            .text_sm()
            .cursor_pointer()
            .hover(|style| style.bg(theme.list_hover))
            .child(Icon::new(if collapsed { IconName::ChevronRight } else { IconName::ChevronDown }).xsmall().flex_shrink_0().text_color(theme.muted_foreground))
            .child(Icon::new(file_icon(&name)).small().flex_shrink_0())
            // A long name gives way before the match count does.
            .child(div().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().child(name))
            .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().text_xs().text_color(theme.muted_foreground).child(directory))
            .child(div().flex_shrink_0().px_1p5().rounded_full().bg(theme.muted).text_xs().child(file.matches.len().to_string()))
            .on_click(cx.listener(move |this, _, _, cx| {
                if !this.collapsed.remove(&id) {
                    this.collapsed.insert(id.clone());
                }
                this.rebuild_rows();
                cx.notify();
            }))
            .into_any_element()
    }

    fn render_match(&self, row: usize, file: &TextSearchFile, found: &TextSearchMatch, highlight: HighlightStyle, theme: &Theme) -> AnyElement {
        let preview = found.preview.trim_end();
        let range = utf16_range(&found.preview, found.preview_column, found.length);
        let text = StyledText::new(preview.to_owned()).with_highlights(Some((range, highlight)).into_iter().filter(|(range, _)| range.end <= preview.len()));
        let (key, line, column) = (key_of(file), found.line.saturating_sub(1), found.column);
        let ide = self.ide.clone();
        h_flex()
            .id(("search-match", row))
            .w_full()
            .h(px(ROW_HEIGHT))
            .pl(px(30.))
            .pr_2()
            .gap_2()
            .items_center()
            .text_sm()
            .cursor_pointer()
            .hover(|style| style.bg(theme.list_hover))
            .child(div().w(px(32.)).flex_shrink_0().text_xs().text_color(theme.muted_foreground).child(found.line.to_string()))
            .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(text))
            .on_click(move |_, window, cx| {
                let key = key.clone();
                ide.update(cx, |ide, cx| ide.reveal(key, line, column, window, cx));
            })
            .into_any_element()
    }
}

/// The editor key of a result file.
fn key_of(file: &TextSearchFile) -> String {
    crate::store::ide::root_key(Some(&file.root_id), &file.path)
}

impl Render for SearchView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let has_workspace = self.ide.read(cx).workspace.is_some();
        let flag = |id: &'static str, label: &'static str, active: bool, tooltip: SharedString| {
            Button::new(id).ghost().xsmall().label(label).selected(active).tooltip(tooltip)
        };
        let case = flag("search-case", "Aa", self.case_sensitive, tr(cx, "search.searchMatchCase"))
            .on_click(cx.listener(|this, _, window, cx| this.toggle(|this| &mut this.case_sensitive, window, cx)));
        let word = flag("search-word", "ab", self.whole_word, tr(cx, "search.searchWholeWord"))
            .on_click(cx.listener(|this, _, window, cx| this.toggle(|this| &mut this.whole_word, window, cx)));
        let regex = flag("search-regex", ".*", self.regex, tr(cx, "search.searchRegex"))
            .on_click(cx.listener(|this, _, window, cx| this.toggle(|this| &mut this.regex, window, cx)));
        let summary = self.result.as_ref().map(|result| {
            let word = |one: &str, many: &str, count: usize| tr(cx, if count == 1 { one } else { many });
            let files = result.files.len();
            trf(cx, "search.searchSummary", &[
                ("matches", &result.matches),
                ("resultWord", &word("search.searchResultWord", "search.searchResultsWord", result.matches as usize)),
                ("files", &files),
                ("fileWord", &word("search.searchFileWord", "search.searchFilesWord", files)),
            ])
        });
        let truncated = self.result.as_ref().is_some_and(|result| result.truncated);
        let empty = self.result.as_ref().is_some_and(|result| result.files.is_empty());
        let can_replace = self.result.as_ref().is_some_and(|result| !result.files.is_empty());
        let no_rows = self.rows.is_empty();
        let list = uniform_list("search-results", self.rows.len(), cx.processor(|this, range: Range<usize>, _, cx| this.render_rows(range, cx)))
            .flex_1()
            .track_scroll(&self.scroll);
        v_flex()
            .size_full()
            .child(
                v_flex()
                    .flex_shrink_0()
                    .p_2()
                    .gap_1p5()
                    .child(
                        h_flex()
                            .gap_1()
                            .child(
                                Button::new("search-toggle-replace")
                                    .ghost()
                                    .xsmall()
                                    .icon(if self.show_replace { IconName::ChevronDown } else { IconName::ChevronRight })
                                    .tooltip(tr(cx, "search.searchToggleReplace"))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.show_replace = !this.show_replace;
                                        cx.notify();
                                    })),
                            )
                            .child(div().flex_1().child(Input::new(&self.query).small().disabled(!has_workspace)))
                            .child(case)
                            .child(word)
                            .child(regex),
                    )
                    .when(self.show_replace, |this| {
                        this.child(
                            h_flex()
                                .gap_1()
                                .pl(px(26.))
                                .child(div().flex_1().child(Input::new(&self.replacement).small()))
                                .child(
                                    Button::new("search-replace-all")
                                        .ghost()
                                        .xsmall()
                                        .icon(IconName::Replace)
                                        .tooltip(tr(cx, "search.searchReplaceAll"))
                                        .disabled(!can_replace)
                                        .on_click(cx.listener(|this, _, window, cx| this.confirm_replace(window, cx))),
                                ),
                        )
                    })
                    .child(
                        h_flex()
                            .pl(px(26.))
                            .child(
                                Button::new("search-filters")
                                    .ghost()
                                    .xsmall()
                                    .icon(IconName::Ellipsis)
                                    .tooltip(tr(cx, "search.searchFilters"))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.show_filters = !this.show_filters;
                                        cx.notify();
                                    })),
                            )
                            .child(div().flex_1())
                            .when_some(summary, |this, summary| this.child(div().text_xs().text_color(theme.muted_foreground).child(summary))),
                    )
                    .when(self.show_filters, |this| {
                        this.child(
                            v_flex()
                                .pl(px(26.))
                                .gap_1()
                                .child(div().text_xs().text_color(theme.muted_foreground).child(tr(cx, "search.searchInclude")))
                                .child(Input::new(&self.include).small())
                                .child(div().text_xs().text_color(theme.muted_foreground).child(tr(cx, "search.searchExclude")))
                                .child(Input::new(&self.exclude).small()),
                        )
                    }),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .when(!has_workspace, |this| this.child(div().p_3().text_sm().text_color(theme.muted_foreground).child(tr(cx, "search.searchNoWorkspace"))))
                    .when(self.searching && no_rows, |this| {
                        this.child(h_flex().p_3().gap_2().text_sm().text_color(theme.muted_foreground).child(Spinner::new().small()).child(tr(cx, "search.searchSearching")))
                    })
                    .when_some(self.error.clone(), |this, error| this.child(div().p_3().text_sm().text_color(theme.danger).child(error)))
                    .when(empty && !self.searching, |this| this.child(div().p_3().text_sm().text_color(theme.muted_foreground).child(tr(cx, "search.searchNone"))))
                    .when(truncated, |this| {
                        let count = self.result.as_ref().map_or(0, |result| result.matches);
                        this.child(div().px_3().py_1().text_xs().text_color(crate::theme::warning_text(cx)).child(trf(cx, "search.searchTruncated", &[("count", &count)])))
                    })
                    .child(v_flex().relative().flex_1().min_h_0().child(list).vertical_scrollbar(&self.scroll)),
            )
    }
}
