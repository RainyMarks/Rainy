//! MCP server list and expandable per-server tool toggles inside Settings → Skills & MCP.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::tag::Tag;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _, StyledExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use rainy_carrier::{ExecutionTarget, mcp_json};

use super::{ExtensionsPage, McpServer, McpTool, is_windows_mcp};
use crate::store::Store;
use crate::ui::settings::models_extra::card;
use crate::ui::{tr, trf};

impl ExtensionsPage {
    /// The tool list under a server: what each tool does on hover, and a switch for each.
    fn render_tools(&self, index: usize, server: &McpServer, live: &[McpTool], cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let open = self.expanded.contains(&server.name);
        let enabled = live.iter().filter(|tool| tool.enabled).count();
        let busy = self.guard.busy();
        let name = server.name.clone();
        let header = h_flex()
            .id(("mcp-tools-toggle", index))
            .gap_1()
            .items_center()
            .cursor_pointer()
            .text_sm()
            .font_semibold()
            .child(Icon::new(if open { IconName::ChevronDown } else { IconName::ChevronRight }).xsmall())
            .child(trf(cx, "settings.mcpToolsSummary", &[("enabled", &enabled), ("total", &live.len())]))
            .on_click(cx.listener(move |this, _, _, cx| {
                if !this.expanded.remove(&name) {
                    this.expanded.insert(name.clone());
                }
                cx.notify();
            }));
        let mut panel = v_flex().gap_2().mt_1().pt_2().border_t_1().border_color(theme.border).child(header);
        if !open {
            return panel.into_any_element();
        }
        let on_name = server.name.clone();
        let off_name = server.name.clone();
        panel = panel.child(
            h_flex()
                .gap_2()
                .flex_wrap()
                .items_center()
                .child(Button::new(("mcp-tools-all-on", index)).outline().xsmall().label(tr(cx, "settings.mcpToolsAllOn")).disabled(busy || enabled == live.len()).on_click(cx.listener(
                    move |this, _, window, cx| this.toggle_tools(&on_name, None, true, window, cx),
                )))
                .child(Button::new(("mcp-tools-all-off", index)).outline().xsmall().label(tr(cx, "settings.mcpToolsAllOff")).disabled(busy || enabled == 0).on_click(cx.listener(
                    move |this, _, window, cx| this.toggle_tools(&off_name, None, false, window, cx),
                )))
                .child(div().text_xs().text_color(theme.muted_foreground).child(tr(cx, "settings.mcpToolsNextMessage"))),
        );
        let rows = live.iter().enumerate().map(|(position, tool)| {
            let server_name = server.name.clone();
            let tool_name = tool.name.clone();
            let description: SharedString = if tool.description.trim().is_empty() { tr(cx, "settings.mcpToolNoDescription") } else { tool.description.trim().to_owned().into() };
            let title: SharedString = tool.name.clone().into();
            h_flex()
                .id(SharedString::from(format!("mcp-tool-{index}-{position}")))
                .w(relative(0.5))
                .p_1()
                .child(
                    h_flex()
                        .w_full()
                        .gap_2()
                        .items_center()
                        .px_3()
                        .py_2()
                        .rounded(theme.radius)
                        .border_1()
                        .border_color(theme.border)
                        .bg(theme.background)
                        .child(div().size(px(7.)).rounded_full().flex_none().bg(if tool.enabled { theme.success } else { theme.muted_foreground.opacity(0.5) }))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .overflow_hidden()
                                .text_ellipsis()
                                .text_sm()
                                .font_family(theme.mono_font_family.clone())
                                .text_color(if tool.enabled { theme.foreground } else { theme.muted_foreground })
                                .child(tool.name.clone()),
                        )
                        .child(Switch::new(SharedString::from(format!("mcp-tool-switch-{index}-{position}"))).checked(tool.enabled).disabled(busy).on_click(cx.listener(
                            move |this, checked: &bool, window, cx| {
                                let names = [tool_name.clone()];
                                this.toggle_tools(&server_name, Some(&names), *checked, window, cx);
                            },
                        ))),
                )
                // A long description wraps inside a bounded box instead of running off the window.
                .tooltip(move |window, cx| {
                    let (title, description) = (title.clone(), description.clone());
                    gpui_kit::component::tooltip::Tooltip::element(move |_, _| {
                        v_flex().max_w(px(440.)).gap_1().py_1().child(div().font_semibold().child(title.clone())).child(div().text_xs().child(description.clone()))
                    })
                    .build(window, cx)
                })
        });
        panel
            .child(div().id(("mcp-tools-list", index)).max_h(px(360.)).overflow_y_scroll().child(h_flex().flex_wrap().children(rows)))
            .into_any_element()
    }

    pub(super) fn render_servers(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let Some(status) = &self.status else { return card(cx, "settings.mcpTitle", None, vec![]) };
        let busy = self.guard.busy();
        // Windows-MCP drives this PC's desktop, which a Host on an SSH server cannot reach.
        let target = Store::global(cx).read(cx).target.clone();
        let desktop = match target {
            None | Some(ExecutionTarget::Windows { .. }) => Some(false),
            Some(ExecutionTarget::Wsl { .. }) => Some(true),
            Some(ExecutionTarget::Ssh { .. }) => None,
        };
        let has_desktop = status.servers.iter().any(is_windows_mcp);
        let actions = h_flex()
            .gap_2()
            .flex_wrap()
            .justify_end()
            .when_some(desktop.filter(|_| !has_desktop), |this, wsl| {
                this.child(Button::new("mcp-desktop").outline().small().label(tr(cx, "settings.mcpAddDesktop")).disabled(busy).on_click(cx.listener(
                    move |this, _, window, cx| this.open_desktop(wsl, window, cx),
                )))
            })
            .child(Button::new("mcp-import").outline().small().label(tr(cx, "native.mcpImport")).disabled(busy).on_click(cx.listener(|this, _, window, cx| {
                this.start_import(String::new(), window, cx)
            })))
            .child(Button::new("mcp-add").outline().small().label(tr(cx, "settings.mcpAdd")).disabled(busy).on_click(cx.listener(|this, _, window, cx| {
                this.open_form(None, false, window, cx)
            })));
        let mut body = vec![actions.into_any_element()];
        if status.servers.is_empty() {
            body.push(div().text_sm().text_color(theme.muted_foreground).child(tr(cx, "settings.mcpEmpty")).into_any_element());
        }
        for (index, server) in status.servers.iter().enumerate() {
            let live = status.status.iter().find(|entry| entry.name == server.name);
            let state = if server.enabled { live.map_or("connecting", |entry| entry.state.as_str()) } else { "disabled" };
            let (label, tag) = match state {
                "ready" => ("settings.mcpStateReady", Tag::success()),
                "error" => ("settings.mcpStateError", Tag::danger()),
                "disabled" => ("settings.mcpStateDisabled", Tag::secondary()),
                _ => ("settings.mcpStateConnecting", Tag::warning()),
            };
            let line = if server.transport == "stdio" {
                mcp_json::masked_command(&server.command.iter().chain(server.args.iter().flatten()).cloned().collect::<Vec<_>>())
            } else {
                mcp_json::masked_url(server.url.as_deref().unwrap_or_default())
            };
            let tools = live.filter(|entry| !entry.tools.is_empty() && server.enabled).map(|entry| self.render_tools(index, server, &entry.tools, cx));
            let error = live.filter(|_| state == "error").and_then(|entry| entry.error.clone());
            let (toggled, edited, removed) = (server.clone(), server.clone(), server.name.clone());
            let desktop_server = is_windows_mcp(server);
            let confirming = self.removing.as_deref() == Some(server.name.as_str());
            let row = h_flex()
                .gap_3()
                .items_start()
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_0p5()
                        .child(
                            h_flex()
                                .gap_2()
                                .items_center()
                                .text_sm()
                                .child(server.name.clone())
                                .child(Tag::secondary().outline().small().child(if server.transport == "stdio" { "stdio" } else { "HTTP" }))
                                .child(tag.small().child(tr(cx, label))),
                        )
                        .child(div().text_xs().font_family(theme.mono_font_family.clone()).text_color(theme.muted_foreground).overflow_hidden().text_ellipsis().child(line))
                        .when_some(error, |this, error| this.child(div().text_xs().text_color(theme.danger).child(error))),
                )
                .child(
                    h_flex()
                        .flex_none()
                        .gap_2()
                        .items_center()
                        .child(Switch::new(("mcp-enabled", index)).checked(server.enabled).disabled(busy).on_click(cx.listener(
                            move |this, checked: &bool, window, cx| this.toggle(toggled.clone(), *checked, window, cx),
                        )))
                        .child(Button::new(("mcp-edit", index)).small().outline().label(tr(cx, "settings.mcpEdit")).disabled(busy).on_click(cx.listener(
                            move |this, _, window, cx| this.open_form(Some(edited.clone()), desktop_server, window, cx),
                        )))
                        .child(
                            Button::new(("mcp-remove", index))
                                .small()
                                .map(|button| if confirming { button.danger() } else { button.outline() })
                                .label(tr(cx, if confirming { "settings.mcpRemoveConfirm" } else { "settings.mcpRemove" }))
                                .disabled(busy)
                                .on_click(cx.listener(move |this, _, window, cx| this.remove(removed.clone(), window, cx))),
                        ),
                );
            body.push(v_flex().gap_1().py_2().border_t_1().border_color(theme.border).child(row).children(tools).into_any_element());
        }
        if let Some(import) = &self.import {
            body.push(self.render_import(import, cx));
        }
        if let Some(form) = &self.form {
            body.push(self.render_form(form, cx));
        }
        card(cx, "settings.mcpTitle", None, body)
    }
}
