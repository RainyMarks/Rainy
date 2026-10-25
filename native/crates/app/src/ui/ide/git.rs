//! Source control (`GitView.tsx`): each repository of the project with its staged and unstaged changes, commit,
//! and a commit message the chat model can write.

use std::collections::HashMap;
use std::time::Duration;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{InputEvent, Textarea, TextareaState};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _, StyledExt as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use rainy_protocol::{GitFileChange, GitRepository, GitStatus};
use serde_json::{Value, json};

use crate::store::ide::{Ide, IdeEvent};
use crate::store::{Store, StoreEvent};
use crate::ui::ide::file_icon;
use crate::ui::{tr, trf};

/// How often the status is read while the pane is shown, and while it is not (for the change count).
const FAST: Duration = Duration::from_secs(3);
const SLOW: Duration = Duration::from_secs(15);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Section {
    Staged,
    Unstaged,
}

pub struct GitView {
    ide: Entity<Ide>,
    status: Option<GitStatus>,
    error: Option<SharedString>,
    loading: bool,
    busy: bool,
    /// Roots whose commit message the model is writing.
    writing: Vec<String>,
    messages: HashMap<String, Entity<TextareaState>>,
    visible: bool,
    poll: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

fn kind_label(kind: &str) -> &'static str {
    match kind {
        "added" => "git.gitKindAdded",
        "deleted" => "git.gitKindDeleted",
        "renamed" => "git.gitKindRenamed",
        "copied" => "git.gitKindCopied",
        "untracked" => "git.gitKindUntracked",
        "conflicted" => "git.gitKindConflicted",
        _ => "git.gitKindModified",
    }
}

fn kind_letter(kind: &str) -> &'static str {
    match kind {
        "added" => "A",
        "deleted" => "D",
        "renamed" => "R",
        "copied" => "C",
        "untracked" => "U",
        "conflicted" => "!",
        _ => "M",
    }
}

/// The editor key of a repository file.
fn key_of(root_id: &str, path: &str) -> String {
    if root_id == "primary" || root_id.is_empty() { path.to_owned() } else { format!("\0{root_id}/{path}") }
}

impl GitView {
    pub fn new(ide: Entity<Ide>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let store = Store::global(cx);
        let subscriptions = vec![
            cx.subscribe_in(&ide, window, |this, _, event: &IdeEvent, window, cx| match event {
                IdeEvent::WorkspaceChanged => {
                    this.status = None;
                    this.error = None;
                    this.messages.clear();
                    this.refresh(window, cx);
                }
                IdeEvent::Saved => this.refresh_soon(window, cx),
                _ => {}
            }),
            cx.subscribe_in(&store, window, |this, _, event: &StoreEvent, window, cx| match event {
                StoreEvent::Host { name, data } => {
                    let files = name == "ide.hint" && data.get("kind").and_then(Value::as_str) == Some("files");
                    if files || name == "session.changes" {
                        this.refresh_soon(window, cx);
                    }
                }
                // The interface language may have changed.
                StoreEvent::PrefsChanged => {
                    let placeholder = tr(cx, "git.gitMessagePlaceholder");
                    for input in this.messages.values() {
                        input.update(cx, |state, cx| state.set_placeholder(placeholder.clone(), window, cx));
                    }
                }
                _ => {}
            }),
        ];
        let mut view = Self {
            ide,
            status: None,
            error: None,
            loading: false,
            busy: false,
            writing: Vec::new(),
            messages: HashMap::new(),
            visible: false,
            poll: None,
            _subscriptions: subscriptions,
        };
        view.refresh(window, cx);
        view
    }

    /// Changed files across the project's repositories, for the badge on the pane's tab.
    pub fn change_count(&self) -> usize {
        self.status.as_ref().map_or(0, |status| status.repositories.iter().map(|repository| repository.files.len()).sum())
    }

    /// The first repository's branch, or its short detached head, or None while the project is not a Git repository (3.x status bar).
    pub fn branch(&self) -> Option<Option<String>> {
        let repository = self.status.as_ref()?.repositories.first()?;
        Some(repository.branch.clone().or_else(|| repository.head.as_ref().map(|head| head.chars().take(7).collect())))
    }

    pub fn set_visible(&mut self, visible: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.visible != visible {
            self.visible = visible;
            if visible {
                self.refresh(window, cx);
            }
        }
    }

    fn workspace_id(&self, cx: &App) -> Option<String> {
        self.ide.read(cx).workspace.as_ref().map(|workspace| workspace.workspace_id.clone())
    }

    fn refresh_soon(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.poll = Some(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(400)).await;
            let _ = this.update_in(cx, |this, window, cx| this.refresh(window, cx));
        }));
    }

    /// Read the status now, then again after a while.
    pub fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(workspace) = self.workspace_id(cx) else {
            self.poll = None;
            return;
        };
        let call = Store::global(cx).read(cx).call("git.status", json!({ "workspaceId": workspace }));
        self.loading = self.status.is_none();
        self.poll = Some(cx.spawn_in(window, async move |this, cx| {
            let result = call.await;
            let delay = this
                .update(cx, |this, cx| {
                    this.loading = false;
                    if this.workspace_id(cx).as_deref() == Some(workspace.as_str()) {
                        match result.map(serde_json::from_value::<GitStatus>) {
                            Ok(Ok(status)) => {
                                if this.status.as_ref() != Some(&status) || this.error.is_some() {
                                    this.status = Some(status);
                                    this.error = None;
                                    cx.notify();
                                }
                            }
                            Ok(Err(error)) => {
                                this.error = Some(error.to_string().into());
                                cx.notify();
                            }
                            Err(error) => {
                                this.error = Some(crate::ui::error_text(cx, &error));
                                cx.notify();
                            }
                        }
                    }
                    if this.visible { FAST } else { SLOW }
                })
                .unwrap_or(SLOW);
            cx.background_executor().timer(delay).await;
            let _ = this.update_in(cx, |this, window, cx| this.refresh(window, cx));
        }));
    }

    /// Run one Git operation, then read the status again.
    fn operate(&mut self, method: &'static str, root_id: String, mut params: Value, window: &mut Window, cx: &mut Context<Self>) {
        let Some(workspace) = self.workspace_id(cx) else { return };
        params["workspaceId"] = json!(workspace);
        params["rootId"] = json!(root_id);
        let call = Store::global(cx).read(cx).call(method, params);
        self.busy = true;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = call.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                match result {
                    Ok(value) => {
                        if method == "git.commit" {
                            if let Some(message) = this.messages.get(&root_id) {
                                message.update(cx, |state, cx| state.set_value("", window, cx));
                            }
                            let commit = value["commit"].as_str().unwrap_or_default().chars().take(7).collect::<String>();
                            window.push_notification(trf(cx, "git.gitCommitted", &[("commit", &commit)]).to_string(), cx);
                        }
                        if method == "git.discard" {
                            this.ide.update(cx, |ide, cx| ide.check_files(cx));
                        }
                    }
                    Err(error) => window.push_notification(crate::ui::error_notification(cx, &error), cx),
                }
                this.refresh(window, cx);
            });
        })
        .detach();
    }

    fn paths(changes: &[GitFileChange]) -> Value {
        json!(changes.iter().map(|change| change.path.clone()).collect::<Vec<_>>())
    }

    fn stage(&mut self, root_id: String, changes: Vec<GitFileChange>, stage: bool, window: &mut Window, cx: &mut Context<Self>) {
        if changes.is_empty() {
            return;
        }
        let method = if stage { "git.stage" } else { "git.unstage" };
        self.operate(method, root_id, json!({ "paths": Self::paths(&changes) }), window, cx);
    }

    fn confirm_discard(&mut self, root_id: String, changes: Vec<GitFileChange>, window: &mut Window, cx: &mut Context<Self>) {
        if changes.is_empty() {
            return;
        }
        let view = cx.entity();
        let count = changes.len();
        window.open_alert_dialog(cx, move |alert, _, cx| {
            let (view, root_id, changes) = (view.clone(), root_id.clone(), changes.clone());
            alert
                .title(tr(cx, "git.gitDiscardTitle"))
                .description(trf(cx, "git.gitDiscardBody", &[("count", &count)]))
                .ok_text(tr(cx, "git.gitDiscard"))
                .cancel_text(tr(cx, "git.cancel"))
                .show_cancel(true)
                .on_ok(move |_, window, cx| {
                    let params = json!({ "paths": Self::paths(&changes) });
                    let root_id = root_id.clone();
                    view.update(cx, |view, cx| view.operate("git.discard", root_id, params, window, cx));
                    true
                })
        });
    }

    fn commit(&mut self, repository: &GitRepository, window: &mut Window, cx: &mut Context<Self>) {
        let Some(message) = self.messages.get(&repository.root_id) else { return };
        let text = message.read(cx).value().trim().to_owned();
        let staged = repository.files.iter().any(|file| file.staged.is_some());
        if text.is_empty() || self.busy || repository.files.is_empty() {
            return;
        }
        self.operate("git.commit", repository.root_id.clone(), json!({ "message": text, "stageAll": !staged }), window, cx);
    }

    /// Have the chat model write the message (`git.message`).
    fn generate(&mut self, root_id: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(workspace) = self.workspace_id(cx) else { return };
        if self.writing.contains(&root_id) {
            return;
        }
        let call = Store::global(cx).read(cx).call("git.message", json!({ "workspaceId": workspace, "rootId": root_id }));
        self.writing.push(root_id.clone());
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = call.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.writing.retain(|root| *root != root_id);
                match result {
                    Ok(value) => {
                        let text = value["message"].as_str().unwrap_or_default().to_owned();
                        if let Some(message) = this.messages.get(&root_id) {
                            message.update(cx, |state, cx| state.set_value(text, window, cx));
                        }
                    }
                    Err(error) => window.push_notification(crate::ui::error_notification(cx, &error), cx),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn message_input(&mut self, root_id: &str, window: &mut Window, cx: &mut Context<Self>) -> Entity<TextareaState> {
        if let Some(input) = self.messages.get(root_id) {
            return input.clone();
        }
        let input = cx.new(|cx| TextareaState::new(window, cx).auto_grow(3, 8).placeholder(tr(cx, "git.gitMessagePlaceholder")));
        let root = root_id.to_owned();
        cx.subscribe_in(&input, window, move |this, _, event: &InputEvent, window, cx| {
            if let InputEvent::PressEnter { secondary: true, .. } = event
                && let Some(repository) =
                    this.status.as_ref().and_then(|status| status.repositories.iter().find(|repository| repository.root_id == root)).cloned()
            {
                this.commit(&repository, window, cx);
            }
        })
        .detach();
        self.messages.insert(root_id.to_owned(), input.clone());
        input
    }

    fn render_row(&self, root_id: &str, change: &GitFileChange, section: Section, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let kind = match section {
            Section::Staged => change.staged.clone(),
            Section::Unstaged => change.unstaged.clone(),
        }
        .unwrap_or_default();
        let (name, directory) = match change.path.rsplit_once('/') {
            Some((directory, name)) => (name.to_owned(), directory.to_owned()),
            None => (change.path.clone(), String::new()),
        };
        let color = match kind.as_str() {
            "added" | "untracked" => theme.success,
            "deleted" | "conflicted" => theme.danger,
            _ => crate::theme::warning_text(cx),
        };
        let key = key_of(root_id, &change.path);
        let id = SharedString::from(format!("git-{}-{}-{index}", if section == Section::Staged { "s" } else { "u" }, root_id));
        let (open_key, show_key) = (key.clone(), key);
        let (ide_open, ide_show) = (self.ide.clone(), self.ide.clone());
        let deleted = kind == "deleted";
        let new_file = kind == "untracked" || kind == "added";
        let (stage_root, discard_root) = (root_id.to_owned(), root_id.to_owned());
        let (stage_change, discard_change) = (change.clone(), change.clone());
        let (tip_path, tip_from, tip_kind) = (change.path.clone(), change.from.clone(), kind_label(&kind));
        let tooltip = move |window: &mut Window, cx: &mut App| {
            let text = match &tip_from {
                Some(from) => format!("{tip_path} · {}", trf(cx, "git.gitRenamedFrom", &[("from", from)])),
                None => format!("{tip_path} · {}", tr(cx, tip_kind)),
            };
            Tooltip::new(text).build(window, cx)
        };
        h_flex()
            .id(id.clone())
            .group(id.clone())
            .h(px(26.))
            .pl(px(20.))
            .pr_1()
            .gap_1p5()
            .items_center()
            .text_sm()
            .cursor_pointer()
            .hover(|style| style.bg(theme.list_hover))
            .tooltip(tooltip)
            .child(Icon::new(file_icon(&name)).small().flex_shrink_0())
            // A long name gives way before the buttons and the status letter do.
            .child(div().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().when(deleted, |this| this.line_through()).child(name))
            .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().text_xs().text_color(theme.muted_foreground).child(directory))
            .child(
                h_flex()
                    .flex_shrink_0()
                    .gap_0p5()
                    .invisible()
                    .group_hover(id, |style| style.visible())
                    .when(!deleted, |this| {
                        this.child(Button::new("open").ghost().xsmall().icon(IconName::ExternalLink).tooltip(tr(cx, "git.gitOpenFile")).on_click(move |_, window, cx| {
                            let key = open_key.clone();
                            ide_open.update(cx, |ide, cx| ide.show_file(key, window, cx));
                        }))
                    })
                    .when(section == Section::Unstaged, |this| {
                        this.child(Button::new("discard").ghost().xsmall().icon(IconName::Undo2).tooltip(tr(cx, "git.gitDiscard")).disabled(self.busy).on_click(
                            cx.listener(move |this, _, window, cx| this.confirm_discard(discard_root.clone(), vec![discard_change.clone()], window, cx)),
                        ))
                    })
                    .child(
                        Button::new("stage")
                            .ghost()
                            .xsmall()
                            .icon(if section == Section::Unstaged { IconName::Plus } else { IconName::Minus })
                            .tooltip(tr(cx, if section == Section::Unstaged { "git.gitStage" } else { "git.gitUnstage" }))
                            .disabled(self.busy)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.stage(stage_root.clone(), vec![stage_change.clone()], section == Section::Unstaged, window, cx)
                            })),
                    ),
            )
            .child(div().w(px(14.)).flex_shrink_0().text_xs().font_semibold().text_color(color).child(kind_letter(&kind)))
            .on_click(move |_, window, cx| {
                // A deleted file has nothing to open; a new one has nothing to compare with.
                if deleted {
                    return;
                }
                let key = show_key.clone();
                ide_show.update(cx, |ide, cx| if new_file { ide.show_file(key, window, cx) } else { ide.open_diff(key, window, cx) });
            })
            .into_any_element()
    }

    fn render_repository(&mut self, repository: &GitRepository, titled: bool, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let root_id = repository.root_id.clone();
        let mut staged: Vec<GitFileChange> = repository.files.iter().filter(|file| file.staged.is_some()).cloned().collect();
        let mut unstaged: Vec<GitFileChange> = repository.files.iter().filter(|file| file.unstaged.is_some()).cloned().collect();
        staged.sort_by(|a, b| a.path.cmp(&b.path));
        unstaged.sort_by(|a, b| a.path.cmp(&b.path));
        let stage_all = staged.is_empty() && !unstaged.is_empty();
        let branch: SharedString = match (&repository.branch, &repository.head) {
            (Some(branch), _) => branch.clone().into(),
            (None, None) => tr(cx, "git.gitNoCommits"),
            (None, Some(head)) => format!("{} {}", tr(cx, "git.gitDetached"), head.chars().take(7).collect::<String>()).into(),
        };
        let message = self.message_input(&root_id, window, cx);
        let writing = self.writing.contains(&root_id);
        let nothing = staged.is_empty() && unstaged.is_empty();
        let commit_repository = repository.clone();
        let (generate_root, unstage_root, discard_root, stage_root) = (root_id.clone(), root_id.clone(), root_id.clone(), root_id.clone());
        let (unstage_all, discard_all, stage_all_changes) = (staged.clone(), unstaged.clone(), unstaged.clone());
        let staged_rows: Vec<AnyElement> = staged.iter().enumerate().map(|(index, change)| self.render_row(&root_id, change, Section::Staged, index, cx)).collect();
        let unstaged_rows: Vec<AnyElement> =
            unstaged.iter().enumerate().map(|(index, change)| self.render_row(&root_id, change, Section::Unstaged, index, cx)).collect();
        let section = |title: SharedString, count: usize, actions: Vec<AnyElement>| {
            h_flex()
                .h(px(28.))
                .px_2()
                .gap_1()
                .items_center()
                .text_xs()
                .font_semibold()
                .text_color(theme.muted_foreground)
                .child(title)
                .child(div().px_1p5().rounded_full().bg(theme.muted).child(count.to_string()))
                .child(div().flex_1())
                .children(actions)
        };
        v_flex()
            .gap_1()
            .pb_2()
            .child(
                h_flex()
                    .px_2()
                    .pt_2()
                    .gap_1p5()
                    .items_center()
                    .text_sm()
                    .when(titled, |this| this.child(div().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().font_semibold().child(repository.title.clone())))
                    .child(Icon::new(gpui_kit::assets::IconName::GitBranch).small().flex_shrink_0().text_color(theme.muted_foreground))
                    .child(div().overflow_hidden().text_ellipsis().whitespace_nowrap().child(branch))
                    .when(repository.ahead > 0, |this| this.child(div().text_xs().text_color(theme.muted_foreground).child(format!("↑{}", repository.ahead))))
                    .when(repository.behind > 0, |this| this.child(div().text_xs().text_color(theme.muted_foreground).child(format!("↓{}", repository.behind)))),
            )
            .child(div().px_2().child(Textarea::new(&message)))
            .child(
                h_flex()
                    .px_2()
                    .gap_1()
                    .child(
                        Button::new(SharedString::from(format!("commit-{root_id}")))
                            .primary()
                            .small()
                            .flex_1()
                            .label(tr(cx, if stage_all { "git.gitCommitAll" } else { "git.gitCommit" }))
                            .disabled(self.busy || nothing)
                            .on_click(cx.listener(move |this, _, window, cx| this.commit(&commit_repository, window, cx))),
                    )
                    .child(
                        Button::new(SharedString::from(format!("generate-{root_id}")))
                            .outline()
                            .small()
                            .icon(Icon::new(gpui_kit::assets::IconName::Sparkles))
                            .loading(writing)
                            .tooltip(tr(cx, if writing { "git.gitGenerating" } else { "git.gitGenerate" }))
                            .disabled(writing || nothing)
                            .on_click(cx.listener(move |this, _, window, cx| this.generate(generate_root.clone(), window, cx))),
                    ),
            )
            .when(repository.truncated, |this| this.child(div().px_2().text_xs().text_color(crate::theme::warning_text(cx)).child(tr(cx, "git.gitTruncated"))))
            .when(!staged.is_empty(), |this| {
                this.child(section(
                    tr(cx, "git.gitStaged"),
                    staged.len(),
                    vec![
                        Button::new("unstage-all")
                            .ghost()
                            .xsmall()
                            .icon(IconName::Minus)
                            .tooltip(tr(cx, "git.gitUnstageAll"))
                            .disabled(self.busy)
                            .on_click(cx.listener(move |this, _, window, cx| this.stage(unstage_root.clone(), unstage_all.clone(), false, window, cx)))
                            .into_any_element(),
                    ],
                ))
                .children(staged_rows)
            })
            .when(!unstaged.is_empty(), |this| {
                this.child(section(
                    tr(cx, "git.gitChanges"),
                    unstaged.len(),
                    vec![
                        Button::new("discard-all")
                            .ghost()
                            .xsmall()
                            .icon(IconName::Undo2)
                            .tooltip(tr(cx, "git.gitDiscardAll"))
                            .disabled(self.busy)
                            .on_click(cx.listener(move |this, _, window, cx| this.confirm_discard(discard_root.clone(), discard_all.clone(), window, cx)))
                            .into_any_element(),
                        Button::new("stage-all")
                            .ghost()
                            .xsmall()
                            .icon(IconName::Plus)
                            .tooltip(tr(cx, "git.gitStageAll"))
                            .disabled(self.busy)
                            .on_click(cx.listener(move |this, _, window, cx| this.stage(stage_root.clone(), stage_all_changes.clone(), true, window, cx)))
                            .into_any_element(),
                    ],
                ))
                .children(unstaged_rows)
            })
            .when(nothing, |this| this.child(div().px_2().py_1().text_sm().text_color(theme.muted_foreground).child(tr(cx, "git.gitNoChanges"))))
            .into_any_element()
    }
}

impl Render for GitView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let has_workspace = self.ide.read(cx).workspace.is_some();
        let status = self.status.clone();
        let titled = status.as_ref().is_some_and(|status| status.repositories.len() + status.plain.len() > 1);
        let repositories: Vec<AnyElement> = status
            .iter()
            .flat_map(|status| status.repositories.iter())
            .map(|repository| self.render_repository(repository, titled, window, cx))
            .collect();
        let plain: Vec<AnyElement> = status
            .iter()
            .flat_map(|status| status.plain.iter())
            .enumerate()
            .map(|(index, root)| {
                let root_id = root.root_id.clone();
                v_flex()
                    .p_2()
                    .gap_2()
                    .items_start()
                    .child(div().text_sm().text_color(theme.muted_foreground).child(trf(cx, "git.gitNotRepository", &[("title", &root.title)])))
                    .child(
                        Button::new(("git-init", index))
                            .outline()
                            .small()
                            .label(tr(cx, "git.gitInit"))
                            .disabled(self.busy)
                            .on_click(cx.listener(move |this, _, window, cx| this.operate("git.init", root_id.clone(), json!({}), window, cx))),
                    )
                    .into_any_element()
            })
            .collect();
        let unavailable = status.as_ref().is_some_and(|status| !status.available);
        v_flex()
            .id("git")
            .size_full()
            .overflow_y_scrollbar()
            .when(!has_workspace, |this| this.child(div().p_3().text_sm().text_color(theme.muted_foreground).child(tr(cx, "git.gitNoWorkspace"))))
            .when(has_workspace && self.loading, |this| this.child(h_flex().p_3().child(Spinner::new().small())))
            .when_some(self.error.clone(), |this, error| this.child(div().p_3().text_sm().text_color(theme.danger).child(error)))
            .when(unavailable, |this| this.child(div().p_3().text_sm().text_color(theme.muted_foreground).child(tr(cx, "git.gitMissing"))))
            .children(repositories)
            .children(plain)
    }
}
