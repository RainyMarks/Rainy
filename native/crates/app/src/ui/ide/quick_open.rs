//! Open a project file by name (`QuickOpenDialog.tsx`, Ctrl+P).

use std::time::Duration;

use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use rainy_protocol::IdeFileSearch;
use serde_json::json;

use crate::store::Store;
use crate::store::ide::{Ide, file_key};
use crate::ui::ide::file_icon;
use crate::ui::tr;

pub struct QuickOpen {
    ide: Entity<Ide>,
    query: Entity<InputState>,
    /// Matching file keys, best first.
    results: Vec<String>,
    truncated: bool,
    selected: usize,
    generation: u64,
    debounce: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl QuickOpen {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let query = cx.new(|cx| InputState::new(window, cx).placeholder(tr(cx, "ide.ideSearchPlaceholder")));
        let subscriptions = vec![cx.subscribe_in(&query, window, |this, _, event: &InputEvent, window, cx| if let InputEvent::Change = event {
            this.debounce = Some(cx.spawn_in(window, async move |this, cx| {
                cx.background_executor().timer(Duration::from_millis(120)).await;
                let _ = this.update(cx, |this, cx| this.search(cx));
            }));
        })];
        let mut view = Self {
            ide: Ide::global(cx),
            query,
            results: Vec::new(),
            truncated: false,
            selected: 0,
            generation: 0,
            debounce: None,
            _subscriptions: subscriptions,
        };
        view.search(cx);
        view
    }

    /// Search every folder of the project (`files.search`).
    fn search(&mut self, cx: &mut Context<Self>) {
        let text = self.query.read(cx).value().trim().to_owned();
        let Some(workspace) = self.ide.read(cx).workspace.clone() else { return };
        self.generation += 1;
        let generation = self.generation;
        let store = Store::global(cx);
        let calls: Vec<_> = workspace
            .roots()
            .into_iter()
            .map(|root| {
                let mut params = json!({ "op": "files.search", "workspaceId": workspace.workspace_id, "query": text, "limit": 60 });
                if !root.primary {
                    params["rootId"] = json!(root.root_id);
                }
                (root, store.read(cx).call("ide", params))
            })
            .collect();
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let mut results = Vec::new();
            let mut truncated = false;
            for (root, call) in calls {
                if let Ok(Ok(found)) = call.await.map(serde_json::from_value::<IdeFileSearch>) {
                    truncated |= found.truncated;
                    results.extend(found.paths.iter().map(|path| file_key(path, Some(&root))));
                }
            }
            let _ = this.update(cx, |this, cx| {
                if this.generation == generation {
                    this.results = results;
                    this.truncated = truncated;
                    this.selected = 0;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn open(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = self.results.get(index).cloned() else { return };
        window.close_dialog(cx);
        self.ide.update(cx, |ide, cx| ide.open(key, true, window, cx));
    }

    fn selected_key(&self) -> Option<String> {
        self.results.get(self.selected).cloned()
    }

    fn key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let count = self.results.len();
        if count == 0 {
            return;
        }
        match event.keystroke.key.as_str() {
            "down" => self.selected = (self.selected + 1) % count,
            "up" => self.selected = (self.selected + count - 1) % count,
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }
}

impl Render for QuickOpen {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let selected = self.selected;
        let rows: Vec<AnyElement> = self
            .results
            .iter()
            .enumerate()
            .map(|(index, key)| {
                let (_, path) = crate::store::ide::split_key(key);
                let (name, directory) = match path.rsplit_once('/') {
                    Some((directory, name)) => (name.to_owned(), directory.to_owned()),
                    None => (path.to_owned(), String::new()),
                };
                h_flex()
                    .id(("quick-open", index))
                    .h(px(30.))
                    .px_2()
                    .gap_2()
                    .items_center()
                    .flex_shrink_0()
                    .rounded(theme.radius)
                    .text_sm()
                    .cursor_pointer()
                    .when(index == selected, |this| this.bg(theme.list_active))
                    .hover(|style| style.bg(theme.list_hover))
                    .child(Icon::new(file_icon(&name)).small())
                    .child(div().flex_shrink_0().child(name))
                    .child(div().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().text_xs().text_color(theme.muted_foreground).child(directory))
                    .on_click(cx.listener(move |this, _, window, cx| this.open(index, window, cx)))
                    .into_any_element()
            })
            .collect();
        let empty = rows.is_empty();
        v_flex()
            .gap_2()
            .capture_key_down(cx.listener(Self::key_down))
            .child(Input::new(&self.query))
            .child(
                v_flex()
                    .id("quick-open-results")
                    .max_h(px(420.))
                    .overflow_y_scrollbar()
                    .children(rows)
                    .when(empty, |this| this.child(div().p_2().text_sm().text_color(theme.muted_foreground).child(tr(cx, "ide.ideSearchEmpty")))),
            )
            .when(self.truncated, |this| this.child(div().text_xs().text_color(theme.muted_foreground).child(tr(cx, "ide.ideSearchTruncated"))))
    }
}

/// Show the dialog with the cursor in its field.
pub fn open(window: &mut Window, cx: &mut App) {
    if Ide::global(cx).read(cx).workspace.is_none() {
        return;
    }
    let view = cx.new(|cx| QuickOpen::new(window, cx));
    let query = view.read(cx).query.clone();
    // Enter is the dialog's Confirm: it opens the highlighted file and closes the dialog.
    window.open_dialog(cx, move |dialog, _, cx| {
        let chosen = view.clone();
        dialog.title(tr(cx, "ide.ideQuickOpen")).w(px(620.)).child(view.clone()).on_ok(move |_, window, cx| {
            if let Some(key) = chosen.read(cx).selected_key() {
                Ide::global(cx).update(cx, |ide, cx| ide.open(key, true, window, cx));
            }
            true
        })
    });
    window.defer(cx, move |window, cx| {
        window.defer(cx, move |window, cx| query.update(cx, |state, cx| state.focus(window, cx)));
    });
}
