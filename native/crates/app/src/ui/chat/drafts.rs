//! Composer drafts, image attachments, message dispatch and failure recovery.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;

use gpui_kit::component::notification::Notification;
use gpui_kit::component::WindowExt as _;
use gpui_kit::*;
use rainy_protocol::{ImageContent, RpcError};
use serde_json::{Value, json};

use super::ChatPane;
use crate::ui::composer::{ImageError, MAX_IMAGES};
use crate::ui::{tr, trf};

/// Maximum number of background chat drafts (and their image attachments) kept in memory.
pub(super) const MAX_DRAFTS: usize = 24;

/// An image the next message carries: what is sent, and its thumbnail, decoded once when it was attached.
#[derive(Clone)]
pub(super) struct Attachment {
    pub(super) content: ImageContent,
    pub(super) preview: Arc<Image>,
}

/// What the composer held for a chat while another one was shown.
#[derive(Default)]
pub(super) struct Draft {
    pub(super) text: String,
    pub(super) attachments: Vec<Attachment>,
}

/// An image being attached: pasted bytes, or a file read off the interface thread.
pub(super) enum NewImage {
    Bytes(Vec<u8>),
    File(PathBuf),
}

/// Record recency for `key` and evict the oldest non-empty-session drafts beyond `cap`.
pub(super) fn touch_and_evict_draft<T>(drafts: &mut HashMap<Option<String>, T>, order: &mut VecDeque<Option<String>>, key: Option<String>, cap: usize) {
    order.retain(|k| k != &key);
    order.push_back(key);
    while drafts.len() > cap {
        if let Some(pos) = order.iter().position(|k| k.is_some()) {
            if let Some(oldest) = order.remove(pos) {
                drafts.remove(&oldest);
            }
        } else {
            break;
        }
    }
}

impl ChatPane {
    /// Keep the composer's text and images with the chat they were typed for, and bring back those of `next`.
    pub(super) fn swap_draft(&mut self, next: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        if next == self.draft_key {
            return;
        }
        let text = self.composer.read(cx).value().to_string();
        let attachments = std::mem::take(&mut self.attachments);
        let previous = std::mem::replace(&mut self.draft_key, next.clone());
        if text.trim().is_empty() && attachments.is_empty() {
            self.draft_order.retain(|k| k != &previous);
            self.drafts.remove(&previous);
        } else {
            self.drafts.insert(previous.clone(), Draft { text, attachments });
            touch_and_evict_draft(&mut self.drafts, &mut self.draft_order, previous, MAX_DRAFTS);
        }
        self.draft_order.retain(|k| k != &next);
        let draft = self.drafts.remove(&next).unwrap_or_default();
        self.attachments = draft.attachments;
        self.composer.update(cx, |state, cx| state.set_value(draft.text, window, cx));
        self.cancel_suggestions();
    }

    /// Send the composer's text; while the chat works it is queued or steered (`busyEnter`, Ctrl+Enter for the other).
    /// The composer empties at once; a message that does not go out comes back to the draft it was typed in.
    pub(super) fn send(&mut self, alternate: bool, window: &mut Window, cx: &mut Context<Self>) {
        let typed = self.composer.read(cx).value().to_string();
        let text = typed.trim().to_owned();
        if text.is_empty() && self.attachments.is_empty() {
            return;
        }
        // In an external agent's chat only /new is Rainy's; any other command goes to the agent as typed.
        let external = self.is_external_chat(cx);
        if self.attachments.is_empty()
            && let Some(name) = text.strip_prefix('/').filter(|name| crate::ui::composer::COMMANDS.iter().any(|(command, _)| command == name) && (!external || *name == "new"))
        {
            let name = name.to_owned();
            self.run_command(&name, window, cx);
            return;
        }
        // One new chat at a time: what is typed meanwhile waits until the first message's chat exists.
        if self.creating_here() {
            return;
        }
        // The draft belongs to the chat shown (`swap_draft`), which is where it goes.
        let session = self.draft_key.clone();
        let images = std::mem::take(&mut self.attachments);
        self.cancel_suggestions();
        let mode = self.busy(cx).then(|| {
            let steer_first = self.store.read(cx).prefs.busy_enter == "steer";
            if steer_first != alternate { "steer" } else { "queue" }
        });
        self.composer.update(cx, |state, cx| state.set_value("", window, cx));
        self.list.set_follow_mode(FollowMode::Tail);
        self.list.scroll_to_end();
        self.scrolled_up = false;
        let store = self.store.clone();
        let workspace = crate::store::ide::Ide::global(cx).read(cx).workspace.as_ref().map(|workspace| workspace.workspace_id.clone());
        if session.is_none() {
            self.creating = true;
        }
        let since = self.store.read(cx).selections;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let session = match session {
                Some(id) => id,
                None => {
                    let created = store.update(cx, |store, cx| store.create_chat(workspace, cx)).await;
                    let _ = this.update(cx, |this, cx| {
                        this.creating = false;
                        cx.notify();
                        // The chat made for the message is shown only while the user still waits for it on the new
                        // chat; one who went to another chat meanwhile stays there and finds it in the list.
                        if let Ok(id) = &created
                            && this.store.read(cx).selections == since
                        {
                            // The composer, still on the new chat, now belongs to the chat made for its message.
                            if this.draft_key.is_none() {
                                this.draft_key = Some(id.clone());
                            }
                            this.store.update(cx, |store, cx| store.select(Some(id.clone()), cx));
                        }
                    });
                    match created {
                        Ok(id) => id,
                        Err(error) => {
                            let _ = this.update_in(cx, |this, window, cx| this.send_failed(None, typed, images, &error, window, cx));
                            return;
                        }
                    }
                }
            };
            let mut params = json!({ "sessionId": session, "text": text });
            if !images.is_empty() {
                params["images"] = json!(images.iter().map(|image| &image.content).collect::<Vec<_>>());
            }
            if let Some(mode) = mode {
                params["mode"] = json!(mode);
            }
            let call = store.read_with(cx, |store, _| store.call("chat.send", params));
            if let Err(error) = call.await {
                let _ = this.update_in(cx, |this, window, cx| this.send_failed(Some(session), typed, images, &error, window, cx));
            }
        })
        .detach();
    }

    /// Say why a message did not go out, and put it back in front of what its draft holds now.
    fn send_failed(&mut self, key: Option<String>, text: String, images: Vec<Attachment>, error: &RpcError, window: &mut Window, cx: &mut Context<Self>) {
        let message = trf(cx, "chat.sendFailed", &[("message", &crate::ui::error_text(cx, error))]);
        window.push_notification(Notification::error(message), cx);
        if key == self.draft_key {
            let typed = self.composer.read(cx).value().to_string();
            let (text, attachments) = (merge_text(text, typed), merge_images(images, std::mem::take(&mut self.attachments)));
            self.attachments = attachments;
            self.composer.update(cx, |state, cx| state.set_value(text, window, cx));
        } else {
            let draft = self.drafts.entry(key.clone()).or_default();
            draft.text = merge_text(text, std::mem::take(&mut draft.text));
            draft.attachments = merge_images(images, std::mem::take(&mut draft.attachments));
            touch_and_evict_draft(&mut self.drafts, &mut self.draft_order, key, MAX_DRAFTS);
        }
        cx.notify();
    }

    /// Encode an image a viewer page sent, then attach it; `added` is said once it is attached.
    pub fn attach_bytes(&mut self, name: String, bytes: Vec<u8>, added: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        self.attach_image(name, NewImage::Bytes(bytes), added, window, cx);
    }

    /// Read and encode an image off the interface thread, then attach it to the draft it was added to; `added` is said
    /// once it is attached.
    pub(super) fn attach_image(&mut self, name: String, image: NewImage, added: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let key = self.draft_key.clone();
        let prepared = cx.background_spawn(async move {
            let bytes = match image {
                NewImage::Bytes(bytes) => bytes,
                NewImage::File(path) => crate::ui::composer::read_image(&path)?,
            };
            let (content, shown) = crate::ui::composer::prepare_image(&bytes)?;
            let preview = Arc::new(Image::from_bytes(image_format(&content.mime_type), shown));
            Ok::<_, ImageError>(Attachment { content, preview })
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = prepared.await;
            let _ = this.update_in(cx, |this, window, cx| {
                match result {
                    Ok(attachment) => {
                        let is_current = key == this.draft_key;
                        let attachments = if is_current {
                            &mut this.attachments
                        } else {
                            &mut this.drafts.entry(key.clone()).or_default().attachments
                        };
                        if attachments.len() < MAX_IMAGES {
                            attachments.push(attachment);
                            if !is_current {
                                touch_and_evict_draft(&mut this.drafts, &mut this.draft_order, key, MAX_DRAFTS);
                            }
                            if let Some(added) = added {
                                window.push_notification(Notification::success(added), cx);
                            }
                        } else {
                            window.push_notification(Notification::error(trf(cx, "chat.imageLimit", &[("count", &MAX_IMAGES)])), cx);
                        }
                    }
                    Err(error) => {
                        let text = match &error {
                            ImageError::TooLarge => trf(cx, "chat.imageTooLarge", &[("name", &name)]),
                            ImageError::Unreadable => trf(cx, "chat.imageUnsupported", &[("name", &name)]),
                            ImageError::Read(error) => trf(cx, "chat.imageReadFailed", &[("name", &name), ("message", error)]),
                        };
                        window.push_notification(Notification::error(text), cx);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Files from Windows: images are attached, other files are referenced with `@` and their path as the Host sees it.
    pub(super) fn attach_paths(&mut self, paths: Vec<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        let target = self.store.read(cx).target.clone();
        let mut references = Vec::new();
        for path in paths {
            let name = path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
            if crate::ui::composer::is_image_file(&path) {
                self.attach_image(name, NewImage::File(path), None, window, cx);
                continue;
            }
            match crate::ui::composer::host_path(&path, target.as_ref()) {
                Some(host) => references.push(format!("@{}", crate::ui::composer::quote_path(&host))),
                None => window.push_notification(Notification::error(trf(cx, "chat.attachPathFailed", &[("name", &name)]).to_string()), cx),
            }
        }
        if !references.is_empty() {
            let text = format!("{} ", references.join(" "));
            self.composer.update(cx, |state, cx| {
                state.focus(window, cx);
                state.insert(text, window, cx);
            });
        }
    }

    /// Choose image files to attach.
    pub(super) fn choose_images(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let chosen = cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: true, prompt: Some(tr(cx, "chat.attachImage")) });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = chosen.await else { return };
            let _ = this.update_in(cx, |this, window, cx| this.attach_paths(paths, window, cx));
        })
        .detach();
    }
}

/// The text of a message that did not go out, in front of what was typed since.
pub(super) fn merge_text(sent: String, typed: String) -> String {
    if typed.trim().is_empty() { sent } else { format!("{sent}\n{typed}") }
}

/// The images of a message that did not go out, in front of those added since, up to the most a message carries.
pub(super) fn merge_images(mut sent: Vec<Attachment>, added: Vec<Attachment>) -> Vec<Attachment> {
    sent.extend(added);
    sent.truncate(MAX_IMAGES);
    sent
}

fn image_format(mime: &str) -> ImageFormat {
    match mime {
        "image/png" => ImageFormat::Png,
        "image/gif" => ImageFormat::Gif,
        "image/webp" => ImageFormat::Webp,
        _ => ImageFormat::Jpeg,
    }
}

/// Wait for a Host call and show its failure, in the interface language.
pub(super) fn report_failure(request: impl Future<Output = Result<Value, RpcError>> + 'static, window: &mut Window, cx: &mut App) {
    window
        .spawn(cx, async move |cx| {
            if let Err(error) = request.await {
                let _ = cx.update(|window, cx| window.push_notification(crate::ui::error_notification(cx, &error), cx));
            }
        })
        .detach();
}

/// Like [`report_failure`], and also say what a request that went through answers (`message`), such as what a
/// compaction did or that there was nothing to compact.
pub(super) fn report_outcome(request: impl Future<Output = Result<Value, RpcError>> + 'static, window: &mut Window, cx: &mut App) {
    window
        .spawn(cx, async move |cx| {
            let result = request.await;
            let _ = cx.update(|window, cx| match result {
                Ok(value) => {
                    if let Some(message) = value["message"].as_str().filter(|message| !message.is_empty()) {
                        window.push_notification(message.to_owned(), cx);
                    }
                }
                Err(error) => window.push_notification(crate::ui::error_notification(cx, &error), cx),
            });
        })
        .detach();
}
