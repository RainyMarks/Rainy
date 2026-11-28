//! Managing many chats and projects at once: the bulk actions on chosen chats (`sessions.bulk`) and their
//! confirmation, the archived chats' window (search, choose, restore or delete many) and the project manager (search,
//! choose, pin, archive the chats of or remove many projects).

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use rainy_protocol::{IdeWorkspace, SessionSummary};
use serde_json::json;

use crate::store::ide::Ide;
use crate::store::{Store, StoreEvent};
use crate::ui::actions::run;
use crate::ui::history::{ago_from, confirm_delete, folder_name, found, newest_first, now_millis, title_of};
use crate::ui::prompt::prompt_text;
use crate::ui::{tr, trf};

/// Most chats one `sessions.bulk` request carries; a larger choice goes in several.
const BULK_CHUNK: usize = 500;

/// Pin, unpin, archive, restore or delete many chats through the Host (`action` is `pin`, `unpin`, `archive`,
/// `unarchive` or `delete`), then say in one notification how many it did and, when some could not be done, why the
/// first of those failed.
pub fn bulk(action: &'static str, ids: Vec<String>, window: &mut Window, cx: &mut App) {
    if ids.is_empty() {
        return;
    }
    let store = Store::global(cx);
    let requests: Vec<_> = ids.chunks(BULK_CHUNK).map(|chunk| store.read(cx).call("sessions.bulk", json!({ "action": action, "sessionIds": chunk }))).collect();
    window
        .spawn(cx, async move |cx| {
            let (mut done, mut failed, mut reason, mut error) = (0u64, 0u64, None::<String>, None);
            for request in requests {
                match request.await {
                    Ok(answer) => {
                        done += answer["done"].as_u64().unwrap_or(0);
                        let missed = answer["failed"].as_array().cloned().unwrap_or_default();
                        failed += missed.len() as u64;
                        if reason.is_none() {
                            reason = missed.first().and_then(|first| first["message"].as_str()).map(str::to_owned);
                        }
                    }
                    Err(failure) => {
                        error = Some(failure);
                        break;
                    }
                }
            }
            let _ = cx.update(|window, cx| {
                if done > 0 {
                    let key = match action {
                        "pin" => "agent.bulkPinned",
                        "unpin" => "agent.bulkUnpinned",
                        "archive" => "agent.bulkArchived",
                        "unarchive" => "agent.bulkUnarchived",
                        _ => "agent.bulkDeleted",
                    };
                    window.push_notification(Notification::success(trf(cx, key, &[("count", &done)]).to_string()), cx);
                }
                if failed > 0 {
                    let reason = reason.unwrap_or_default();
                    window.push_notification(Notification::warning(trf(cx, "agent.bulkFailed", &[("count", &failed), ("message", &reason)]).to_string()), cx);
                }
                if let Some(error) = error {
                    window.push_notification(crate::ui::error_notification(cx, &error), cx);
                }
            });
        })
        .detach();
}

/// Up to three names in quotes, then a word for the rest: `「甲」、「乙」、「丙」等`, `"A", "B", "C" and more`.
fn quoted(names: &[SharedString], cx: &App) -> String {
    quoted_in(names, crate::ui::locale(cx) == crate::i18n::Locale::Zh)
}

fn quoted_in(names: &[SharedString], zh: bool) -> String {
    let short = |name: &SharedString| {
        let mut text: String = name.chars().take(24).collect();
        if name.chars().count() > 24 {
            text.push('…');
        }
        text
    };
    let shown: Vec<String> = names.iter().take(3).map(|name| if zh { format!("「{}」", short(name)) } else { format!("“{}”", short(name)) }).collect();
    let more = names.len() > shown.len();
    match (zh, more) {
        (true, true) => format!("{}等", shown.join("、")),
        (true, false) => shown.join("、"),
        (false, true) => format!("{} and more", shown.join(", ")),
        (false, false) => shown.join(", "),
    }
}

/// Ask once, then delete the chats; `after` runs when the user agreed (to forget the choice). One chat is asked about
/// as its own menu does.
pub fn confirm_bulk_delete(ids: Vec<String>, titles: Vec<SharedString>, after: impl Fn(&mut App) + 'static, window: &mut Window, cx: &mut App) {
    let after = Rc::new(after);
    let count = ids.len();
    let (title, description) = match titles.as_slice() {
        [only] if count == 1 => (tr(cx, "chat.delete"), trf(cx, "chat.deleteConfirm", &[("title", only)])),
        _ => (trf(cx, "agent.deleteChats", &[("count", &count)]), trf(cx, "agent.deleteChatsConfirm", &[("count", &count), ("titles", &quoted(&titles, cx))])),
    };
    window.open_alert_dialog(cx, move |alert, _, cx| {
        let (ids, after) = (ids.clone(), after.clone());
        alert
            .title(title.clone())
            .description(description.clone())
            .ok_text(tr(cx, "chat.delete"))
            .cancel_text(tr(cx, "chat.cancel"))
            .show_cancel(true)
            .on_ok(move |_, window, cx| {
                bulk("delete", ids.clone(), window, cx);
                after(cx);
                true
            })
    });
}

/// A square that shows whether a row is chosen; the row itself takes the click.
pub fn check_box(checked: bool, cx: &App) -> AnyElement {
    let theme = cx.theme();
    div()
        .size(px(14.))
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(3.))
        .border_1()
        .border_color(if checked { theme.primary } else { theme.muted_foreground.opacity(0.7) })
        .when(checked, |this| this.bg(theme.primary).child(Icon::new(IconName::Check).xsmall().text_color(theme.primary_foreground)))
        .into_any_element()
}

/// The words of a search field, lower-cased.
fn words_of(search: &Entity<InputState>, cx: &App) -> Vec<String> {
    search.read(cx).value().split_whitespace().map(str::to_lowercase).take(8).collect()
}

/// A search field for a manager window.
fn search_field(search: &Entity<InputState>, cx: &App) -> impl IntoElement {
    Input::new(search).small().cleanable(true).prefix(Icon::new(IconName::Search).small().text_color(cx.theme().muted_foreground))
}

/// The archived chats, in their own window: find them, open, restore or delete one, or choose several and restore or
/// delete them together.
pub fn open_archived(store: Entity<Store>, window: &mut Window, cx: &mut App) {
    let list = cx.new(|cx| ArchivedList::new(store, window, cx));
    window.open_dialog(cx, move |dialog, _, cx| dialog.title(tr(cx, "agent.archivedChats")).w(px(640.)).child(list.clone()));
}

struct ArchivedList {
    store: Entity<Store>,
    search: Entity<InputState>,
    chosen: HashSet<String>,
    _subscriptions: Vec<Subscription>,
}

impl ArchivedList {
    fn new(store: Entity<Store>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder(tr(cx, "agent.searchArchived")).clean_on_escape());
        let subscriptions = vec![
            cx.subscribe(&store, |_, _, event: &StoreEvent, cx| {
                if matches!(event, StoreEvent::SummariesChanged | StoreEvent::PrefsChanged) {
                    cx.notify();
                }
            }),
            cx.subscribe(&search, |_, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            }),
        ];
        Self { store, search, chosen: HashSet::new(), _subscriptions: subscriptions }
    }
}

impl Render for ArchivedList {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (border, muted_foreground) = {
            let theme = cx.theme();
            (theme.border, theme.muted_foreground)
        };
        let zh = crate::ui::locale(cx) == crate::i18n::Locale::Zh;
        let now_ms = now_millis();
        let workspaces = Ide::global(cx).read(cx).workspaces.clone();
        let titles: HashMap<&str, &str> = workspaces.iter().map(|workspace| (workspace.workspace_id.as_str(), workspace.title.as_str())).collect();
        let summaries = self.store.read(cx).summaries.clone();
        let words = words_of(&self.search, cx);
        let untitled = tr(cx, "chat.newChat");
        let shown = newest_first(summaries.iter().filter(|summary| summary.archived && found(summary, &untitled, &words)));
        // Only what the list shows stays chosen, so an action never reaches a chat the user cannot see.
        let shown_ids: HashSet<&str> = shown.iter().map(|summary| summary.id.as_str()).collect();
        self.chosen.retain(|id| shown_ids.contains(id.as_str()));
        let chosen: Vec<&SessionSummary> = shown.iter().copied().filter(|summary| self.chosen.contains(&summary.id)).collect();
        let ids: Vec<String> = chosen.iter().map(|summary| summary.id.clone()).collect();
        let chosen_titles: Vec<SharedString> = chosen.iter().map(|summary| title_of(summary, cx)).collect();
        let every: Vec<String> = shown.iter().map(|summary| summary.id.clone()).collect();
        let all_chosen = !shown.is_empty() && chosen.len() == shown.len();
        let none = ids.is_empty();
        let restore_ids = ids.clone();
        let weak = cx.entity().downgrade();
        let bar = h_flex()
            .w_full()
            .h(px(32.))
            .gap_2()
            .items_center()
            .child(
                Checkbox::new("archived-all")
                    .checked(all_chosen)
                    .disabled(shown.is_empty())
                    .label(tr(cx, "agent.selectAll"))
                    .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                        this.chosen = if *checked { every.iter().cloned().collect() } else { HashSet::new() };
                        cx.notify();
                    })),
            )
            .child(div().flex_1().min_w_0().text_xs().text_color(muted_foreground).when(!none, |this| this.child(trf(cx, "agent.selectedCount", &[("count", &ids.len())]))))
            .child(
                Button::new("archived-restore-chosen")
                    .outline()
                    .xsmall()
                    .icon(Icon::new(gpui_kit::assets::IconName::ArchiveRestore))
                    .label(tr(cx, "chat.unarchive"))
                    .disabled(none)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        bulk("unarchive", restore_ids.clone(), window, cx);
                        this.chosen.clear();
                        cx.notify();
                    })),
            )
            .child(
                Button::new("archived-delete-chosen")
                    .outline()
                    .xsmall()
                    .icon(Icon::new(gpui_kit::assets::IconName::Trash))
                    .label(tr(cx, "chat.delete"))
                    .disabled(none)
                    .on_click(move |_, window, cx| {
                        let weak = weak.clone();
                        confirm_bulk_delete(
                            ids.clone(),
                            chosen_titles.clone(),
                            move |cx| {
                                let _ = weak.update(cx, |this, cx| {
                                    this.chosen.clear();
                                    cx.notify();
                                });
                            },
                            window,
                            cx,
                        );
                    }),
            );
        let rows: Vec<AnyElement> = shown
            .iter()
            .map(|summary| {
                let title = title_of(summary, cx);
                let (toggle, open, restore, delete) = (summary.id.clone(), summary.id.clone(), summary.id.clone(), (summary.id.clone(), title.clone()));
                let store = self.store.clone();
                h_flex()
                    .w_full()
                    .py_1p5()
                    .gap_2()
                    .items_center()
                    .border_b_1()
                    .border_color(border)
                    .child(Checkbox::new(ElementId::Name(format!("archived-check-{}", summary.id).into())).checked(self.chosen.contains(&summary.id)).on_click(cx.listener(
                        move |this, checked: &bool, _, cx| {
                            if *checked {
                                this.chosen.insert(toggle.clone());
                            } else {
                                this.chosen.remove(&toggle);
                            }
                            cx.notify();
                        },
                    )))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(div().text_sm().overflow_hidden().text_ellipsis().whitespace_nowrap().child(title))
                            .child(div().text_xs().text_color(muted_foreground).child(format!("{} · {}", folder_name(summary, &titles), ago_from(now_ms, summary.updated_at, zh)))),
                    )
                    .child(Button::new(ElementId::Name(format!("archived-open-{open}").into())).ghost().xsmall().label(tr(cx, "agent.open")).on_click(move |_, window, cx| {
                        store.update(cx, |store, cx| store.select(Some(open.clone()), cx));
                        window.close_dialog(cx);
                    }))
                    .child(Button::new(ElementId::Name(format!("archived-restore-{restore}").into())).outline().xsmall().label(tr(cx, "chat.unarchive")).on_click(move |_, window, cx| {
                        run("sessions.archive", json!({ "sessionId": restore, "archived": false }), window.window_handle(), cx);
                    }))
                    .child(Button::new(ElementId::Name(format!("archived-delete-{}", delete.0).into())).ghost().xsmall().label(tr(cx, "chat.delete")).on_click(move |_, window, cx| {
                        confirm_delete(delete.0.clone(), delete.1.clone(), window, cx);
                    }))
                    .into_any_element()
            })
            .collect();
        let empty = rows.is_empty();
        let any_archived = summaries.iter().any(|summary| summary.archived);
        v_flex()
            .w_full()
            .gap_2()
            .when(any_archived, |this| this.child(search_field(&self.search, cx)).child(bar))
            .child(
                v_flex()
                    .id("archived-list")
                    .w_full()
                    .max_h(px(400.))
                    .overflow_y_scroll()
                    .children(rows)
                    .when(empty, |this| {
                        this.child(div().py_4().text_sm().text_color(muted_foreground).child(tr(cx, if any_archived { "chat.noResults" } else { "agent.noArchived" })))
                    }),
            )
    }
}

/// The projects in their own window: find them, pin, rename or remove one, or choose several to pin, unpin, archive
/// the chats of or remove from the list together. Removing keeps the chats and the files.
pub fn open_projects(window: &mut Window, cx: &mut App) {
    let view = cx.new(|cx| ProjectManager::new(window, cx));
    window.open_dialog(cx, move |dialog, _, cx| dialog.title(tr(cx, "agent.manageProjects")).w(px(680.)).child(view.clone()));
}

struct ProjectManager {
    search: Entity<InputState>,
    chosen: HashSet<String>,
    _subscriptions: Vec<Subscription>,
}

impl ProjectManager {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder(tr(cx, "agent.searchProjects")).clean_on_escape());
        let (ide, store) = (Ide::global(cx), Store::global(cx));
        let subscriptions = vec![
            cx.observe(&ide, |_, _, cx| cx.notify()),
            cx.subscribe(&store, |_, _, event: &StoreEvent, cx| {
                if matches!(event, StoreEvent::SummariesChanged | StoreEvent::PrefsChanged) {
                    cx.notify();
                }
            }),
            cx.subscribe(&search, |_, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            }),
        ];
        Self { search, chosen: HashSet::new(), _subscriptions: subscriptions }
    }
}

/// Pin or unpin projects, each through the Host as its own menu does.
fn pin_projects(workspaces: &[IdeWorkspace], pinned: bool, window: &mut Window, cx: &mut App) {
    for workspace in workspaces.iter().filter(|workspace| workspace.pinned != pinned) {
        let request = json!({ "op": "workspaces.pin", "workspaceId": workspace.workspace_id, "pinned": pinned });
        Ide::global(cx).update(cx, |ide, cx| ide.change_workspace(request, window, cx));
    }
}

/// Ask once, then take projects off the list; their chats and files stay. `after` runs when the user agreed.
fn confirm_remove_projects(workspaces: Vec<IdeWorkspace>, after: impl Fn(&mut App) + 'static, window: &mut Window, cx: &mut App) {
    let after = Rc::new(after);
    let count = workspaces.len();
    let (title, description) = match workspaces.as_slice() {
        [only] => (tr(cx, "ide.ideRemoveWorkspace"), trf(cx, "ide.ideRemoveWorkspaceDescription", &[("title", &only.title)])),
        _ => {
            let names = quoted(&workspaces.iter().map(|workspace| SharedString::from(workspace.title.clone())).collect::<Vec<_>>(), cx);
            (trf(cx, "agent.removeProjects", &[("count", &count)]), trf(cx, "agent.removeProjectsConfirm", &[("count", &count), ("titles", &names)]))
        }
    };
    let ids: Vec<String> = workspaces.iter().map(|workspace| workspace.workspace_id.clone()).collect();
    window.open_alert_dialog(cx, move |alert, _, cx| {
        let (ids, after) = (ids.clone(), after.clone());
        alert
            .title(title.clone())
            .description(description.clone())
            .ok_text(tr(cx, "ide.ideConfirm"))
            .cancel_text(tr(cx, "ide.ideCancel"))
            .show_cancel(true)
            .on_ok(move |_, window, cx| {
                for id in &ids {
                    let id = id.clone();
                    Ide::global(cx).update(cx, |ide, cx| ide.remove_workspace(id, window, cx));
                }
                after(cx);
                true
            })
    });
}

impl Render for ProjectManager {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (border, muted_foreground) = {
            let theme = cx.theme();
            (theme.border, theme.muted_foreground)
        };
        let (workspaces, current) = {
            let ide = Ide::global(cx).read(cx);
            (ide.workspaces.clone(), ide.workspace.as_ref().map(|workspace| workspace.workspace_id.clone()))
        };
        let summaries = Store::global(cx).read(cx).summaries.clone();
        // Chats still in the list and archived ones, and the latest activity, per project.
        let mut counts: HashMap<&str, (usize, usize)> = HashMap::new();
        let mut latest: HashMap<&str, f64> = HashMap::new();
        for summary in &summaries {
            let Some(id) = summary.workspace_id.as_deref() else { continue };
            let count = counts.entry(id).or_default();
            if summary.archived {
                count.1 += 1;
            } else {
                count.0 += 1;
            }
            let newest = latest.entry(id).or_insert(summary.updated_at);
            *newest = newest.max(summary.updated_at);
        }
        let words = words_of(&self.search, cx);
        let mut shown: Vec<&IdeWorkspace> = workspaces
            .iter()
            .filter(|workspace| {
                let (title, path) = (workspace.title.to_lowercase(), workspace.path.to_lowercase());
                words.iter().all(|word| title.contains(word.as_str()) || path.contains(word.as_str()))
            })
            .collect();
        // As the sidebar lists them: pinned first, then by their latest chat.
        shown.sort_by(|a, b| {
            let newest = |workspace: &IdeWorkspace| latest.get(workspace.workspace_id.as_str()).copied().unwrap_or(0.0);
            b.pinned.cmp(&a.pinned).then(newest(b).total_cmp(&newest(a)))
        });
        let shown_ids: HashSet<&str> = shown.iter().map(|workspace| workspace.workspace_id.as_str()).collect();
        self.chosen.retain(|id| shown_ids.contains(id.as_str()));
        let chosen: Vec<IdeWorkspace> = shown.iter().filter(|workspace| self.chosen.contains(&workspace.workspace_id)).map(|workspace| (*workspace).clone()).collect();
        let chat_ids: Vec<String> = summaries
            .iter()
            .filter(|summary| !summary.archived && summary.workspace_id.as_ref().is_some_and(|id| self.chosen.contains(id)))
            .map(|summary| summary.id.clone())
            .collect();
        let every: Vec<String> = shown.iter().map(|workspace| workspace.workspace_id.clone()).collect();
        let all_chosen = !shown.is_empty() && chosen.len() == shown.len();
        let none = chosen.is_empty();
        let (pin_chosen, unpin_chosen, remove_chosen) = (chosen.clone(), chosen.clone(), chosen.clone());
        let weak = cx.entity().downgrade();
        let bar = h_flex()
            .w_full()
            .h(px(32.))
            .gap_2()
            .items_center()
            .child(
                Checkbox::new("projects-all")
                    .checked(all_chosen)
                    .disabled(shown.is_empty())
                    .label(tr(cx, "agent.selectAll"))
                    .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                        this.chosen = if *checked { every.iter().cloned().collect() } else { HashSet::new() };
                        cx.notify();
                    })),
            )
            .child(div().flex_1().min_w_0().text_xs().text_color(muted_foreground).when(!none, |this| this.child(trf(cx, "agent.selectedCount", &[("count", &chosen.len())]))))
            .child(
                Button::new("projects-pin")
                    .outline()
                    .xsmall()
                    .icon(Icon::new(gpui_kit::assets::IconName::Pin))
                    .label(tr(cx, "chat.pin"))
                    .disabled(chosen.iter().all(|workspace| workspace.pinned))
                    .on_click(move |_, window, cx| pin_projects(&pin_chosen, true, window, cx)),
            )
            .child(
                Button::new("projects-unpin")
                    .outline()
                    .xsmall()
                    .icon(Icon::new(gpui_kit::assets::IconName::PinOff))
                    .label(tr(cx, "chat.unpin"))
                    .disabled(chosen.iter().all(|workspace| !workspace.pinned))
                    .on_click(move |_, window, cx| pin_projects(&unpin_chosen, false, window, cx)),
            )
            .child(
                Button::new("projects-archive")
                    .outline()
                    .xsmall()
                    .icon(Icon::new(gpui_kit::assets::IconName::Archive))
                    .label(tr(cx, "agent.archiveChats"))
                    .tooltip(tr(cx, "agent.archiveChatsTip"))
                    .disabled(chat_ids.is_empty())
                    .on_click(move |_, window, cx| bulk("archive", chat_ids.clone(), window, cx)),
            )
            .child(
                Button::new("projects-remove")
                    .outline()
                    .xsmall()
                    .icon(Icon::new(IconName::Close))
                    .label(tr(cx, "agent.removeProject"))
                    .disabled(none)
                    .on_click(move |_, window, cx| {
                        let weak = weak.clone();
                        confirm_remove_projects(
                            remove_chosen.clone(),
                            move |cx| {
                                let _ = weak.update(cx, |this, cx| {
                                    this.chosen.clear();
                                    cx.notify();
                                });
                            },
                            window,
                            cx,
                        );
                    }),
            );
        let rows: Vec<AnyElement> = shown
            .iter()
            .map(|workspace| {
                let id = workspace.workspace_id.clone();
                let (live, archived) = counts.get(id.as_str()).copied().unwrap_or_default();
                let chats = if archived > 0 {
                    trf(cx, "agent.projectChatsArchived", &[("count", &live), ("archived", &archived)])
                } else {
                    trf(cx, "agent.projectChats", &[("count", &live)])
                };
                let is_current = current.as_deref() == Some(id.as_str());
                let (toggle, pin, rename, remove) = (id.clone(), (*workspace).clone(), (*workspace).clone(), (*workspace).clone());
                h_flex()
                    .w_full()
                    .py_1p5()
                    .gap_2()
                    .items_center()
                    .border_b_1()
                    .border_color(border)
                    .child(Checkbox::new(ElementId::Name(format!("project-check-{id}").into())).checked(self.chosen.contains(&id)).on_click(cx.listener(
                        move |this, checked: &bool, _, cx| {
                            if *checked {
                                this.chosen.insert(toggle.clone());
                            } else {
                                this.chosen.remove(&toggle);
                            }
                            cx.notify();
                        },
                    )))
                    .child(Icon::new(IconName::Folder).small().flex_shrink_0().text_color(muted_foreground))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(
                                h_flex()
                                    .gap_1()
                                    .items_center()
                                    .child(div().min_w_0().text_sm().overflow_hidden().text_ellipsis().whitespace_nowrap().when(is_current, |title| title.font_weight(FontWeight::MEDIUM)).child(workspace.title.clone()))
                                    .when(workspace.pinned, |this| this.child(Icon::new(gpui_kit::assets::IconName::Pin).xsmall().flex_shrink_0().text_color(muted_foreground))),
                            )
                            .child(div().text_xs().text_color(muted_foreground).overflow_hidden().text_ellipsis().whitespace_nowrap().child(workspace.path.clone())),
                    )
                    .child(div().flex_shrink_0().text_xs().text_color(muted_foreground).child(chats))
                    .child(
                        Button::new(ElementId::Name(format!("project-pin-{id}").into()))
                            .ghost()
                            .xsmall()
                            .icon(Icon::new(if workspace.pinned { gpui_kit::assets::IconName::PinOff } else { gpui_kit::assets::IconName::Pin }))
                            .tooltip(tr(cx, if workspace.pinned { "agent.unpinProject" } else { "agent.pinProject" }))
                            .on_click(move |_, window, cx| pin_projects(std::slice::from_ref(&pin), !pin.pinned, window, cx)),
                    )
                    .child(
                        Button::new(ElementId::Name(format!("project-rename-{id}").into()))
                            .ghost()
                            .xsmall()
                            .icon(Icon::new(gpui_kit::assets::IconName::FilePen))
                            .tooltip(tr(cx, "agent.renameProject"))
                            .on_click(move |_, window, cx| {
                                let id = rename.workspace_id.clone();
                                prompt_text(
                                    tr(cx, "agent.renameProject"),
                                    rename.title.clone(),
                                    tr(cx, "chat.save"),
                                    move |title, window, cx| {
                                        let request = json!({ "op": "workspaces.rename", "workspaceId": id, "title": title });
                                        Ide::global(cx).update(cx, |ide, cx| ide.change_workspace(request, window, cx));
                                    },
                                    window,
                                    cx,
                                );
                            }),
                    )
                    .child(
                        Button::new(ElementId::Name(format!("project-remove-{id}").into()))
                            .ghost()
                            .xsmall()
                            .icon(Icon::new(IconName::Close))
                            .tooltip(tr(cx, "agent.removeProject"))
                            .on_click(move |_, window, cx| crate::ui::agent::confirm_remove(Ide::global(cx), remove.workspace_id.clone(), remove.title.clone(), window, cx)),
                    )
                    .into_any_element()
            })
            .collect();
        let empty = rows.is_empty();
        let any = !workspaces.is_empty();
        v_flex()
            .w_full()
            .gap_2()
            .when(any, |this| this.child(search_field(&self.search, cx)).child(bar))
            .child(
                v_flex()
                    .id("project-list")
                    .w_full()
                    .max_h(px(400.))
                    .overflow_y_scroll()
                    .children(rows)
                    .when(empty, |this| this.child(div().py_4().text_sm().text_color(muted_foreground).child(tr(cx, if any { "chat.noResults" } else { "agent.noProjects" })))),
            )
    }
}

#[cfg(test)]
mod tests {
    use ::core::prelude::v1::test;

    use super::*;

    #[test]
    fn names_up_to_three_chats_then_says_there_are_more() {
        let names: Vec<SharedString> = ["甲", "乙", "丙", "丁"].into_iter().map(SharedString::from).collect();
        assert_eq!(quoted_in(&names[..2], true), "「甲」、「乙」");
        assert_eq!(quoted_in(&names, true), "「甲」、「乙」、「丙」等");
        assert_eq!(quoted_in(&names, false), "“甲”, “乙”, “丙” and more");
        let long = SharedString::from("a".repeat(30));
        assert_eq!(quoted_in(std::slice::from_ref(&long), false), format!("“{}…”", "a".repeat(24)));
    }
}
