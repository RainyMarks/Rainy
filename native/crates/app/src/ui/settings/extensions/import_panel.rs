//! Pasted/file MCP JSON import panel inside Settings → Skills & MCP.

use std::collections::{BTreeMap, HashSet};

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{InputEvent, Textarea, TextareaState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::tag::Tag;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, StyledExt as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use rainy_carrier::ExecutionTarget;
use rainy_carrier::mcp_json::{self, ExistingServer, ImportedServer, McpImport, McpImportEnvironment, McpImportMessage, McpServerConfig};
use serde_json::json;

use super::{ExtensionsPage, McpServer, Status};
use crate::store::Store;
use crate::ui::settings::models_extra::notice;
use crate::ui::{tr, trf};

/// What the import's empty text box shows.
const IMPORT_EXAMPLE: &str = "{\n  \"mcpServers\": {\n    \"github\": { \"command\": \"npx\", \"args\": [\"-y\", \"@modelcontextprotocol/server-github\"] }\n  }\n}";

/// A configured server as the import compares it.
fn configured(server: &McpServer) -> McpServerConfig {
    let pairs = |map: &Option<BTreeMap<String, String>>| map.as_ref().map(|map| map.iter().map(|(key, value)| (key.clone(), value.clone())).collect());
    McpServerConfig {
        name: server.name.clone(),
        enabled: server.enabled,
        transport: server.transport.clone(),
        command: server.command.clone(),
        args: server.args.clone(),
        env: pairs(&server.env),
        url: server.url.clone(),
        headers: pairs(&server.headers),
        tools: server.tools.clone(),
    }
}

/// Where the Host runs the imported servers' commands; unknown until startup has read the saved choice.
pub(super) fn import_environment(cx: &App) -> Option<McpImportEnvironment> {
    match Store::global(cx).read(cx).target {
        None => None,
        Some(ExecutionTarget::Windows { .. }) => Some(McpImportEnvironment::Windows),
        Some(ExecutionTarget::Wsl { .. }) => Some(McpImportEnvironment::Wsl),
        Some(ExecutionTarget::Ssh { .. }) => Some(McpImportEnvironment::Ssh),
    }
}

/// An import message in the interface language.
fn message_text(cx: &App, message: &McpImportMessage) -> SharedString {
    let vars: Vec<(&str, &dyn std::fmt::Display)> = message.vars.iter().map(|(name, value)| (*name, value as &dyn std::fmt::Display)).collect();
    trf(cx, &format!("native.{}", message.key), &vars)
}

/// Pasted MCP JSON and what was read from it (`mcp-json.ts`).
pub(super) struct Import {
    pub text: Entity<TextareaState>,
    /// The servers read from the text that the user unchecked, by [`mcp_json::import_key`].
    pub skipped: HashSet<String>,
    /// Why the chosen file could not be read.
    pub error: Option<SharedString>,
    /// The text read for the configured servers and the environment; nothing while the text is blank.
    pub preview: Option<Result<McpImport, McpImportMessage>>,
    _subscription: Subscription,
}

impl ExtensionsPage {
    /// Open the import with some text, closing the server form.
    pub(super) fn start_import(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        let state = cx.new(|cx| TextareaState::new(window, cx).auto_grow(8, 16).placeholder(IMPORT_EXAMPLE).default_value(text));
        let subscription = cx.subscribe_in(&state, window, |this, _, event: &InputEvent, _, cx| {
            if matches!(event, InputEvent::Change)
                && let Some(import) = &mut this.import
            {
                import.skipped.clear();
                import.error = None;
                this.refresh_preview(cx);
            }
        });
        self.import = Some(Import { text: state, skipped: HashSet::new(), error: None, preview: None, _subscription: subscription });
        self.form = None;
        self.removing = None;
        self.file_read += 1;
        self.refresh_preview(cx);
    }

    pub(super) fn close_import(&mut self, cx: &mut Context<Self>) {
        self.import = None;
        self.file_read += 1;
        cx.notify();
    }

    pub(super) fn refresh_preview(&mut self, cx: &mut Context<Self>) {
        self.read_import(cx);
        cx.notify();
    }

    /// Read the import's text again, for the servers configured now and where the Host runs.
    fn read_import(&mut self, cx: &App) {
        let Some(import) = &self.import else { return };
        let environment = import_environment(cx);
        let existing: Vec<ExistingServer> = self.servers().iter().map(|server| ExistingServer::Server(configured(server))).collect();
        let text = import.text.read(cx).value();
        let preview = (!mcp_json::is_blank(&text)).then(|| mcp_json::parse_mcp_json(&text, &existing, environment));
        if let Some(import) = &mut self.import {
            import.preview = preview;
        }
    }

    /// The servers read from the text that stay checked.
    fn chosen(&self) -> Vec<McpServerConfig> {
        let Some(import) = &self.import else { return Vec::new() };
        match &import.preview {
            Some(Ok(preview)) => preview.servers.iter().filter(|entry| !import.skipped.contains(&mcp_json::import_key(entry))).map(|entry| entry.server.clone()).collect(),
            _ => Vec::new(),
        }
    }

    /// Read a chosen configuration file into the import's text.
    fn choose_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let chosen = cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: false, prompt: Some(tr(cx, "native.mcpImportFile")) });
        let read = self.file_read;
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = chosen.await else { return };
            let Some(path) = paths.into_iter().next() else { return };
            let Ok(read) = this.update(cx, |this, _| {
                // Only the newest read of a file the panel still waits for is shown.
                if this.file_read != read || this.import.is_none() {
                    return None;
                }
                this.file_read += 1;
                Some(this.file_read)
            }) else {
                return;
            };
            let Some(read) = read else { return };
            let text = cx.background_spawn(async move { mcp_json::read_config_file(&path) }).await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.file_read != read {
                    return;
                }
                let Some(import) = &mut this.import else { return };
                match text {
                    Ok(text) => {
                        let unchanged = import.text.read(cx).value().as_ref() == text.as_str();
                        import.skipped.clear();
                        import.error = None;
                        if unchanged {
                            // `set_value` is a no-op when content is identical; clear stale error/skips and repaint once.
                            cx.notify();
                        } else {
                            // `set_value` emits `InputEvent::Change`, which calls `refresh_preview` exactly once.
                            import.text.update(cx, |state, cx| state.set_value(text, window, cx));
                        }
                    }
                    Err(message) => {
                        import.error = Some(message_text(cx, &message));
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    /// Save the checked servers one by one; a failure stops the import and says how many were saved before it. Saved
    /// servers then match configured ones, so trying again adds no copies. Closing the settings window does not stop
    /// the import, and its result is still shown.
    fn import_servers(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let chosen = self.chosen();
        if chosen.is_empty() || self.guard.busy() {
            return;
        }
        let generation = self.guard.begin_write();
        cx.notify();
        let store = self.store.clone();
        cx.spawn_in(window, async move |this, cx| {
            let total = chosen.len();
            let mut failure = None;
            for (done, server) in chosen.into_iter().enumerate() {
                let call = store.read_with(cx, |store, _| store.call("extensions.saveServer", json!({ "server": server })));
                let error = match call.await.map(serde_json::from_value::<Status>) {
                    Ok(Ok(status)) => {
                        let _ = this.update(cx, |this, cx| this.merge(status, cx));
                        continue;
                    }
                    Ok(Err(error)) => error.to_string(),
                    Err(error) => cx.update(|_, cx| crate::ui::error_text(cx, &error).to_string()).unwrap_or(error.message),
                };
                failure = Some((server.name, done, error));
                break;
            }
            let _ = this.update(cx, |this, cx| {
                let _ = this.guard.end_write(generation);
                if failure.is_none() {
                    this.close_import(cx);
                }
                cx.notify();
            });
            let _ = cx.update(|window, cx| {
                let notification = match &failure {
                    Some((name, done, error)) => Notification::error(trf(cx, "native.mcpImportFailed", &[("name", name), ("count", done), ("error", error)]).to_string()),
                    None => trf(cx, "native.mcpImportDone", &[("count", &total)]).to_string().into(),
                };
                window.push_notification(notification, cx);
            });
        })
        .detach();
    }

    /// The import panel: the text, what it holds with notes and masked secrets, and the entries left out.
    pub(super) fn render_import(&self, import: &Import, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let busy = self.guard.busy();
        let muted = |text: SharedString| div().text_xs().text_color(theme.muted_foreground).child(text);
        let warning_color = crate::theme::warning_text(cx);
        let warning = |text: SharedString| div().text_xs().text_color(warning_color).child(text);
        let chosen = self.chosen().len();
        let mut panel = v_flex()
            .gap_2()
            .mt_2()
            .p_3()
            .rounded(theme.radius)
            .border_1()
            .border_color(theme.border)
            .child(div().text_sm().font_semibold().child(tr(cx, "native.mcpImportTitle")))
            .child(muted(tr(cx, "native.mcpImportHint")))
            .child(Textarea::new(&import.text).disabled(busy).aria_label(tr(cx, "native.mcpImportLabel")))
            .child(
                h_flex().gap_2().child(
                    Button::new("mcp-import-file")
                        .outline()
                        .small()
                        .label(tr(cx, "native.mcpImportFile"))
                        .disabled(busy)
                        .on_click(cx.listener(|this, _, window, cx| this.choose_file(window, cx))),
                ),
            );
        if let Some(error) = &import.error {
            panel = panel.child(notice(cx, error.clone(), true));
        }
        match &import.preview {
            None => {}
            Some(Err(error)) => panel = panel.child(notice(cx, message_text(cx, error), true)),
            Some(Ok(preview)) => {
                for repair in &preview.notices {
                    panel = panel.child(warning(message_text(cx, repair)));
                }
                if !preview.servers.is_empty() {
                    panel = panel.child(muted(trf(cx, "native.mcpImportFound", &[("count", &preview.servers.len())])));
                }
                for (index, entry) in preview.servers.iter().enumerate() {
                    panel = panel.child(self.render_imported(index, entry, import, cx));
                }
                if !preview.problems.is_empty() {
                    panel = panel.child(muted(tr(cx, "native.mcpImportSkipped")));
                    for problem in &preview.problems {
                        panel = panel.child(div().text_xs().text_color(theme.danger).child(message_text(cx, problem)));
                    }
                }
            }
        }
        panel
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("mcp-import-submit")
                            .primary()
                            .small()
                            .label(trf(cx, "native.mcpImportSubmit", &[("count", &chosen)]))
                            .disabled(busy || chosen == 0)
                            .on_click(cx.listener(|this, _, window, cx| this.import_servers(window, cx))),
                    )
                    .child(Button::new("mcp-import-cancel").outline().small().label(tr(cx, "settings.settingsCancel")).disabled(busy).on_click(cx.listener(|this, _, _, cx| this.close_import(cx)))),
            )
            .into_any_element()
    }

    /// One server read from the text: whether it is imported, what it runs or reaches with its secrets hidden, and notes.
    fn render_imported(&self, index: usize, entry: &ImportedServer, import: &Import, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let server = &entry.server;
        let key = mcp_json::import_key(entry);
        let checked = !import.skipped.contains(&key);
        let stdio = server.transport == "stdio";
        let line = if stdio {
            mcp_json::masked_command(&std::iter::once(server.command.clone().unwrap_or_default()).chain(server.args.clone().unwrap_or_default()).collect::<Vec<_>>())
        } else {
            mcp_json::masked_url(server.url.as_deref().unwrap_or_default())
        };
        let keys = |pairs: &Option<Vec<(String, String)>>| pairs.as_ref().map(|pairs| pairs.iter().map(|(key, _)| key.as_str()).collect::<Vec<_>>().join(", "));
        let env = keys(&server.env).map(|keys| trf(cx, "native.mcpImportEnv", &[("keys", &keys)]));
        let headers = keys(&server.headers).map(|keys| trf(cx, "native.mcpImportHeaders", &[("keys", &keys)]));
        let notes: Vec<SharedString> = entry.notes.iter().map(|note| message_text(cx, note)).collect();
        let warning = crate::theme::warning_text(cx);
        let include = trf(cx, "native.mcpImportInclude", &[("name", &server.name)]);
        v_flex()
            .gap_0p5()
            .py_2()
            .border_t_1()
            .border_color(theme.border)
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .text_sm()
                    .child(Checkbox::new(("mcp-import", index)).label(server.name.clone()).tooltip(include).checked(checked).disabled(self.guard.busy()).on_click(cx.listener(
                        move |this, checked: &bool, _, cx| {
                            if let Some(import) = &mut this.import {
                                if *checked {
                                    import.skipped.remove(&key);
                                } else {
                                    import.skipped.insert(key.clone());
                                }
                                cx.notify();
                            }
                        },
                    )))
                    .child(Tag::secondary().outline().small().child(if stdio { "stdio" } else { "HTTP" }))
                    .when(!server.enabled, |this| this.child(Tag::secondary().small().child(tr(cx, "settings.mcpStateDisabled")))),
            )
            .child(div().text_xs().font_family(theme.mono_font_family.clone()).text_color(theme.muted_foreground).child(line))
            .when_some(env, |this, env| this.child(div().text_xs().text_color(theme.muted_foreground).child(env)))
            .when_some(headers, |this, headers| this.child(div().text_xs().text_color(theme.muted_foreground).child(headers)))
            .children(notes.into_iter().map(|note| div().text_xs().text_color(warning).child(note)))
            .into_any_element()
    }
}
