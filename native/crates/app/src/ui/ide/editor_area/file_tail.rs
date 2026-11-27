//! Incremental live tail for oversized (>5 MiB) log or text files, backed by the shared virtualized `LogTailBuffer`.

use std::time::Duration;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use rainy_protocol::FileTailResult;
use serde_json::json;

use crate::store::Store;
use crate::store::ide::split_key;
use crate::ui::ide::log_tail::{LogTailBuffer, format_size};
use crate::ui::{tr, trf};

use super::EditorArea;

/// Active live-tail state for an oversized (>5 MiB) log or text file (`ide` `files.tail`).
pub(super) struct FileTail {
    pub key: String,
    pub offset: Option<u64>,
    pub buffer: LogTailBuffer,
    pub _task: Task<()>,
}

impl EditorArea {
    /// Start incremental `ide` `files.tail` streaming for an oversized text/log file.
    pub(super) fn start_file_tail(&mut self, key: String, cx: &mut Context<Self>) {
        let key_loop = key.clone();
        let task = cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            loop {
                let request = this
                    .update(cx, |this, cx| {
                        let tail = this.file_tail.as_ref()?;
                        if tail.key != key_loop {
                            return None;
                        }
                        if tail.buffer.paused {
                            return Some(None);
                        }
                        let workspace_id = this.ide.read(cx).workspace.as_ref()?.workspace_id.clone();
                        let (root, path) = split_key(&key_loop);
                        let mut params = json!({
                            "op": "files.tail",
                            "workspaceId": workspace_id,
                            "path": path,
                            "maxBytes": 65_536u64,
                        });
                        if let Some(root) = root {
                            params["rootId"] = json!(root);
                        }
                        if let Some(offset) = tail.offset {
                            params["offset"] = json!(offset);
                        }
                        let call = Store::global(cx).read(cx).call("ide", params);
                        Some(Some(call))
                    })
                    .ok()
                    .flatten();

                match request {
                    None => break,
                    Some(None) => {}
                    Some(Some(call)) => {
                        if let Ok(Ok(res)) = call.await.map(serde_json::from_value::<FileTailResult>) {
                            let _ = this.update(cx, |this, cx| {
                                let Some(tail) = this.file_tail.as_mut().filter(|t| t.key == key_loop) else { return };
                                tail.offset = Some(res.next_offset);
                                if tail.buffer.apply_chunk(&res.text, res.reset, res.total_bytes) {
                                    cx.notify();
                                }
                            });
                        }
                    }
                }
                cx.background_executor().timer(Duration::from_secs(1)).await;
            }
        });
        self.file_tail = Some(FileTail {
            key,
            offset: None,
            buffer: LogTailBuffer::new(),
            _task: task,
        });
        cx.notify();
    }

    pub(super) fn render_file_tail(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(tail) = &self.file_tail else { return div().into_any_element() };
        let theme = cx.theme().clone();
        let paused = tail.buffer.paused;
        let reset = tail.buffer.reset;
        let size_str = format_size(tail.buffer.total_bytes);
        v_flex()
            .size_full()
            .child(
                h_flex()
                    .flex_shrink_0()
                    .px_3()
                    .py_1p5()
                    .gap_2()
                    .items_center()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .border_b_1()
                    .border_color(theme.border)
                    .child(div().size(px(7.)).flex_shrink_0().rounded_full().bg(if paused { theme.warning } else { theme.success }))
                    .child(div().flex_1().min_w_0().truncate().child(trf(cx, "experiments.fileTailInfo", &[("size", &size_str)])))
                    .child(
                        Button::new("file-tail-pause")
                            .ghost()
                            .xsmall()
                            .label(tr(cx, if paused { "experiments.tailResume" } else { "experiments.tailPause" }))
                            .on_click(cx.listener(|this, _, _, cx| {
                                if let Some(tail) = this.file_tail.as_mut() {
                                    tail.buffer.toggle_pause();
                                    cx.notify();
                                }
                            })),
                    )
                    .child(
                        Button::new("file-tail-clear")
                            .ghost()
                            .xsmall()
                            .label(tr(cx, "experiments.tailClear"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                if let Some(tail) = this.file_tail.as_mut() {
                                    tail.buffer.clear_lines();
                                    cx.notify();
                                }
                            })),
                    )
                    .child(
                        Button::new("file-tail-quote")
                            .ghost()
                            .xsmall()
                            .label(tr(cx, "experiments.tailSendToChat"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                if let Some(tail) = &this.file_tail {
                                    tail.buffer.quote_to_chat(window, cx);
                                }
                            })),
                    )
                    .child(
                        Button::new("file-tail-stop")
                            .outline()
                            .xsmall()
                            .label(tr(cx, "experiments.fileTailStop"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.file_tail = None;
                                cx.notify();
                            })),
                    ),
            )
            .when(reset, |this| {
                this.child(
                    div()
                        .px_3()
                        .py_1()
                        .bg(theme.warning.opacity(0.10))
                        .text_xs()
                        .text_color(crate::theme::warning_text(cx))
                        .child(tr(cx, "experiments.tailResetNotice")),
                )
            })
            .child(tail.buffer.render_body("file-tail-body", Some(theme.mono_font_size), px(12.), cx))
            .into_any_element()
    }
}
