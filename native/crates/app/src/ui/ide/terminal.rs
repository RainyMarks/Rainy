//! One terminal of the project: an `alacritty_terminal` grid fed with the Host's terminal output, drawn cell by
//! cell, with keyboard, IME, selection, copy and paste.

use std::ops::Range;
use std::sync::{Arc, Mutex};

use alacritty_terminal::event::{Event as TermEvent, EventListener};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point as GridPoint, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::vte::ansi::{Color as AnsiColor, NamedColor, Processor, Rgb};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::store::execution::{Execution, ExecutionEvent};

// Shortcuts the window binds that a shell needs as control characters.
actions!(terminal, [SendCtrlS, SendCtrlW, SendCtrlO]);

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("ctrl-s", SendCtrlS, Some("Terminal")),
        KeyBinding::new("ctrl-w", SendCtrlW, Some("Terminal")),
        KeyBinding::new("ctrl-o", SendCtrlO, Some("Terminal")),
    ]);
}

/// Responses the terminal writes back, such as cursor position reports.
#[derive(Clone, Default)]
struct Replies(Arc<Mutex<Vec<String>>>);

impl EventListener for Replies {
    fn send_event(&self, event: TermEvent) {
        if let TermEvent::PtyWrite(text) = event {
            self.0.lock().unwrap().push(text);
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct GridSize {
    cols: usize,
    rows: usize,
}

impl Dimensions for GridSize {
    fn total_lines(&self) -> usize {
        self.rows
    }

    fn screen_lines(&self) -> usize {
        self.rows
    }

    fn columns(&self) -> usize {
        self.cols
    }
}

/// The 16 ANSI colors for a light and a dark background.
const DARK: [u32; 16] = [
    0x000000, 0xcd3131, 0x0dbc79, 0xe5e510, 0x2472c8, 0xbc3fbc, 0x11a8cd, 0xe5e5e5, 0x666666, 0xf14c4c, 0x23d18b, 0xf5f543,
    0x3b8eea, 0xd670d6, 0x29b8db, 0xe5e5e5,
];
const LIGHT: [u32; 16] = [
    0x000000, 0xcd3131, 0x00bc00, 0x949800, 0x0451a5, 0xbc05bc, 0x0598bc, 0x555555, 0x666666, 0xcd3131, 0x14ce14, 0xb5ba00,
    0x0451a5, 0xbc05bc, 0x0598bc, 0xa5a5a5,
];

fn rgb_hsla(color: Rgb) -> Hsla {
    Rgba { r: color.r as f32 / 255., g: color.g as f32 / 255., b: color.b as f32 / 255., a: 1. }.into()
}

/// The xterm 256-color palette beyond the first 16.
fn indexed(index: u8) -> Hsla {
    let index = index as u32;
    if index >= 232 {
        let level = 8 + 10 * (index - 232);
        return rgb((level << 16) | (level << 8) | level).into();
    }
    let cube = index - 16;
    let level = |value: u32| if value == 0 { 0 } else { 55 + 40 * value };
    rgb((level(cube / 36) << 16) | (level((cube / 6) % 6) << 8) | level(cube % 6)).into()
}

struct Palette {
    ansi: [Hsla; 16],
    foreground: Hsla,
    background: Hsla,
    cursor: Hsla,
    selection: Hsla,
}

impl Palette {
    fn new(cx: &App) -> Self {
        let theme = cx.theme();
        let table = if theme.is_dark() { DARK } else { LIGHT };
        Self {
            ansi: table.map(|value| rgb(value).into()),
            foreground: theme.foreground,
            background: theme.background,
            cursor: theme.primary,
            selection: theme.selection,
        }
    }

    fn color(&self, color: AnsiColor, overrides: &alacritty_terminal::term::color::Colors) -> Hsla {
        match color {
            AnsiColor::Spec(value) => rgb_hsla(value),
            AnsiColor::Indexed(index) => match overrides[index as usize] {
                Some(value) => rgb_hsla(value),
                None if index < 16 => self.ansi[index as usize],
                None => indexed(index),
            },
            AnsiColor::Named(name) => {
                if let Some(value) = overrides[name as usize] {
                    return rgb_hsla(value);
                }
                match name {
                    NamedColor::Foreground | NamedColor::BrightForeground => self.foreground,
                    NamedColor::DimForeground => self.foreground.opacity(0.7),
                    NamedColor::Background => self.background,
                    NamedColor::Cursor => self.cursor,
                    other => {
                        let index = other as usize;
                        if index < 16 {
                            self.ansi[index]
                        } else {
                            // Dim colors follow the normal ones.
                            let base = (index - NamedColor::DimBlack as usize).min(7);
                            self.ansi[base].opacity(0.7)
                        }
                    }
                }
            }
        }
    }
}

/// One run of cells drawn with the same style.
struct Run {
    column: usize,
    cells: usize,
    text: String,
    foreground: Hsla,
    background: Option<Hsla>,
    bold: bool,
    italic: bool,
    underline: bool,
    strike: bool,
}

pub struct TerminalView {
    pub id: String,
    term: Term<Replies>,
    processor: Processor,
    replies: Replies,
    size: GridSize,
    /// Cell width and line height, measured at render.
    cell: Size<Pixels>,
    bounds: Bounds<Pixels>,
    /// Text being composed by an input method, shown at the cursor.
    marked: Option<String>,
    selecting: bool,
    scroll_remainder: f32,
    focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl TerminalView {
    pub fn new(id: String, cx: &mut Context<Self>) -> Self {
        let replies = Replies::default();
        let size = GridSize { cols: 100, rows: 24 };
        let config = Config { scrolling_history: 5000, ..Config::default() };
        let term = Term::new(config, &size, replies.clone());
        let watched = id.clone();
        let subscriptions = vec![cx.subscribe(&Execution::global(cx), move |this: &mut Self, _, event: &ExecutionEvent, cx| {
            if let ExecutionEvent::Output { operation, stream, text } = event
                && *operation == watched
            {
                match stream.as_str() {
                    "terminal" => this.feed(text, cx),
                    // A run without a pseudo-terminal writes plain lines; the grid needs carriage returns too.
                    "stdout" | "stderr" => this.feed(&text.replace("\r\n", "\n").replace('\n', "\r\n"), cx),
                    _ => {}
                }
            }
        })];
        Self {
            id,
            term,
            processor: Processor::new(),
            replies,
            size,
            cell: gpui_kit::size(px(8.), px(18.)),
            bounds: Bounds::default(),
            marked: None,
            selecting: false,
            scroll_remainder: 0.,
            focus: cx.focus_handle(),
            _subscriptions: subscriptions,
        }
    }

    fn feed(&mut self, text: &str, cx: &mut Context<Self>) {
        self.processor.advance(&mut self.term, text.as_bytes());
        let replies: Vec<String> = std::mem::take(&mut *self.replies.0.lock().unwrap());
        for reply in replies {
            self.write(&reply, cx);
        }
        cx.notify();
    }

    fn write(&self, data: &str, cx: &App) {
        Execution::global(cx).read(cx).input(&self.id, data, cx);
    }

    /// Typed input goes to the shell, and the view returns to the live screen.
    fn send(&mut self, data: &str, cx: &mut Context<Self>) {
        self.term.scroll_display(Scroll::Bottom);
        self.term.selection = None;
        self.write(data, cx);
        cx.notify();
    }

    /// Follow the element's size: the grid gets as many cells as fit.
    fn fit(&mut self, bounds: Bounds<Pixels>, cx: &mut Context<Self>) {
        self.bounds = bounds;
        let cols = ((bounds.size.width / self.cell.width).floor() as usize).max(2);
        let rows = ((bounds.size.height / self.cell.height).floor() as usize).max(1);
        let size = GridSize { cols, rows };
        if size != self.size {
            self.size = size;
            self.term.resize(size);
            Execution::global(cx).read(cx).resize(&self.id, cols as u16, rows as u16, cx);
            cx.notify();
        }
    }

    /// The last lines of the screen and its history, as text (3.x `recentTerminalText`).
    pub fn recent_text(&self, lines: usize) -> String {
        let grid = self.term.grid();
        let bottom = grid.bottommost_line();
        let top = Line((bottom.0 - lines as i32 + 1).max(grid.topmost_line().0));
        let text = self.term.bounds_to_string(GridPoint::new(top, Column(0)), GridPoint::new(bottom, grid.last_column()));
        text.lines().map(str::trim_end).collect::<Vec<_>>().join("\n").trim().to_owned()
    }

    fn copy(&mut self, cx: &mut Context<Self>) -> bool {
        match self.term.selection_to_string().filter(|text| !text.is_empty()) {
            Some(text) => {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
                self.term.selection = None;
                cx.notify();
                true
            }
            None => false,
        }
    }

    fn paste(&mut self, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else { return };
        let text = text.replace("\r\n", "\r").replace('\n', "\r");
        if self.term.mode().contains(TermMode::BRACKETED_PASTE) {
            self.send(&format!("\x1b[200~{}\x1b[201~", text.replace('\x1b', "")), cx);
        } else {
            self.send(&text, cx);
        }
    }

    fn key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        let modifiers = keystroke.modifiers;
        let key = keystroke.key.as_str();
        if modifiers.control && (key == "v" || (modifiers.shift && key == "v")) {
            self.paste(cx);
            cx.stop_propagation();
            return;
        }
        if modifiers.control && key == "c" && (modifiers.shift || self.term.selection.as_ref().is_some_and(|s| !s.is_empty())) {
            self.copy(cx);
            cx.stop_propagation();
            return;
        }
        if let Some(data) = escape(keystroke, self.term.mode().contains(TermMode::APP_CURSOR)) {
            self.send(&data, cx);
            cx.stop_propagation();
        }
    }

    /// The grid point under a window position.
    fn point_at(&self, position: Point<Pixels>) -> (GridPoint, Side) {
        let x = (position.x - self.bounds.origin.x).max(px(0.));
        let y = (position.y - self.bounds.origin.y).max(px(0.));
        let column = ((x / self.cell.width) as usize).min(self.size.cols.saturating_sub(1));
        let row = ((y / self.cell.height) as usize).min(self.size.rows.saturating_sub(1));
        let half = (x / self.cell.width).fract() >= 0.5;
        let offset = self.term.grid().display_offset() as i32;
        (GridPoint::new(Line(row as i32 - offset), Column(column)), if half { Side::Right } else { Side::Left })
    }

    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.focus.focus(window, cx);
        let (point, side) = self.point_at(event.position);
        let kind = if event.click_count >= 2 { SelectionType::Semantic } else { SelectionType::Simple };
        self.term.selection = Some(Selection::new(kind, point, side));
        self.selecting = true;
        cx.notify();
    }

    fn mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selecting || event.pressed_button != Some(MouseButton::Left) {
            return;
        }
        let (point, side) = self.point_at(event.position);
        if let Some(selection) = &mut self.term.selection {
            selection.update(point, side);
            cx.notify();
        }
    }

    fn mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.selecting = false;
    }

    fn scroll(&mut self, event: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        let lines = event.delta.pixel_delta(self.cell.height).y / self.cell.height + self.scroll_remainder;
        let whole = lines.trunc();
        self.scroll_remainder = lines - whole;
        if whole == 0. {
            return;
        }
        if self.term.mode().contains(TermMode::ALT_SCREEN) {
            // Full-screen programs such as less get arrow keys instead.
            let key = if whole > 0. { "\x1b[A" } else { "\x1b[B" };
            self.write(&key.repeat(whole.abs() as usize), cx);
        } else {
            self.term.scroll_display(Scroll::Delta(whole as i32));
            cx.notify();
        }
    }

    fn runs(&self, palette: &Palette) -> (Vec<Vec<Run>>, Option<(usize, usize, bool)>) {
        let content = self.term.renderable_content();
        let offset = content.display_offset as i32;
        let mut rows: Vec<Vec<Run>> = (0..self.size.rows).map(|_| Vec::new()).collect();
        for indexed in content.display_iter {
            let row = indexed.point.line.0 + offset;
            if row < 0 || row as usize >= rows.len() {
                continue;
            }
            let cell = &indexed.cell;
            if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                continue;
            }
            let mut foreground = palette.color(cell.fg, content.colors);
            let mut background = match cell.bg {
                AnsiColor::Named(NamedColor::Background) => None,
                other => Some(palette.color(other, content.colors)),
            };
            if cell.flags.contains(Flags::INVERSE) {
                let back = background.unwrap_or(palette.background);
                background = Some(foreground);
                foreground = back;
            }
            if cell.flags.contains(Flags::DIM) {
                foreground = foreground.opacity(0.7);
            }
            if content.selection.as_ref().is_some_and(|selection| selection.contains(indexed.point)) {
                background = Some(palette.selection);
            }
            let wide = cell.flags.contains(Flags::WIDE_CHAR);
            let character = if cell.flags.contains(Flags::HIDDEN) { ' ' } else { cell.c };
            let (bold, italic) = (cell.flags.contains(Flags::BOLD), cell.flags.contains(Flags::ITALIC));
            let (underline, strike) = (cell.flags.intersects(Flags::ALL_UNDERLINES), cell.flags.contains(Flags::STRIKEOUT));
            let column = indexed.point.column.0;
            let line = &mut rows[row as usize];
            // A wide character gets its own run, placed on its two cells; runs of narrow cells merge.
            let merge = !wide
                && line.last().is_some_and(|run: &Run| {
                    run.column + run.cells == column
                        && run.foreground == foreground
                        && run.background == background
                        && run.bold == bold
                        && run.italic == italic
                        && run.underline == underline
                        && run.strike == strike
                        && !run.text.chars().any(|c| unicode_wide(c))
                });
            if merge {
                let run = line.last_mut().unwrap();
                run.text.push(character);
                run.cells += 1;
            } else {
                line.push(Run {
                    column,
                    cells: if wide { 2 } else { 1 },
                    text: character.to_string(),
                    foreground,
                    background,
                    bold,
                    italic,
                    underline,
                    strike,
                });
            }
        }
        // Trailing blank cells draw nothing.
        for line in &mut rows {
            while line.last().is_some_and(|run| run.background.is_none() && run.text.trim().is_empty() && !run.underline) {
                line.pop();
            }
        }
        let cursor = content.cursor.point;
        let row = cursor.line.0 + offset;
        let visible = content.mode.contains(TermMode::SHOW_CURSOR) && row >= 0 && (row as usize) < self.size.rows;
        let wide = self.term.grid()[cursor].flags.contains(Flags::WIDE_CHAR);
        (rows, visible.then_some((row as usize, cursor.column.0, wide)))
    }
}

/// Characters that take two cells and so are drawn alone.
fn unicode_wide(c: char) -> bool {
    (c as u32) >= 0x1100
}

/// The bytes a key sends, for keys that are not plain text; typed text arrives through the input handler.
fn escape(keystroke: &Keystroke, app_cursor: bool) -> Option<String> {
    let modifiers = keystroke.modifiers;
    let key = keystroke.key.as_str();
    let cursor = |code: char| if app_cursor && !modifiers.control && !modifiers.shift { format!("\x1bO{code}") } else { modifier_csi(code, modifiers) };
    let tilde = |number: u8| {
        let modifier = modifier_code(modifiers);
        if modifier > 1 { format!("\x1b[{number};{modifier}~") } else { format!("\x1b[{number}~") }
    };
    let sequence = match key {
        "enter" => (if modifiers.alt { "\x1b\r" } else { "\r" }).to_owned(),
        "backspace" => (if modifiers.control { "\x08" } else if modifiers.alt { "\x1b\x7f" } else { "\x7f" }).to_owned(),
        "tab" => (if modifiers.shift { "\x1b[Z" } else { "\t" }).to_owned(),
        "escape" => "\x1b".to_owned(),
        "up" => cursor('A'),
        "down" => cursor('B'),
        "right" => cursor('C'),
        "left" => cursor('D'),
        "home" => cursor('H'),
        "end" => cursor('F'),
        "insert" => tilde(2),
        "delete" => tilde(3),
        "pageup" => tilde(5),
        "pagedown" => tilde(6),
        "f1" => "\x1bOP".to_owned(),
        "f2" => "\x1bOQ".to_owned(),
        "f3" => "\x1bOR".to_owned(),
        "f4" => "\x1bOS".to_owned(),
        "f5" => tilde(15),
        "f6" => tilde(17),
        "f7" => tilde(18),
        "f8" => tilde(19),
        "f9" => tilde(20),
        "f10" => tilde(21),
        "f11" => tilde(23),
        "f12" => tilde(24),
        _ if modifiers.control && !modifiers.alt => {
            let mut chars = key.chars();
            let (Some(c), None) = (chars.next(), chars.next()) else { return None };
            match c {
                'a'..='z' => ((c as u8 - b'a' + 1) as char).to_string(),
                '@' | ' ' | '2' => "\0".to_owned(),
                '[' | '3' => "\x1b".to_owned(),
                '\\' | '4' => "\x1c".to_owned(),
                ']' | '5' => "\x1d".to_owned(),
                '^' | '6' => "\x1e".to_owned(),
                '_' | '-' | '7' => "\x1f".to_owned(),
                '/' => "\x1f".to_owned(),
                _ => return None,
            }
        }
        _ if modifiers.alt && !modifiers.control => {
            let text = keystroke.key_char.clone().unwrap_or_else(|| key.to_owned());
            if text.chars().count() != 1 {
                return None;
            }
            format!("\x1b{text}")
        }
        _ => return None,
    };
    Some(sequence)
}

fn modifier_code(modifiers: Modifiers) -> u8 {
    1 + u8::from(modifiers.shift) + 2 * u8::from(modifiers.alt) + 4 * u8::from(modifiers.control)
}

fn modifier_csi(code: char, modifiers: Modifiers) -> String {
    let modifier = modifier_code(modifiers);
    if modifier > 1 { format!("\x1b[1;{modifier}{code}") } else { format!("\x1b[{code}") }
}

impl EntityInputHandler for TerminalView {
    fn text_for_range(&mut self, _: Range<usize>, _: &mut Option<Range<usize>>, _: &mut Window, _: &mut Context<Self>) -> Option<String> {
        None
    }

    fn selected_text_range(&mut self, _: bool, _: &mut Window, _: &mut Context<Self>) -> Option<UTF16Selection> {
        Some(UTF16Selection { range: 0..0, reversed: false })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked.as_ref().map(|text| 0..text.encode_utf16().count())
    }

    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.marked = None;
        cx.notify();
    }

    fn replace_text_in_range(&mut self, _: Option<Range<usize>>, text: &str, _: &mut Window, cx: &mut Context<Self>) {
        self.marked = None;
        if !text.is_empty() {
            self.send(text, cx);
        }
    }

    fn replace_and_mark_text_in_range(&mut self, _: Option<Range<usize>>, text: &str, _: Option<Range<usize>>, _: &mut Window, cx: &mut Context<Self>) {
        self.marked = (!text.is_empty()).then(|| text.to_owned());
        cx.notify();
    }

    fn bounds_for_range(&mut self, _: Range<usize>, element: Bounds<Pixels>, _: &mut Window, _: &mut Context<Self>) -> Option<Bounds<Pixels>> {
        let cursor = self.term.grid().cursor.point;
        let row = (cursor.line.0 + self.term.grid().display_offset() as i32).max(0) as f32;
        let origin = point(element.origin.x + self.cell.width * cursor.column.0 as f32, element.origin.y + self.cell.height * row);
        Some(Bounds::new(origin, self.cell))
    }

    fn character_index_for_point(&mut self, _: Point<Pixels>, _: &mut Window, _: &mut Context<Self>) -> Option<usize> {
        None
    }
}

impl Focusable for TerminalView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let palette = Palette::new(cx);
        let family = theme.mono_font_family.clone();
        let font_size = theme.mono_font_size;
        let font_id = window.text_system().resolve_font(&font(family.clone()));
        let width = window.text_system().advance(font_id, font_size, 'm').map(|size| size.width).unwrap_or(font_size * 0.6);
        self.cell = gpui_kit::size(width, (font_size * 1.35).round());
        let cell = self.cell;
        let (rows, cursor) = self.runs(&palette);
        let focused = self.focus.is_focused(window);
        let view = cx.entity();
        let input = cx.entity();
        let focus = self.focus.clone();
        let marked = self.marked.clone();
        div()
            .id(SharedString::from(format!("terminal-{}", self.id)))
            .key_context("Terminal")
            .track_focus(&self.focus)
            .relative()
            .size_full()
            .overflow_hidden()
            .bg(palette.background)
            .text_color(palette.foreground)
            .font_family(family)
            .text_size(font_size)
            .line_height(cell.height)
            .cursor(CursorStyle::IBeam)
            .on_key_down(cx.listener(Self::key_down))
            .on_action(cx.listener(|this, _: &SendCtrlS, _, cx| this.send("\x13", cx)))
            .on_action(cx.listener(|this, _: &SendCtrlW, _, cx| this.send("\x17", cx)))
            .on_action(cx.listener(|this, _: &SendCtrlO, _, cx| this.send("\x0f", cx)))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_move(cx.listener(Self::mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_scroll_wheel(cx.listener(Self::scroll))
            .on_mouse_down(MouseButton::Right, cx.listener(|this, _, window, cx| {
                // Right click copies a selection, or pastes, as Windows consoles do.
                this.focus.focus(window, cx);
                if !this.copy(cx) {
                    this.paste(cx);
                }
            }))
            .child(
                canvas(
                    move |bounds, _, cx| view.update(cx, |view, cx| view.fit(bounds, cx)),
                    move |bounds, _, window, cx| window.handle_input(&focus, ElementInputHandler::new(bounds, input.clone()), cx),
                )
                .absolute()
                .size_full(),
            )
            .children(rows.into_iter().enumerate().map(|(row, runs)| {
                div().absolute().top(cell.height * row as f32).left_0().h(cell.height).w_full().children(runs.into_iter().map(|run| {
                    div()
                        .absolute()
                        .left(cell.width * run.column as f32)
                        .w(cell.width * run.cells as f32)
                        .h(cell.height)
                        .whitespace_nowrap()
                        .overflow_hidden()
                        .text_color(run.foreground)
                        .when_some(run.background, |this, background| this.bg(background))
                        .when(run.bold, |this| this.font_weight(FontWeight::BOLD))
                        .when(run.italic, |this| this.italic())
                        .when(run.underline, |this| this.underline())
                        .when(run.strike, |this| this.line_through())
                        .child(run.text)
                }))
            }))
            .when_some(cursor, |this, (row, column, wide)| {
                let width = if wide { cell.width * 2. } else { cell.width };
                let at = div().absolute().top(cell.height * row as f32).left(cell.width * column as f32).h(cell.height);
                match &marked {
                    Some(text) => this.child(at.bg(palette.background).underline().whitespace_nowrap().child(text.clone())),
                    None if focused => this.child(at.w(width).bg(palette.cursor.opacity(0.6))),
                    None => this.child(at.w(width).border_1().border_color(palette.cursor)),
                }
            })
    }
}
