//! Host calls made from buttons and menus: the answer is not needed, a failure is shown as a notification.

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::notification::Notification;
use gpui_kit::*;
use serde_json::Value;

use crate::store::Store;

/// Call `method` and report a failure in `window`.
pub fn run(method: &str, params: Value, window: AnyWindowHandle, cx: &mut App) {
    let request = Store::global(cx).read(cx).call(method, params);
    cx.spawn(async move |cx: &mut AsyncApp| {
        if let Err(error) = request.await {
            let _ = window.update(cx, |_, window, cx| window.push_notification(Notification::error(error.message), cx));
        }
    })
    .detach();
}
