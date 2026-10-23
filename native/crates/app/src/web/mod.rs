//! Web pages inside the workbench, in WebView2: the 3.x viewers the Host serves for PDF, Office documents, media and
//! Markdown, and the agent's preview of the project's own server.
//!
//! A WebView2 control is a child window on top of everything GPUI draws, so a pane is shown only in frames where its
//! area was painted and no dialog covers the window.

pub mod export;
pub mod office;
pub mod pdf_text;
pub mod preview;
pub mod webview;

use std::collections::{HashMap, HashSet};

use gpui_kit::component::WindowExt as _;
use gpui_kit::*;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

pub use webview::{Options, Shortcut, WebView};

/// What a pane reports to its owner.
#[derive(Debug, Clone, PartialEq)]
pub enum PaneEvent {
    /// The control exists and shows its first page.
    Ready,
    Failed(SharedString),
    /// A string the page posted with `window.ipc.postMessage`.
    Message(String),
    /// A shortcut of the workbench pressed while the page had the keyboard.
    Shortcut(Shortcut),
    /// The page asked for a new window with this address.
    NewWindow(String),
}

enum State {
    Creating,
    Ready(WebView),
    Failed(SharedString),
}

/// One page shown in an area of the window.
pub struct WebPane {
    state: State,
    /// Where it was painted last, in logical pixels of the window.
    bounds: Option<Bounds<Pixels>>,
    shown: bool,
    _forward: Task<()>,
}

impl EventEmitter<PaneEvent> for WebPane {}

/// The panes of the window, and which of them were painted in the current frame.
#[derive(Default)]
struct Panes {
    all: Vec<WeakEntity<WebPane>>,
    painted: HashSet<EntityId>,
    /// What had the keyboard at the last frame of each window; a window is missing until it drew one.
    focus: HashMap<WindowId, Option<WeakFocusHandle>>,
    /// Windows with a menu open, whose focus is looked at every frame.
    following: HashSet<WindowId>,
}

impl Global for Panes {}

/// The window's Win32 handle.
pub fn hwnd(window: &Window) -> Option<windows::Win32::Foundation::HWND> {
    match HasWindowHandle::window_handle(window).ok()?.as_raw() {
        RawWindowHandle::Win32(handle) => Some(windows::Win32::Foundation::HWND(handle.hwnd.get() as *mut _)),
        _ => None,
    }
}

impl WebPane {
    /// A pane that opens `url`; `options` decide the profile, scripts and which addresses it may visit.
    pub fn new(url: String, mut options: Options, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (events, received) = async_channel::unbounded::<PaneEvent>();
        if let Some(mut on_message) = options.on_message.take() {
            let events = events.clone();
            options.on_message = Some(Box::new(move |text| {
                on_message(text.clone());
                let _ = events.try_send(PaneEvent::Message(text));
            }));
        } else {
            let events = events.clone();
            options.on_message = Some(Box::new(move |text| drop(events.try_send(PaneEvent::Message(text)))));
        }
        let shortcuts = events.clone();
        let mut filter = options.on_shortcut.take();
        options.on_shortcut = Some(Box::new(move |shortcut| {
            let take = filter.as_mut().is_some_and(|filter| filter(shortcut));
            if take {
                let _ = shortcuts.try_send(PaneEvent::Shortcut(shortcut));
            }
            take
        }));
        let windows = events.clone();
        options.on_new_window = Some(Box::new(move |url| drop(windows.try_send(PaneEvent::NewWindow(url)))));
        let entity = cx.entity().downgrade();
        match hwnd(window) {
            Some(parent) => WebView::create(parent, options, {
                let events = events.clone();
                move |result| {
                    let _ = events.try_send(match result {
                        Ok(view) => {
                            view.set_visible(false);
                            view.navigate(&url);
                            CREATED.with(|created| created.borrow_mut().push((entity, view)));
                            PaneEvent::Ready
                        }
                        Err(error) => PaneEvent::Failed(error.into()),
                    });
                }
            }),
            None => drop(events.try_send(PaneEvent::Failed("No window handle for the page.".into()))),
        }
        // Events come from WebView2 callbacks, outside any GPUI update; they are applied here, in order.
        let forward = cx.spawn(async move |this: WeakEntity<WebPane>, cx: &mut AsyncApp| {
            while let Ok(event) = received.recv().await {
                let applied = this.update(cx, |pane, cx| {
                    match &event {
                        PaneEvent::Ready => {
                            if let Some(view) = CREATED.with(|created| {
                                let mut created = created.borrow_mut();
                                let at = created.iter().position(|(owner, _)| owner.entity_id() == cx.entity_id())?;
                                Some(created.remove(at).1)
                            }) {
                                pane.state = State::Ready(view);
                                pane.place(false);
                            }
                        }
                        PaneEvent::Failed(error) => pane.state = State::Failed(error.clone()),
                        _ => {}
                    }
                    cx.emit(event);
                    cx.notify();
                });
                if applied.is_err() {
                    break;
                }
            }
        });
        let weak = cx.entity().downgrade();
        if !cx.has_global::<Panes>() {
            cx.set_global(Panes::default());
        }
        cx.update_global::<Panes, _>(|panes, _| panes.all.push(weak));
        Self { state: State::Creating, bounds: None, shown: false, _forward: forward }
    }

    pub fn view(&self) -> Option<&WebView> {
        match &self.state {
            State::Ready(view) => Some(view),
            _ => None,
        }
    }

    pub fn error(&self) -> Option<SharedString> {
        match &self.state {
            State::Failed(error) => Some(error.clone()),
            _ => None,
        }
    }

    pub fn is_creating(&self) -> bool {
        matches!(self.state, State::Creating)
    }

    /// Show or hide the control where it was last painted.
    fn place(&mut self, shown: bool) {
        self.shown = shown;
        let Some(view) = self.view() else { return };
        if let (true, Some(bounds)) = (shown, self.bounds) {
            let scale = SCALE.with(|scale| *scale.borrow());
            let px = |value: Pixels| (f32::from(value) * scale).round() as i32;
            view.set_bounds(px(bounds.origin.x), px(bounds.origin.y), px(bounds.size.width), px(bounds.size.height));
        }
        view.set_visible(shown && self.bounds.is_some());
    }

    /// The area the page covers; place it where this element is laid out.
    pub fn area(pane: &Entity<WebPane>) -> impl IntoElement {
        let pane = pane.clone();
        canvas(
            move |bounds, window, cx| {
                SCALE.with(|scale| *scale.borrow_mut() = window.scale_factor());
                let id = pane.entity_id();
                cx.update_global::<Panes, _>(|panes, _| {
                    panes.painted.insert(id);
                });
                pane.update(cx, |pane, _| {
                    if pane.bounds != Some(bounds) {
                        pane.bounds = Some(bounds);
                        if pane.shown {
                            pane.place(true);
                        }
                    }
                });
            },
            |_, _, _, _| {},
        )
        .size_full()
    }
}

impl Drop for WebPane {
    fn drop(&mut self) {
        if let State::Ready(view) = &self.state {
            view.close();
        }
    }
}

thread_local! {
    /// Controls created but not yet handed to their pane.
    static CREATED: std::cell::RefCell<Vec<(WeakEntity<WebPane>, WebView)>> = std::cell::RefCell::default();
    static SCALE: std::cell::RefCell<f32> = const { std::cell::RefCell::new(1.0) };
}

/// Start a frame: no pane has been painted yet. Called by the workbench before its children.
pub fn begin_frame(cx: &mut App) {
    if !cx.has_global::<Panes>() {
        cx.set_global(Panes::default());
    }
    cx.update_global::<Panes, _>(|panes, _| panes.painted.clear());
}

/// End a frame: show the panes painted in it, unless a dialog, a sheet or the caller's own overlay (`covered`) is over
/// the window, and hide the others.
pub fn end_frame(covered: bool, window: &mut Window, cx: &mut App) {
    // A click anywhere GPUI draws takes the keyboard back from a page; clicks inside a page never reach GPUI.
    window.on_mouse_event(|_: &MouseDownEvent, phase, window, _| {
        if phase == DispatchPhase::Capture {
            reclaim_focus(window);
        }
    });
    // Menus are drawn inside the window, under the pages' own windows; an open menu holds the keyboard, so while the
    // focus is in one the pages step aside.
    let id = Window::window_handle(window).window_id();
    let focused = window.focused(cx).map(|handle| handle.downgrade());
    let before = cx.update_global::<Panes, _>(|panes, _| panes.focus.insert(id, focused.clone()));
    // GPUI looks the focus up in the frame drawn before, which lacks a menu opened in this one; once the focus moved,
    // one more frame settles it.
    if before.as_ref().is_some_and(|before| *before != focused) {
        window.on_next_frame(|window, _| window.refresh());
    }
    let menu_open = before.is_some() && focused.is_some() && window.context_stack().iter().any(|context| context.contains("PopupMenu"));
    if menu_open && cx.update_global::<Panes, _>(|panes, _| panes.following.insert(id)) {
        follow_menu(id, focused.clone(), window);
    }
    let covered = covered || menu_open || window.has_active_dialog(cx) || window.has_active_sheet(cx);
    let (all, painted) = cx.update_global::<Panes, _>(|panes, _| {
        panes.all.retain(|pane| pane.upgrade().is_some());
        (panes.all.clone(), panes.painted.clone())
    });
    for pane in all.iter().filter_map(WeakEntity::upgrade) {
        let shown = !covered && painted.contains(&pane.entity_id());
        pane.update(cx, |pane, _| {
            if pane.shown != shown || shown {
                pane.place(shown);
            }
        });
    }
}

/// Look at the focus every frame while a menu is open: a menu that closes may hand the focus back while a frame is
/// drawn, which asks for no new frame, and the pages would stay hidden until something else redraws.
fn follow_menu(id: WindowId, seen: Option<WeakFocusHandle>, window: &mut Window) {
    window.on_next_frame(move |window, cx| {
        if window.focused(cx).map(|handle| handle.downgrade()) == seen {
            follow_menu(id, seen, window);
        } else {
            cx.update_global::<Panes, _>(|panes, _| panes.following.remove(&id));
            window.refresh();
        }
    });
}

/// The background a pane shows before its page paints: the theme's.
pub fn background(color: Hsla) -> (u8, u8, u8) {
    let rgba = Rgba::from(color);
    let channel = |value: f32| (value * 255.0).round() as u8;
    (channel(rgba.r), channel(rgba.g), channel(rgba.b))
}

/// Give the keyboard back to the GPUI window when a page has it.
pub fn reclaim_focus(window: &Window) {
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetFocus, SetFocus};
    let Some(hwnd) = hwnd(window) else { return };
    // SAFETY: plain focus calls on the UI thread with the live GPUI window.
    unsafe {
        if GetFocus() != hwnd {
            let _ = SetFocus(Some(hwnd));
        }
    }
}
