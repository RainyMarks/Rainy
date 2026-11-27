//! Shared virtualized log-tail buffer and renderer reused by `ExperimentsView` and oversized file tailing in `EditorArea`.

use std::rc::Rc;

use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::*;

use crate::ui::tr;

/// Bounded log buffer limits so long-running logs never grow unbounded in RAM.
pub const MAX_TAIL_LINES: usize = 1500;
pub const MAX_TAIL_BYTES: usize = 256 * 1024;
/// Lines included when quoting tail logs into the chat composer.
pub const QUOTE_LINES: usize = 60;
/// Fixed row height for `uniform_list` virtualization.
const LOG_ROW_HEIGHT: f32 = 18.;

/// Bounded, line-indexed log buffer backed by `Rc<Vec<SharedString>>` for virtualized `uniform_list` rendering.
pub struct LogTailBuffer {
    text: String,
    lines: Rc<Vec<SharedString>>,
    pub total_bytes: u64,
    pub reset: bool,
    pub paused: bool,
    pub scroll: UniformListScrollHandle,
}

impl Default for LogTailBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl LogTailBuffer {
    pub fn new() -> Self {
        Self {
            text: String::new(),
            lines: Rc::new(Vec::new()),
            total_bytes: 0,
            reset: false,
            paused: false,
            scroll: UniformListScrollHandle::new(),
        }
    }

    /// Clear buffered log lines while keeping the active stream's byte offset/total.
    pub fn clear_lines(&mut self) {
        self.text.clear();
        self.lines = Rc::new(Vec::new());
        self.reset = false;
    }

    /// Reset buffer and stream metadata when switching targets or stopping a tail.
    pub fn reset_stream(&mut self) {
        self.text.clear();
        self.lines = Rc::new(Vec::new());
        self.total_bytes = 0;
        self.reset = false;
        self.paused = false;
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    pub fn scroll_to_bottom(&self) {
        if let Some(last) = self.lines.len().checked_sub(1) {
            self.scroll.scroll_to_item(last, ScrollStrategy::Bottom);
        }
    }

    pub fn toggle_pause(&mut self) {
        self.paused = !self.paused;
        if !self.paused {
            self.scroll_to_bottom();
        }
    }

    /// Apply one incremental tail response; returns `true` only when visible state changed
    /// so callers skip `cx.notify()` on idle empty polls.
    pub fn apply_chunk(&mut self, chunk: &str, reset: bool, total_bytes: u64) -> bool {
        let mut changed = self.total_bytes != total_bytes;
        self.total_bytes = total_bytes;
        if reset {
            self.text.clear();
            self.lines = Rc::new(Vec::new());
            self.reset = true;
            changed = true;
        }
        if !chunk.is_empty() {
            append_bounded_text(&mut self.text, chunk);
            self.lines = Rc::new(self.text.lines().map(|line| SharedString::from(line.to_owned())).collect());
            if !self.paused {
                self.scroll_to_bottom();
            }
            changed = true;
        }
        changed
    }

    /// Insert the last `QUOTE_LINES` lines into the chat composer as a fenced block quote.
    pub fn quote_to_chat(&self, window: &mut Window, cx: &mut App) {
        let trimmed = self.text.trim();
        if trimmed.is_empty() {
            return;
        }
        let lines: Vec<&str> = trimmed.lines().collect();
        let slice = if lines.len() > QUOTE_LINES { &lines[lines.len() - QUOTE_LINES..] } else { &lines[..] };
        let mut quoted = String::from("> ```text\n");
        for line in slice {
            quoted.push_str("> ");
            quoted.push_str(line);
            quoted.push('\n');
        }
        quoted.push_str("> ```");
        crate::ui::insert_into_composer(quoted, window, cx);
    }

    /// Render the virtualized log body with a vertical scrollbar.
    pub fn render_body(&self, id: &'static str, font_size: Option<Pixels>, pad: Pixels, cx: &App) -> AnyElement {
        let theme = cx.theme().clone();
        if self.lines.is_empty() {
            return div()
                .flex_1()
                .min_h_0()
                .p(pad)
                .font_family(theme.mono_font_family.clone())
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(tr(cx, "experiments.tailEmpty"))
                .into_any_element();
        }
        let lines = Rc::clone(&self.lines);
        let mono = theme.mono_font_family.clone();
        let list = uniform_list(id, lines.len(), move |range, _, _| {
            range
                .map(|idx| {
                    let mut row = div()
                        .h(px(LOG_ROW_HEIGHT))
                        .px(pad)
                        .whitespace_nowrap()
                        .font_family(mono.clone())
                        .child(lines[idx].clone());
                    if let Some(size) = font_size {
                        row = row.text_size(size);
                    } else {
                        row = row.text_xs();
                    }
                    row.into_any_element()
                })
                .collect::<Vec<_>>()
        })
        .size_full()
        .py(pad * 0.5)
        .track_scroll(&self.scroll);

        div()
            .relative()
            .flex_1()
            .min_h_0()
            .overflow_hidden()
            .child(list)
            .vertical_scrollbar(&self.scroll)
            .into_any_element()
    }
}

/// Append incoming log text while capping memory to `MAX_TAIL_LINES` lines and `MAX_TAIL_BYTES` bytes.
pub(crate) fn append_bounded_text(buf: &mut String, chunk: &str) {
    buf.push_str(chunk);
    if buf.len() > MAX_TAIL_BYTES {
        let mut cut = buf.len().saturating_sub(MAX_TAIL_BYTES);
        while cut < buf.len() && !buf.is_char_boundary(cut) {
            cut += 1;
        }
        if let Some(nl) = buf[cut..].find('\n') {
            cut += nl + 1;
        }
        buf.drain(..cut);
    }
    let line_count = buf.bytes().filter(|b| *b == b'\n').count();
    if line_count > MAX_TAIL_LINES {
        let drop_lines = line_count - MAX_TAIL_LINES;
        let mut seen = 0usize;
        let mut cut = 0usize;
        for (idx, b) in buf.bytes().enumerate() {
            if b == b'\n' {
                seen += 1;
                if seen == drop_lines {
                    cut = idx + 1;
                    break;
                }
            }
        }
        if cut > 0 && cut <= buf.len() {
            buf.drain(..cut);
        }
    }
}

#[cfg(test)]
mod tests {
    use ::core::prelude::v1::test;

    use super::*;

    #[test]
    fn apply_chunk_skips_notify_on_idle_empty_poll() {
        let mut buf = LogTailBuffer::new();
        assert!(!buf.apply_chunk("", false, 0));
        assert!(buf.apply_chunk("line 1\n", false, 7));
        assert!(!buf.apply_chunk("", false, 7));
        assert!(buf.apply_chunk("", true, 0));
        assert!(buf.reset);
    }
}
