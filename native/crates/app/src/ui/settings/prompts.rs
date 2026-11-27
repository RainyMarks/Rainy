//! Prompts: what each kind of prompt is for, Rainy's built-in prompt of each model group, the global prompt, and the
//! instruction files (`AGENTS.md` and its kin) of the Host and of the current project.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Textarea, TextareaState};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::tag::Tag;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, StyledExt as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use rainy_protocol::ModelsStatus;
use serde::Deserialize;
use serde_json::{Value, json};

use super::models_extra::notice;
use super::widgets::{AsyncPageGuard, bind_textarea_change};
use super::{heading, segmented};
use crate::store::ide::{Ide, IdeEvent};
use crate::store::{Store, StoreEvent};
use crate::ui::{tr, trf};

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct Group {
    id: String,
    /// The user's text; empty while Rainy's is used.
    custom: String,
    default_text: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct Builtin {
    max_chars: usize,
    groups: Vec<Group>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct InstructionFile {
    path: String,
    label: String,
    global: bool,
    exists: bool,
    #[serde(default)]
    bytes: Option<u64>,
    /// Left out when the file is over the budget.
    #[serde(default)]
    text: Option<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct Instructions {
    budget_bytes: u64,
    files: Vec<InstructionFile>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
struct Status {
    builtin: Builtin,
    instructions: Instructions,
}

/// The model groups, as the Host names them, and their labels.
const GROUPS: [(&str, &str); 3] = [("claude", "settings.promptsGroupClaude"), ("gpt", "settings.promptsGroupGpt"), ("gemini", "settings.promptsGroupGemini")];

/// A size in bytes as the page shows it.
fn size_text(bytes: u64) -> String {
    if bytes < 1024 { format!("{bytes} B") } else { format!("{:.1} KB", bytes as f64 / 1024.) }
}

pub struct PromptsPage {
    store: Entity<Store>,
    workspace: Option<String>,
    status: Option<Status>,
    error: Option<SharedString>,
    guard: AsyncPageGuard,
    /// The model group whose built-in prompt is shown.
    group: &'static str,
    builtin: Entity<TextareaState>,
    global: Entity<TextareaState>,
    /// The global prompt field shows the saved prompt.
    global_loaded: bool,
    /// The instruction file shown (its path) and its field.
    file: Option<String>,
    file_field: Entity<TextareaState>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<super::CloseSettings> for PromptsPage {}

impl PromptsPage {
    pub fn new(store: Entity<Store>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let global_placeholder = tr(cx, "settings.settingsGlobalPromptPlaceholder");
        let file_placeholder = tr(cx, "settings.promptsFilePlaceholder");
        let builtin = cx.new(|cx| TextareaState::new(window, cx).auto_grow(8, 22));
        let global = cx.new(|cx| TextareaState::new(window, cx).auto_grow(3, 10).placeholder(global_placeholder));
        let file_field = cx.new(|cx| TextareaState::new(window, cx).auto_grow(6, 20).placeholder(file_placeholder));
        let ide = Ide::global(cx);
        let subscriptions = vec![
            bind_textarea_change(&builtin, window, cx),
            bind_textarea_change(&global, window, cx),
            bind_textarea_change(&file_field, window, cx),
            cx.subscribe_in(&ide, window, |this, _, event: &IdeEvent, window, cx| {
                if *event == IdeEvent::WorkspaceChanged {
                    this.guard.invalidate();
                    this.reload(window, cx);
                }
            }),
            cx.subscribe_in(&store, window, |this, _, event: &StoreEvent, window, cx| match event {
                StoreEvent::PrefsChanged => {
                    let global_placeholder = tr(cx, "settings.settingsGlobalPromptPlaceholder");
                    let file_placeholder = tr(cx, "settings.promptsFilePlaceholder");
                    this.global.update(cx, |state, cx| state.set_placeholder(global_placeholder, window, cx));
                    this.file_field.update(cx, |state, cx| state.set_placeholder(file_placeholder, window, cx));
                    this.load_global(window, cx);
                    cx.notify();
                }
                StoreEvent::HostReconnected(_) => {
                    this.guard.invalidate();
                    this.global_loaded = false;
                    this.load_global(window, cx);
                    this.reload(window, cx);
                }
                StoreEvent::Host { name, .. } if name == "models.changed" => {
                    this.load_global(window, cx);
                    cx.notify();
                }
                _ => {}
            }),
            cx.observe_in(&store, window, |this, store, window, cx| {
                if !this.global_loaded && store.read(cx).models.is_some() {
                    this.load_global(window, cx);
                }
            }),
        ];
        let mut page = Self {
            store,
            workspace: None,
            status: None,
            error: None,
            guard: AsyncPageGuard::default(),
            group: "claude",
            builtin,
            global,
            global_loaded: false,
            file: None,
            file_field,
            _subscriptions: subscriptions,
        };
        page.load_global(window, cx);
        page.reload(window, cx);
        page
    }

    /// The first time the models are known, the global prompt field shows the saved prompt.
    fn load_global(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.global_loaded {
            return;
        }
        let Some(text) = self.store.read(cx).models.as_ref().and_then(|models| models.global_prompt.as_ref()).map(|prompt| prompt.text.clone()) else { return };
        self.global_loaded = true;
        self.global.update(cx, |state, cx| state.set_value(text, window, cx));
    }

    fn reload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.workspace = Ide::global(cx).read(cx).workspace.as_ref().map(|workspace| workspace.workspace_id.clone());
        let call = self.store.read(cx).call("prompts.status", json!({ "workspaceId": self.workspace }));
        let workspace = self.workspace.clone();
        let generation = self.guard.begin_read();
        cx.spawn_in(window, async move |this, cx| {
            let result = call.await;
            let _ = this.update_in(cx, |this, window, cx| {
                let current = this.guard.end_read(generation);
                if !current || this.workspace != workspace {
                    cx.notify();
                    return;
                }
                match result.map(serde_json::from_value::<Status>) {
                    Ok(Ok(status)) => {
                        let first = this.status.is_none();
                        this.status = Some(status);
                        this.error = None;
                        if first {
                            this.show_group(this.group, window, cx);
                        }
                        this.show_file(this.file.clone(), window, cx);
                    }
                    Ok(Err(error)) => this.error = Some(error.to_string().into()),
                    Err(error) => this.error = Some(crate::ui::error_text(cx, &error)),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn group_status(&self, group: &str) -> Option<&Group> {
        self.status.as_ref()?.builtin.groups.iter().find(|item| item.id == group)
    }

    /// Show a group's built-in prompt: the user's, else Rainy's.
    fn show_group(&mut self, group: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        self.group = group;
        let text = self.group_status(group).map(|item| if item.custom.is_empty() { item.default_text.clone() } else { item.custom.clone() }).unwrap_or_default();
        self.builtin.update(cx, |state, cx| state.set_value(text, window, cx));
        cx.notify();
    }

    /// Show an instruction file, by path; without one (or one no longer listed), the project's `AGENTS.md`, else the
    /// Host's.
    fn show_file(&mut self, path: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(status) = &self.status else { return };
        let files = &status.instructions.files;
        let chosen = path
            .and_then(|path| files.iter().find(|file| file.path == path))
            .or_else(|| files.iter().find(|file| !file.global && file.exists))
            .or_else(|| files.iter().find(|file| !file.global && file.label.ends_with("AGENTS.md")))
            .or_else(|| files.first())
            .cloned();
        self.file = chosen.as_ref().map(|file| file.path.clone());
        let text = chosen.and_then(|file| file.text).unwrap_or_default();
        self.file_field.update(cx, |state, cx| state.set_value(text, window, cx));
        cx.notify();
    }

    fn current_file(&self) -> Option<&InstructionFile> {
        let path = self.file.as_deref()?;
        self.status.as_ref()?.instructions.files.iter().find(|file| file.path == path)
    }

    /// Run a save; `done` takes its answer, and `success` says it worked.
    fn save(
        &mut self, method: &str, params: Value, success: &'static str, done: impl FnOnce(&mut Self, Value, &mut Window, &mut Context<Self>) + 'static, window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let call = self.store.read(cx).call(method, params);
        let generation = self.guard.begin_write();
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = call.await;
            let _ = this.update_in(cx, |this, window, cx| {
                let current = this.guard.end_write(generation);
                if !current {
                    cx.notify();
                    return;
                }
                match result {
                    Ok(value) => {
                        done(this, value, window, cx);
                        window.push_notification(tr(cx, success).to_string(), cx);
                    }
                    Err(error) => window.push_notification(crate::ui::error_notification(cx, &error), cx),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn save_builtin(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        let group = self.group;
        let success = if text.trim().is_empty() { "settings.promptsRestored" } else { "settings.settingsSaved" };
        self.save("prompts.builtin", json!({ "group": group, "text": text }), success, move |this, value, window, cx| {
            if let (Ok(builtin), Some(status)) = (serde_json::from_value::<Builtin>(value), this.status.as_mut()) {
                status.builtin = builtin;
            }
            this.show_group(group, window, cx);
        }, window, cx);
    }

    /// What the kinds of prompt are for, side by side.
    fn render_kinds(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let kind = |title: &str, body: &str, cx: &App| {
            v_flex()
                .gap_1()
                .px_4()
                .py_3()
                .child(div().text_sm().font_semibold().child(tr(cx, title)))
                .child(div().text_xs().text_color(theme.muted_foreground).child(tr(cx, body)))
        };
        let rows = [
            ("settings.promptsKindBuiltin", "settings.promptsKindBuiltinNote"),
            ("settings.promptsKindGlobal", "settings.promptsKindGlobalNote"),
            ("settings.promptsKindFiles", "settings.promptsKindFilesNote"),
            ("settings.promptsKindMemory", "settings.promptsKindMemoryNote"),
        ];
        v_flex()
            .gap_2()
            .child(div().text_sm().font_semibold().child(tr(cx, "settings.promptsKinds")))
            .child(
                v_flex()
                    .rounded(theme.radius_lg)
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.group_box)
                    .children(rows.iter().enumerate().map(|(index, (title, body))| {
                        div().when(index > 0, |this| this.border_t_1().border_color(theme.border)).child(kind(title, body, cx))
                    })),
            )
            .child(div().text_xs().text_color(theme.muted_foreground).child(tr(cx, "settings.promptsExternal")))
            .into_any_element()
    }

    fn render_builtin(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let Some(status) = &self.status else { return h_flex().child(Spinner::new().small()).into_any_element() };
        let max = status.builtin.max_chars;
        let custom = self.group_status(self.group).is_some_and(|group| !group.custom.is_empty());
        let count = self.builtin.read(cx).value().chars().count();
        let items: Vec<(&'static str, SharedString)> = GROUPS.iter().map(|(id, key)| (*id, tr(cx, key))).collect();
        let change = cx.entity().downgrade();
        v_flex()
            .gap_2()
            .p_4()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.border)
            .bg(theme.group_box)
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(div().text_sm().font_semibold().child(tr(cx, "settings.promptsBuiltin")))
                    .child(if custom { Tag::primary().small().child(tr(cx, "settings.promptsCustom")) } else { Tag::secondary().small().child(tr(cx, "settings.promptsDefault")) }),
            )
            .child(div().text_xs().text_color(theme.muted_foreground).child(tr(cx, "settings.promptsBuiltinNote")))
            .child(segmented("prompts-group", items, self.group, window, cx, move |group, window, cx| {
                let _ = change.update(cx, |this, cx| this.show_group(group, window, cx));
            }))
            .child(div().text_xs().text_color(theme.muted_foreground).child(tr(cx, match self.group {
                "gpt" => "settings.promptsGroupGptNote",
                "gemini" => "settings.promptsGroupGeminiNote",
                _ => "settings.promptsGroupClaudeNote",
            })))
            .child(Textarea::new(&self.builtin))
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(div().flex_1().text_xs().text_color(if count > max { theme.danger } else { theme.muted_foreground }).child(trf(cx, "settings.settingsGlobalPromptCount", &[("count", &count), ("max", &max)])))
                    .child(Button::new("prompts-builtin-default").small().ghost().label(tr(cx, "settings.promptsRestore")).disabled(self.guard.busy() || !custom).on_click(cx.listener(|this, _, window, cx| {
                        this.save_builtin(String::new(), window, cx);
                    })))
                    .child(Button::new("prompts-builtin-save").small().outline().label(tr(cx, "chat.save")).disabled(self.guard.busy() || count > max || count == 0).on_click(cx.listener(|this, _, window, cx| {
                        let text = this.builtin.read(cx).value().to_string();
                        this.save_builtin(text, window, cx);
                    }))),
            )
            .child(div().text_xs().text_color(theme.muted_foreground).child(tr(cx, "settings.promptsBuiltinAfter")))
            .into_any_element()
    }

    fn render_global(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let max = self.store.read(cx).models.as_ref().and_then(|models| models.global_prompt.as_ref().map(|prompt| prompt.max_chars)).unwrap_or(4000);
        let count = self.global.read(cx).value().chars().count();
        v_flex()
            .gap_2()
            .p_4()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.border)
            .bg(theme.group_box)
            .child(div().text_sm().font_semibold().child(tr(cx, "settings.settingsGlobalPrompt")))
            .child(div().text_xs().text_color(theme.muted_foreground).child(tr(cx, "settings.settingsGlobalPromptNote")))
            .child(Textarea::new(&self.global))
            .child(
                h_flex()
                    .justify_between()
                    .items_center()
                    .child(div().text_xs().text_color(if count > max { theme.danger } else { theme.muted_foreground }).child(trf(cx, "settings.settingsGlobalPromptCount", &[("count", &count), ("max", &max)])))
                    .child(Button::new("prompts-global-save").small().outline().label(tr(cx, "chat.save")).disabled(self.guard.busy() || count > max).on_click(cx.listener(|this, _, window, cx| {
                        let text = this.global.read(cx).value().to_string();
                        this.save("prompt.global", json!({ "text": text }), "settings.settingsSaved", |this, value, _, cx| {
                            if let Ok(status) = serde_json::from_value::<ModelsStatus>(value) {
                                this.store.update(cx, |store, cx| {
                                    store.models = Some(status);
                                    cx.notify();
                                });
                            }
                        }, window, cx);
                    }))),
            )
            .into_any_element()
    }

    fn render_files(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let mut card = v_flex()
            .gap_2()
            .p_4()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.border)
            .bg(theme.group_box)
            .child(div().text_sm().font_semibold().child(tr(cx, "settings.promptsFiles")))
            .child(div().text_xs().text_color(theme.muted_foreground).child(tr(cx, "settings.promptsFilesNote")));
        let Some(status) = &self.status else { return card.child(h_flex().child(Spinner::new().small())).into_any_element() };
        if self.workspace.is_none() {
            card = card.child(div().text_xs().text_color(theme.muted_foreground).child(tr(cx, "settings.promptsFilesNoProject")));
        }
        let budget = status.instructions.budget_bytes;
        let used: u64 = status.instructions.files.iter().filter_map(|file| file.bytes).sum();
        card = card.child(
            v_flex().rounded(theme.radius).border_1().border_color(theme.border).children(status.instructions.files.iter().enumerate().map(|(index, file)| {
                let selected = self.file.as_deref() == Some(file.path.as_str());
                let path = file.path.clone();
                h_flex()
                    .id(ElementId::Name(format!("prompts-file-{index}").into()))
                    .px_3()
                    .py_1p5()
                    .gap_2()
                    .items_center()
                    .text_sm()
                    .cursor_pointer()
                    .when(index > 0, |this| this.border_t_1().border_color(theme.border))
                    .when(selected, |this| this.bg(theme.list_active))
                    .when(!selected, |this| this.hover(|style| style.bg(theme.list_hover)))
                    .on_click(cx.listener(move |this, _, window, cx| this.show_file(Some(path.clone()), window, cx)))
                    .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().font_family(theme.mono_font_family.clone()).child(file.label.clone()))
                    .child(div().flex_shrink_0().text_xs().text_color(theme.muted_foreground).child(if file.global { tr(cx, "settings.promptsFileGlobal") } else { tr(cx, "settings.promptsFileProject") }))
                    .child(match file.bytes {
                        Some(bytes) => div().flex_shrink_0().w(px(64.)).text_xs().text_color(theme.muted_foreground).child(size_text(bytes)).into_any_element(),
                        None => h_flex().flex_shrink_0().w(px(64.)).child(Tag::secondary().small().child(tr(cx, "settings.promptsFileMissing"))).into_any_element(),
                    })
            })),
        );
        card = card.child(div().text_xs().text_color(if used > budget { theme.danger } else { theme.muted_foreground }).child(trf(cx, "settings.promptsFilesBudget", &[("used", &size_text(used)), ("max", &size_text(budget))])));
        if let Some(file) = self.current_file().cloned() {
            let too_large = file.exists && file.text.is_none();
            card = card.child(div().pt_2().text_sm().font_family(theme.mono_font_family.clone()).child(file.path.clone()));
            if too_large {
                card = card.child(notice(cx, trf(cx, "settings.promptsFileTooLarge", &[("max", &size_text(budget))]), true));
            } else {
                card = card.child(Textarea::new(&self.file_field));
            }
            let bytes = self.file_field.read(cx).value().len() as u64;
            let editable = !file.global && file.exists && Ide::global(cx).read(cx).key_from_absolute(&file.path).is_some();
            let (open_path, save_path, expected) = (file.path.clone(), file.path.clone(), file.text.clone().filter(|_| file.exists));
            card = card.child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(div().flex_1().text_xs().text_color(if bytes > budget { theme.danger } else { theme.muted_foreground }).child(size_text(bytes)))
                    .child(Button::new("prompts-file-reload").small().ghost().label(tr(cx, "settings.promptsReload")).disabled(self.guard.busy()).on_click(cx.listener(|this, _, window, cx| this.reload(window, cx))))
                    .when(editable, |this| {
                        this.child(Button::new("prompts-file-open").small().ghost().label(tr(cx, "settings.promptsOpenInEditor")).on_click(cx.listener(move |_, _, window, cx| {
                            let ide = Ide::global(cx);
                            if let Some(key) = ide.read(cx).key_from_absolute(&open_path) {
                                ide.update(cx, |ide, cx| ide.open(key, true, window, cx));
                                crate::ui::agent::reveal_editor(cx);
                                cx.emit(super::CloseSettings);
                            }
                        })))
                    })
                    .child(Button::new("prompts-file-save").small().outline().label(tr(cx, "chat.save")).disabled(self.guard.busy() || too_large || bytes > budget).on_click(cx.listener(move |this, _, window, cx| {
                        let text = this.file_field.read(cx).value().to_string();
                        let params = json!({ "workspaceId": this.workspace, "path": save_path, "text": text, "expected": expected });
                        this.save("prompts.saveInstructions", params, "settings.settingsSaved", |this, _, window, cx| this.reload(window, cx), window, cx);
                    }))),
            );
        }
        card.into_any_element()
    }
}

impl Render for PromptsPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut page = v_flex().gap_4().child(heading(cx, "settings.navPrompts", "settings.promptsLead"));
        if let Some(error) = &self.error {
            page = page.child(notice(cx, error.clone(), true));
        }
        page.child(self.render_kinds(cx)).child(self.render_builtin(window, cx)).child(self.render_global(cx)).child(self.render_files(cx))
    }
}

#[cfg(test)]
mod tests {
    use ::core::prelude::v1::test;

    use super::*;

    #[test]
    fn reads_the_hosts_answer() {
        let status: Status = serde_json::from_value(json!({
            "builtin": { "maxChars": 20000, "groups": [{ "id": "claude", "custom": "", "defaultText": "You are Rainy" }] },
            "instructions": { "budgetBytes": 65536, "root": "/p", "files": [
                { "path": "/h/AGENTS.md", "label": "$RAINY_HOME/AGENTS.md", "global": true, "exists": false, "bytes": null, "text": null },
                { "path": "/p/AGENTS.md", "label": "AGENTS.md", "global": false, "exists": true, "bytes": 12, "text": "Run the tests" },
            ] },
        }))
        .unwrap();
        assert_eq!(status.builtin.groups[0].default_text, "You are Rainy");
        assert_eq!((status.instructions.files[1].bytes, status.instructions.files[1].text.as_deref()), (Some(12), Some("Run the tests")));
        assert_eq!((size_text(900), size_text(2048)), ("900 B".to_owned(), "2.0 KB".to_owned()));
    }
}
