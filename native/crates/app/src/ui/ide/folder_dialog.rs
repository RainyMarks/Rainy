//! The in-app folder browser (`DirectoryBrowser.tsx`): folders of the Host's machine, for a Linux host whose
//! directories the Windows picker cannot show.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::DialogFooter;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use rainy_protocol::{DirectoryListing, RpcError};
use serde_json::json;

use crate::store::Store;
use crate::store::ide::Ide;
use crate::ui::tr;

pub struct FolderBrowser {
    store: Entity<Store>,
    listing: Option<DirectoryListing>,
    loading: bool,
    error: Option<SharedString>,
    show_hidden: bool,
    path: Entity<InputState>,
    _subscriptions: Vec<Subscription>,
}

impl FolderBrowser {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let path = cx.new(|cx| InputState::new(window, cx));
        let subscriptions = vec![cx.subscribe_in(&path, window, |this, input, event: &InputEvent, window, cx| {
            if let InputEvent::PressEnter { .. } = event {
                let typed = input.read(cx).value().trim().to_owned();
                if !typed.is_empty() {
                    this.load(Some(typed), window, cx);
                }
            }
        })];
        let mut browser = Self {
            store: Store::global(cx),
            listing: None,
            loading: false,
            error: None,
            show_hidden: false,
            path,
            _subscriptions: subscriptions,
        };
        browser.load(None, window, cx);
        browser
    }

    /// Show one directory; none starts at the home directory.
    fn load(&mut self, path: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let mut params = json!({ "op": "directories.list", "showHidden": self.show_hidden });
        if let Some(path) = path {
            params["path"] = json!(path);
        }
        let call = self.store.read(cx).call("ide", params);
        self.loading = true;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = call.await.and_then(|value| {
                serde_json::from_value::<DirectoryListing>(value).map_err(|error| RpcError { code: "invalid".into(), message: error.to_string(), data: None })
            });
            let _ = this.update_in(cx, |this, window, cx| {
                this.loading = false;
                match result {
                    Ok(listing) => {
                        let shown = listing.path.clone();
                        this.path.update(cx, |state, cx| state.set_value(shown, window, cx));
                        this.listing = Some(listing);
                        this.error = None;
                    }
                    Err(error) => this.error = Some(error.message.into()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn current(&self) -> Option<String> {
        self.listing.as_ref().map(|listing| listing.path.clone())
    }
}

impl Render for FolderBrowser {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let parent = self.listing.as_ref().and_then(|listing| listing.parent.clone());
        let roots = self.listing.as_ref().map(|listing| listing.roots.clone()).unwrap_or_default();
        let entries = self.listing.as_ref().map(|listing| listing.entries.clone()).unwrap_or_default();
        let current = self.current();
        let locations = v_flex()
            .w(px(160.))
            .flex_shrink_0()
            .gap_0p5()
            .child(div().px_2().pb_1().text_xs().text_color(theme.muted_foreground).child(tr(cx, "app.browserLocations")))
            .children(roots.into_iter().enumerate().map(|(index, root)| {
                let selected = current.as_deref().is_some_and(|path| path.starts_with(root.as_str()));
                let target = root.clone();
                h_flex()
                    .id(("location", index))
                    .h(px(28.))
                    .px_2()
                    .gap_1p5()
                    .items_center()
                    .rounded(theme.radius)
                    .text_sm()
                    .cursor_pointer()
                    .when(selected, |this| this.bg(theme.list_active))
                    .hover(|style| style.bg(theme.list_hover))
                    .child(Icon::new(IconName::HardDrive).small().text_color(theme.muted_foreground))
                    .child(div().overflow_hidden().text_ellipsis().whitespace_nowrap().child(root))
                    .on_click(cx.listener(move |this, _, window, cx| this.load(Some(target.clone()), window, cx)))
            }));
        let list = v_flex()
            .id("folders")
            .flex_1()
            .min_w_0()
            .h_full()
            .overflow_y_scrollbar()
            .border_1()
            .border_color(theme.border)
            .rounded(theme.radius)
            .p_1()
            .when(self.loading && entries.is_empty(), |this| {
                this.child(h_flex().p_3().gap_2().text_sm().text_color(theme.muted_foreground).child(Spinner::new().small()).child(tr(cx, "app.browserLoading")))
            })
            .when_some(self.error.clone(), |this, error| this.child(div().p_3().text_sm().text_color(theme.danger).child(error)))
            .children(entries.into_iter().enumerate().map(|(index, entry)| {
                let target = entry.path.clone();
                h_flex()
                    .id(("folder", index))
                    .h(px(28.))
                    .px_2()
                    .gap_1p5()
                    .items_center()
                    .flex_shrink_0()
                    .rounded(theme.radius)
                    .text_sm()
                    .cursor_pointer()
                    .when(entry.hidden, |this| this.text_color(theme.muted_foreground))
                    .hover(|style| style.bg(theme.list_hover))
                    .child(Icon::new(IconName::Folder).small().text_color(theme.muted_foreground))
                    .child(div().overflow_hidden().text_ellipsis().whitespace_nowrap().child(entry.name))
                    .on_click(cx.listener(move |this, _, window, cx| this.load(Some(target.clone()), window, cx)))
            }));
        v_flex()
            .gap_3()
            .h(px(440.))
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("folder-up")
                            .outline()
                            .small()
                            .icon(IconName::ArrowUp)
                            .disabled(parent.is_none())
                            .on_click(cx.listener(move |this, _, window, cx| this.load(parent.clone(), window, cx))),
                    )
                    .child(div().flex_1().child(Input::new(&self.path).small())),
            )
            .child(h_flex().flex_1().min_h_0().gap_3().items_start().child(locations).child(list))
            .child(
                Switch::new("folder-hidden")
                    .checked(self.show_hidden)
                    .label(tr(cx, "app.browserShowHidden"))
                    .on_click(cx.listener(|this, checked: &bool, window, cx| {
                        this.show_hidden = *checked;
                        let current = this.current();
                        this.load(current, window, cx);
                    })),
            )
    }
}

/// Browse the Host's folders and open the chosen one as a project, or add it to the shown one.
pub fn open(attach: bool, window: &mut Window, cx: &mut App) {
    let browser = cx.new(|cx| FolderBrowser::new(window, cx));
    window.open_dialog(cx, move |dialog, _, cx| {
        let chosen = browser.clone();
        dialog
            .title(tr(cx, "app.browserTitle"))
            .w(px(760.))
            .child(browser.clone())
            .on_ok(|_, _, _| false)
            .footer(
                DialogFooter::new()
                    .child(Button::new("folder-cancel").outline().label(tr(cx, "app.browserCancel")).on_click(|_, window, cx| window.close_dialog(cx)))
                    .child(Button::new("folder-open").primary().label(tr(cx, "app.browserOpen")).on_click(move |_, window, cx| {
                        let Some(path) = chosen.read(cx).current() else { return };
                        window.close_dialog(cx);
                        Ide::global(cx).update(cx, |ide, cx| if attach { ide.attach_root(path, window, cx) } else { ide.open_folder(path, window, cx) });
                    })),
            )
    });
}
