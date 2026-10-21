//! Memory (`MemorySection.tsx`): the current project's notes, their switches, editing and removal.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Textarea, TextareaState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::tag::Tag;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use serde::Deserialize;
use serde_json::{Value, json};

use super::models_extra::notice;
use super::{group, heading, row};
use crate::store::ide::{Ide, IdeEvent};
use crate::store::{Store, StoreEvent};
use crate::ui::{tr, trf};

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct Source {
    session_id: String,
    seq: u64,
    execution_target_id: String,
    #[serde(default)]
    file: Option<Value>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct Item {
    id: String,
    text: String,
    #[serde(default)]
    edited_by_user: Option<bool>,
    #[serde(default)]
    sources: Vec<Source>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct Snapshot {
    enabled: bool,
    generation_enabled: bool,
    revision: u64,
    #[serde(default)]
    updated_at: Option<String>,
    items: Vec<Item>,
    #[serde(default)]
    pending: Option<bool>,
    #[serde(default)]
    generating: Option<bool>,
    #[serde(default)]
    error: Option<String>,
}

/// An ISO time as local `YYYY-MM-DD HH:MM`.
pub(super) fn local_time(iso: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(iso)
        .map(|time| time.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|_| iso.to_owned())
}

pub struct MemoryPage {
    store: Entity<Store>,
    workspace: Option<String>,
    snapshot: Option<Snapshot>,
    error: Option<SharedString>,
    busy: bool,
    /// The note being edited, its field and the revision the edit started from.
    editing: Option<(String, Entity<TextareaState>, u64)>,
    clearing: bool,
    _subscriptions: Vec<Subscription>,
}

impl MemoryPage {
    pub fn new(store: Entity<Store>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let ide = Ide::global(cx);
        let subscriptions = vec![
            cx.subscribe_in(&ide, window, |this, _, event: &IdeEvent, window, cx| {
                if *event == IdeEvent::WorkspaceChanged {
                    this.reload(window, cx);
                }
            }),
            cx.subscribe_in(&store, window, |this, _, event: &StoreEvent, window, cx| {
                if let StoreEvent::Host { name, data } = event
                    && name == "memory.changed"
                    && data.get("workspaceId").and_then(Value::as_str) == this.workspace.as_deref()
                {
                    this.reload(window, cx);
                }
            }),
        ];
        let mut page = Self { store, workspace: None, snapshot: None, error: None, busy: false, editing: None, clearing: false, _subscriptions: subscriptions };
        page.reload(window, cx);
        page
    }

    fn reload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let workspace = Ide::global(cx).read(cx).workspace.as_ref().map(|workspace| workspace.workspace_id.clone());
        if workspace != self.workspace {
            self.snapshot = None;
            self.editing = None;
            self.clearing = false;
        }
        self.workspace = workspace.clone();
        let Some(workspace) = workspace else {
            cx.notify();
            return;
        };
        self.apply("memory.status", json!({ "workspaceId": workspace }), None, window, cx);
    }

    /// Run a memory operation; its answer is the new snapshot.
    fn apply(&mut self, method: &str, params: Value, success: Option<&'static str>, window: &mut Window, cx: &mut Context<Self>) {
        let call = self.store.read(cx).call(method, params);
        let workspace = self.workspace.clone();
        let reading = method == "memory.status";
        if !reading {
            self.busy = true;
        }
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = call.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                if this.workspace != workspace {
                    return;
                }
                match result.map(serde_json::from_value::<Snapshot>) {
                    Ok(Ok(snapshot)) => {
                        this.snapshot = Some(snapshot);
                        this.error = None;
                        if let Some(key) = success {
                            this.editing = None;
                            window.push_notification(tr(cx, key).to_string(), cx);
                        }
                    }
                    Ok(Err(error)) => this.error = Some(error.to_string().into()),
                    Err(error) if reading => this.error = Some(error.message.into()),
                    Err(error) => window.push_notification(Notification::error(error.message), cx),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn render_item(&self, item: &Item, revision: u64, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let editing = self.editing.as_ref().filter(|(id, _, _)| *id == item.id).map(|(_, field, _)| field.clone());
        let id = item.id.clone();
        let mut sources: Vec<AnyElement> = Vec::new();
        for source in &item.sources {
            let mut text = trf(cx, "settings.settingsMemorySource", &[("session", &source.session_id), ("seq", &source.seq), ("target", &source.execution_target_id)]).to_string();
            if let Some(path) = source.file.as_ref().and_then(|file| file["path"].as_str()) {
                text.push_str(&format!(" · {path}"));
            }
            sources.push(div().text_xs().text_color(theme.muted_foreground).child(text).into_any_element());
        }
        let actions = match editing.clone() {
            Some(field) => {
                let (save_id, empty) = (id.clone(), field.read(cx).value().trim().is_empty());
                h_flex()
                    .gap_2()
                    .child(Button::new(SharedString::from(format!("memory-save-{id}"))).primary().small().label(tr(cx, "settings.settingsSave")).disabled(self.busy || empty).on_click(
                        cx.listener(move |this, _, window, cx| {
                            let Some((_, field, revision)) = this.editing.clone() else { return };
                            let text = field.read(cx).value().to_string();
                            let workspace = this.workspace.clone();
                            this.apply("memory.edit", json!({ "workspaceId": workspace, "id": save_id, "text": text, "expectedRevision": revision }), Some("settings.settingsSaved"), window, cx);
                        }),
                    ))
                    .child(Button::new(SharedString::from(format!("memory-cancel-{id}"))).outline().small().label(tr(cx, "settings.settingsCancel")).on_click(cx.listener(|this, _, _, cx| {
                        this.editing = None;
                        cx.notify();
                    })))
            }
            None => {
                let (edit_id, delete_id, text) = (id.clone(), id.clone(), item.text.clone());
                h_flex()
                    .gap_2()
                    .child(Button::new(SharedString::from(format!("memory-edit-{id}"))).outline().small().label(tr(cx, "settings.settingsMemoryEdit")).disabled(self.busy).on_click(cx.listener(
                        move |this, _, window, cx| {
                            let value = text.clone();
                            let field = cx.new(|cx| TextareaState::new(window, cx).auto_grow(3, 12).default_value(value));
                            this.editing = Some((edit_id.clone(), field, revision));
                            cx.notify();
                        },
                    )))
                    .child(Button::new(SharedString::from(format!("memory-delete-{id}"))).ghost().small().label(tr(cx, "settings.settingsMemoryDelete")).disabled(self.busy).on_click(
                        cx.listener(move |this, _, window, cx| {
                            let workspace = this.workspace.clone();
                            this.apply("memory.delete", json!({ "workspaceId": workspace, "id": delete_id }), Some("settings.settingsApplied"), window, cx);
                        }),
                    ))
            }
        };
        v_flex()
            .gap_2()
            .p_4()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.border)
            .bg(theme.group_box)
            .map(|this| match editing {
                Some(field) => this.child(Textarea::new(&field)),
                None => this.child(div().text_sm().child(item.text.clone())),
            })
            .when(item.edited_by_user == Some(true), |this| this.child(h_flex().child(Tag::secondary().small().child(tr(cx, "settings.settingsMemoryEdited")))))
            .children(sources)
            .child(actions)
            .into_any_element()
    }
}

impl Render for MemoryPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let mut page = v_flex().gap_4().child(heading(cx, "settings.settingsMemory", "settings.settingsMemoryNote"));
        let Some(workspace) = self.workspace.clone() else {
            return page.child(notice(cx, tr(cx, "settings.settingsOpenProject"), false));
        };
        page = page.child(h_flex().child(Button::new("memory-refresh").outline().small().label(tr(cx, "settings.settingsRefresh")).disabled(self.busy).on_click(
            cx.listener(|this, _, window, cx| this.reload(window, cx)),
        )));
        if let Some(error) = &self.error {
            page = page.child(notice(cx, error.clone(), true));
        }
        let Some(snapshot) = self.snapshot.clone() else {
            return page.child(h_flex().child(Spinner::new().small()));
        };
        let (use_workspace, generate_workspace) = (workspace.clone(), workspace.clone());
        page = page.child(group(cx, None, vec![
            row(
                cx,
                tr(cx, "settings.settingsMemoryUse"),
                None,
                Switch::new("memory-enabled").checked(snapshot.enabled).disabled(self.busy).on_click(cx.listener(move |this, checked: &bool, window, cx| {
                    this.apply("memory.setEnabled", json!({ "workspaceId": use_workspace, "enabled": *checked }), Some("settings.settingsApplied"), window, cx);
                })),
            ),
            row(
                cx,
                tr(cx, "settings.settingsMemoryGenerate"),
                Some(tr(cx, "settings.settingsMemoryGenerateNote")),
                Switch::new("memory-generate").checked(snapshot.generation_enabled).disabled(self.busy).on_click(cx.listener(move |this, checked: &bool, window, cx| {
                    this.apply("memory.setEnabled", json!({ "workspaceId": generate_workspace, "generationEnabled": *checked }), Some("settings.settingsApplied"), window, cx);
                })),
            ),
        ]));
        if snapshot.generating == Some(true) {
            page = page.child(notice(cx, tr(cx, "settings.settingsMemoryGenerating"), false));
        } else if snapshot.pending == Some(true) {
            page = page.child(notice(cx, tr(cx, "settings.settingsMemoryPending"), false));
        }
        if let Some(error) = &snapshot.error {
            page = page.child(notice(cx, error.clone().into(), true));
        }
        if let Some(updated) = &snapshot.updated_at {
            page = page.child(div().text_xs().text_color(theme.muted_foreground).child(trf(cx, "settings.settingsMemoryUpdated", &[("time", &local_time(updated))])));
        }
        if snapshot.items.is_empty() {
            page = page.child(notice(cx, tr(cx, "settings.settingsMemoryEmpty"), false));
        }
        let items: Vec<AnyElement> = snapshot.items.iter().map(|item| self.render_item(item, snapshot.revision, cx)).collect();
        page = page.children(items);
        if !snapshot.items.is_empty() {
            let clear_workspace = workspace;
            page = page.child(if self.clearing {
                h_flex()
                    .gap_2()
                    .child(Button::new("memory-clear-confirm").danger().small().label(tr(cx, "settings.settingsMemoryClearConfirm")).disabled(self.busy).on_click(cx.listener(
                        move |this, _, window, cx| {
                            this.clearing = false;
                            this.apply("memory.clear", json!({ "workspaceId": clear_workspace }), Some("settings.settingsApplied"), window, cx);
                        },
                    )))
                    .child(Button::new("memory-clear-cancel").outline().small().label(tr(cx, "settings.settingsCancel")).on_click(cx.listener(|this, _, _, cx| {
                        this.clearing = false;
                        cx.notify();
                    })))
            } else {
                h_flex().child(Button::new("memory-clear").outline().small().label(tr(cx, "settings.settingsMemoryClear")).disabled(self.busy).on_click(cx.listener(|this, _, _, cx| {
                    this.clearing = true;
                    cx.notify();
                })))
            });
        }
        page
    }
}
