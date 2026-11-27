//! The headings of the open Markdown document in the side pane (`Outline.tsx`), as in Typora: click one to go there; the
//! one being read is marked, following the rich editor's scrolling or the source editor's cursor.

use std::rc::Rc;
use std::sync::OnceLock;

use gpui_kit::component::input::InputEvent;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{ActiveTheme as _, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use regex::Regex;

use crate::store::ide::Ide;
use crate::ui::ide::editor_area::EditorArea;
use crate::ui::ide::viewers;
use crate::ui::tr;

/// One heading, in document order.
#[derive(Debug, Clone, PartialEq)]
pub struct Heading {
    /// 1 to 6.
    pub level: usize,
    /// Its text without Markdown syntax.
    pub text: String,
    /// 1-based line of the file where it starts.
    pub line: usize,
}

/// A regular expression compiled once, on first use.
pub(crate) fn pattern(cell: &'static OnceLock<Regex>, source: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(source).expect("a valid pattern"))
}

/// A heading's text as a reader sees it (`plainHeading`).
pub fn plain_heading(raw: &str) -> String {
    static IMAGE: OnceLock<Regex> = OnceLock::new();
    static LINK: OnceLock<Regex> = OnceLock::new();
    static REFERENCE: OnceLock<Regex> = OnceLock::new();
    static TAG: OnceLock<Regex> = OnceLock::new();
    static MARKS: OnceLock<Regex> = OnceLock::new();
    let text = pattern(&IMAGE, r"!\[([^\]]*)\]\([^)]*\)").replace_all(raw, "$1");
    let text = pattern(&LINK, r"\[([^\]]+)\]\([^)]*\)").replace_all(&text, "$1");
    let text = pattern(&REFERENCE, r"\[([^\]]+)\]\[[^\]]*\]").replace_all(&text, "$1");
    let text = pattern(&TAG, r"<[^>]+>").replace_all(&text, "");
    let text = pattern(&MARKS, r"\*\*|__|~~|`").replace_all(&text, "");
    // Single `*` and `_` emphasis: a marker next to a word on one side and not on the other.
    let chars: Vec<char> = text.chars().collect();
    let word = |c: Option<&char>| c.is_some_and(|c| c.is_alphanumeric());
    let mut out = String::with_capacity(text.len());
    let mut index = 0;
    while index < chars.len() {
        let c = chars[index];
        if c == '\\' && index + 1 < chars.len() && "\\`*_{}[]()#+-.!|<>~$".contains(chars[index + 1]) {
            out.push(chars[index + 1]);
            index += 2;
            continue;
        }
        if c == '*' || c == '_' {
            let (before, after) = (index.checked_sub(1).and_then(|at| chars.get(at)), chars.get(index + 1));
            let opens = !word(before) && after.is_some_and(|after| !after.is_whitespace());
            let closes = before.is_some_and(|before| !before.is_whitespace()) && !word(after);
            if opens || closes {
                index += 1;
                continue;
            }
        }
        out.push(c);
        index += 1;
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The lines of YAML (`---`) or TOML (`+++`) front matter at the start, if any (`splitFrontMatter`).
pub(crate) fn front_matter_lines(lines: &[&str]) -> usize {
    let opening = lines.first().map(|line| line.trim_end_matches([' ', '\t']));
    let closing: &[&str] = match opening {
        Some("---") => &["---", "..."],
        Some("+++") => &["+++"],
        _ => return 0,
    };
    lines.iter().skip(1).position(|line| closing.contains(&line.trim_end_matches([' ', '\t']))).map_or(0, |at| at + 2)
}

/// The ATX and setext headings outside fenced code, math blocks and front matter (`markdownOutline`).
pub fn markdown_outline(markdown: &str) -> Vec<Heading> {
    static QUOTE: OnceLock<Regex> = OnceLock::new();
    static FENCE: OnceLock<Regex> = OnceLock::new();
    static ATX: OnceLock<Regex> = OnceLock::new();
    static UNDERLINE: OnceLock<Regex> = OnceLock::new();
    static BREAK: OnceLock<Regex> = OnceLock::new();
    let lines: Vec<&str> = markdown.split('\n').collect();
    let mut headings = Vec::new();
    let mut fence: Option<String> = None;
    let mut math = false;
    // Consecutive paragraph lines, which a `===` or `---` line turns into a setext heading.
    let mut paragraph: Option<(usize, Vec<String>)> = None;
    for (index, raw) in lines.iter().enumerate().skip(front_matter_lines(&lines)) {
        let raw = raw.trim_end_matches('\r');
        let line = pattern(&QUOTE, r"^(?: {0,3}>[ \t]?)+").replace(raw, "").into_owned();
        let fence_re = pattern(&FENCE, r"^ {0,3}(`{3,}|~{3,})");
        if let Some(open) = &fence {
            if let Some(found) = fence_re.captures(&line) {
                let closing = &found[1];
                let rest = line[found.get(0).map_or(0, |m| m.end())..].trim_matches([' ', '\t']);
                if rest.is_empty() && closing.chars().next() == open.chars().next() && closing.len() >= open.len() {
                    fence = None;
                }
            }
            continue;
        }
        let trimmed = line.trim_start_matches(' ');
        let indent = line.len() - trimmed.len();
        if math {
            if indent <= 3 && trimmed.trim_end_matches([' ', '\t']) == "$$" {
                math = false;
            }
            continue;
        }
        if let Some(found) = fence_re.captures(&line) {
            fence = Some(found[1].to_owned());
            paragraph = None;
            continue;
        }
        if indent <= 3 && trimmed.starts_with("$$") {
            // `$$ x $$` on one line is a whole block; a line that only opens one starts it.
            let body = trimmed.trim_end_matches([' ', '\t']);
            if !(body.len() > 2 && body[2..].ends_with("$$")) || body == "$$" {
                math = true;
            }
            paragraph = None;
            continue;
        }
        if let Some(found) = pattern(&ATX, r"^ {0,3}(#{1,6})(?:[ \t]+(.*?))?(?:[ \t]+#+)?[ \t]*$").captures(&line) {
            headings.push(Heading { level: found[1].len(), text: plain_heading(found.get(2).map_or("", |m| m.as_str())), line: index + 1 });
            paragraph = None;
            continue;
        }
        if let Some(found) = pattern(&UNDERLINE, r"^ {0,3}(=+|-+)[ \t]*$").captures(&line)
            && let Some((start, text)) = paragraph.take()
        {
            let level = if found[1].starts_with('=') { 1 } else { 2 };
            headings.push(Heading { level, text: plain_heading(&text.join(" ")), line: start + 1 });
            continue;
        }
        // Blank lines, lists, tables, rules and HTML end a paragraph that could become a setext heading.
        if line.trim().is_empty() || pattern(&BREAK, r"^ {0,3}(?:[-*+][ \t]|\d+[.)][ \t]|\||<|[-*_][ \t]*[-*_][ \t]*[-*_])").is_match(&line) {
            paragraph = None;
            continue;
        }
        match &mut paragraph {
            Some((_, text)) => text.push(line.trim().to_owned()),
            None => paragraph = Some((index, vec![line.trim().to_owned()])),
        }
    }
    headings
}

pub struct OutlineView {
    ide: Entity<Ide>,
    editor: Entity<EditorArea>,
    /// Whether the outline tab is currently visible in the sidebar.
    visible: bool,
    /// Cached headings for the last parsed key, invalidated only on `InputEvent::Change` or file switch.
    cache: Option<(String, Rc<Vec<Heading>>)>,
    headings_dirty: bool,
    /// Active `(key, editor_id, view)` fingerprint so unrelated `Ide` updates are skipped.
    watched: Option<(Option<String>, Option<EntityId>, Option<String>)>,
    last_line: Option<u32>,
    last_page_index: Option<Option<i64>>,
    _active: Vec<Subscription>,
    _subscriptions: Vec<Subscription>,
}

impl OutlineView {
    pub fn new(ide: Entity<Ide>, editor: Entity<EditorArea>, cx: &mut Context<Self>) -> Self {
        let subscriptions = vec![
            cx.observe(&ide, |this, _, cx| {
                this.sync_active(cx);
            }),
            cx.observe(&editor, |this, editor, cx| {
                if !this.visible {
                    return;
                }
                let index = this
                    .ide
                    .read(cx)
                    .active
                    .as_deref()
                    .and_then(|key| editor.read(cx).outline_index(key));
                if this.last_page_index != Some(index) {
                    this.last_page_index = Some(index);
                    cx.notify();
                }
            }),
        ];
        let mut view = Self {
            ide,
            editor,
            visible: true,
            cache: None,
            headings_dirty: true,
            watched: None,
            last_line: None,
            last_page_index: None,
            _active: Vec::new(),
            _subscriptions: subscriptions,
        };
        view.sync_active(cx);
        view
    }

    /// Pause outline tracking while the Outline sidebar tab is hidden; resume and refresh when shown.
    pub fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if self.visible == visible {
            return;
        }
        self.visible = visible;
        if visible {
            self.headings_dirty = true;
            self.sync_active(cx);
            cx.notify();
        } else {
            self._active.clear();
            self.watched = None;
        }
    }

    /// Follow the active editor's text changes and cursor line only when visible.
    fn sync_active(&mut self, cx: &mut Context<Self>) {
        if !self.visible {
            self.headings_dirty = true;
            return;
        }
        let (key, editor, view) = {
            let ide = self.ide.read(cx);
            let key = ide.active.clone();
            let editor = key.as_ref().and_then(|k| ide.files.get(k)).and_then(|file| file.editor.clone());
            let view = key.as_ref().and_then(|k| ide.views.get(k).cloned());
            (key, editor, view)
        };
        let fingerprint = (key.clone(), editor.as_ref().map(Entity::entity_id), view);
        if self.watched.as_ref() == Some(&fingerprint) {
            return;
        }
        self.watched = Some(fingerprint);
        self.headings_dirty = true;
        self.last_line = editor.as_ref().map(|ed| ed.read(cx).cursor_position().line);
        self.last_page_index = None;
        self._active = editor
            .map(|editor| {
                vec![
                    // Re-parse Markdown headings only when the document text actually changes.
                    cx.subscribe(&editor, |this, _, event: &InputEvent, cx| {
                        if matches!(event, InputEvent::Change) {
                            this.headings_dirty = true;
                            if this.visible {
                                cx.notify();
                            }
                        }
                    }),
                    // Highlight follows cursor line without cloning or re-parsing document text.
                    cx.observe(&editor, |this, editor, cx| {
                        if !this.visible {
                            return;
                        }
                        let line = editor.read(cx).cursor_position().line;
                        if this.last_line != Some(line) {
                            this.last_line = Some(line);
                            cx.notify();
                        }
                    }),
                ]
            })
            .unwrap_or_default();
        cx.notify();
    }

    /// The active Markdown file, its cached headings and the cursor's line in its source.
    fn headings(&mut self, cx: &App) -> Option<(String, Rc<Vec<Heading>>, usize)> {
        let ide = self.ide.read(cx);
        let key = ide.active.clone()?;
        let file = ide.files.get(&key)?;
        if viewers::preview_kind(&file.document.path) != Some("markdown") {
            return None;
        }
        let editor = file.editor.as_ref()?.read(cx);
        let line = editor.cursor_position().line as usize + 1;
        let stale = self.headings_dirty || !matches!(&self.cache, Some((cached_key, _)) if *cached_key == key);
        if stale {
            let text = editor.value().to_string();
            let headings = Rc::new(markdown_outline(&text.replace("\r\n", "\n")));
            self.cache = Some((key.clone(), headings));
            self.headings_dirty = false;
        }
        let headings = self.cache.as_ref().map(|(_, headings)| Rc::clone(headings))?;
        Some((key, headings, line))
    }
}

impl Render for OutlineView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let empty = |text: SharedString| div().p_4().text_sm().text_color(theme.muted_foreground).child(text).into_any_element();
        let Some((key, headings, cursor_line)) = self.headings(cx) else { return empty(tr(cx, "ide.ideOutlineNotMarkdown")) };
        if headings.is_empty() {
            return empty(tr(cx, "ide.ideOutlineEmpty"));
        }
        let page = self.ide.read(cx).views.get(&key).map(String::as_str) != Some("source");
        let active = match page.then(|| self.editor.read(cx).outline_index(&key)).flatten() {
            Some(index) => index,
            None => headings.iter().rposition(|heading| heading.line <= cursor_line).map_or(-1, |index| index as i64),
        };
        let top = headings.iter().map(|heading| heading.level).min().unwrap_or(1);
        v_flex()
            .id("outline")
            .size_full()
            .py_1()
            .overflow_y_scrollbar()
            .children(headings.iter().enumerate().map(|(index, heading)| {
                let current = index as i64 == active;
                let (editor, key, line) = (self.editor.clone(), key.clone(), heading.line);
                div()
                    .id(("heading", index))
                    .pl(px(12. + (heading.level - top) as f32 * 14.))
                    .pr_2()
                    .py_0p5()
                    .text_sm()
                    .truncate()
                    .cursor_pointer()
                    .when(heading.level == top, |this| this.font_weight(FontWeight::MEDIUM))
                    .when(current, |this| this.bg(theme.list_active).text_color(theme.foreground))
                    .when(!current, |this| this.text_color(theme.muted_foreground).hover(|style| style.bg(theme.list_hover)))
                    .child(if heading.text.is_empty() { " ".to_owned() } else { heading.text.clone() })
                    .on_click(move |_, window, cx| {
                        let key = key.clone();
                        editor.update(cx, |editor, cx| editor.reveal_heading(&key, index, line, window, cx));
                    })
            }))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use ::core::prelude::v1::test;

    use super::*;

    #[test]
    fn reads_headings_as_the_rich_editor_renders_them() {
        let text = "---\ntitle: x\n---\n# 标题 **一**\n\n```\n# not\n```\n## [Link](a.md) `code` ##\nSetext\n===\n> ### Quoted\n$$\n# math\n$$\n- item\n---\n";
        let headings = markdown_outline(text);
        let shown: Vec<(usize, &str, usize)> = headings.iter().map(|heading| (heading.level, heading.text.as_str(), heading.line)).collect();
        assert_eq!(shown, vec![(1, "标题 一", 4), (2, "Link code", 9), (1, "Setext", 10), (3, "Quoted", 12)]);
    }

    #[test]
    fn strips_inline_markdown() {
        assert_eq!(plain_heading("*em* and _under_ snake_case"), "em and under snake_case");
        assert_eq!(plain_heading("![pic](a.png) <b>bold</b> \\# hash"), "pic bold # hash");
    }
}
