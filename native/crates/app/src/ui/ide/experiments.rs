//! Detached persistent background experiments panel (`BottomTab::Experiments`).
//! Experiments run in their own session on the Host and survive SSH disconnects or Rainy restarts.
//! Supports listing, starting with CUDA/env presets, graceful/force stop, removal, and live incremental
//! `experiments.tail` streaming.

use std::collections::BTreeMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _, StyledExt as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use rainy_protocol::{ExperimentListResponse, ExperimentRecord, ExperimentStartParams, ExperimentStatus, ExperimentTailResult};
use serde_json::json;

use crate::store::ide::{Ide, IdeEvent};
use crate::store::{Store, StoreEvent};
use crate::ui::{tr, trf};

/// Poll interval for live log tailing and running status updates while this tab is visible.
const TAIL_INTERVAL: Duration = Duration::from_secs(1);
/// Maximum tail bytes requested per incremental chunk.
const TAIL_MAX_BYTES: u64 = 65_536;
/// Bounded log buffer limits so long-running training logs never grow unbounded in RAM.
const MAX_TAIL_LINES: usize = 1500;
const MAX_TAIL_BYTES: usize = 256 * 1024;
/// Lines included when quoting experiment logs into the chat composer.
const QUOTE_LINES: usize = 60;

pub struct ExperimentsView {
    store: Entity<Store>,
    ide: Entity<Ide>,
    active: bool,
    experiments: Vec<ExperimentRecord>,
    selected: Option<String>,
    loading: bool,
    starting: bool,
    show_form: bool,
    apply_host_env: bool,
    name_input: Entity<InputState>,
    cmd_input: Entity<InputState>,
    cwd_input: Entity<InputState>,
    cuda_input: Entity<InputState>,
    log_input: Entity<InputState>,
    tail_id: Option<String>,
    tail_offset: Option<u64>,
    tail_text: String,
    tail_total_bytes: u64,
    tail_reset: bool,
    tail_paused: bool,
    tail_scroll: ScrollHandle,
    tail_task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl ExperimentsView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let store = Store::global(cx);
        let ide = Ide::global(cx);
        let default_cwd = ide
            .read(cx)
            .workspace
            .as_ref()
            .map(|w| w.path.clone())
            .unwrap_or_default();

        let name_input = cx.new(|cx| InputState::new(window, cx).placeholder(tr(cx, "experiments.namePlaceholder")));
        let cmd_input = cx.new(|cx| InputState::new(window, cx).placeholder(tr(cx, "experiments.commandPlaceholder")));
        let cwd_input = cx.new(|cx| {
            let mut state = InputState::new(window, cx).placeholder(tr(cx, "experiments.cwdPlaceholder"));
            if !default_cwd.is_empty() {
                state.set_value(default_cwd, window, cx);
            }
            state
        });
        let cuda_input = cx.new(|cx| InputState::new(window, cx).placeholder(tr(cx, "experiments.cudaPlaceholder")));
        let log_input = cx.new(|cx| InputState::new(window, cx).placeholder(tr(cx, "experiments.logPathPlaceholder")));

        let subscriptions = vec![
            cx.subscribe_in(&store, window, |this, _, event: &StoreEvent, window, cx| match event {
                StoreEvent::HostReconnected(_) => {
                    this.stop_tail();
                    this.experiments.clear();
                    this.selected = None;
                    if this.active {
                        this.refresh(cx);
                    }
                    cx.notify();
                }
                StoreEvent::PrefsChanged => {
                    let (n, c, d, g, l) = (
                        tr(cx, "experiments.namePlaceholder"),
                        tr(cx, "experiments.commandPlaceholder"),
                        tr(cx, "experiments.cwdPlaceholder"),
                        tr(cx, "experiments.cudaPlaceholder"),
                        tr(cx, "experiments.logPathPlaceholder"),
                    );
                    this.name_input.update(cx, |s, cx| s.set_placeholder(n, window, cx));
                    this.cmd_input.update(cx, |s, cx| s.set_placeholder(c, window, cx));
                    this.cwd_input.update(cx, |s, cx| s.set_placeholder(d, window, cx));
                    this.cuda_input.update(cx, |s, cx| s.set_placeholder(g, window, cx));
                    this.log_input.update(cx, |s, cx| s.set_placeholder(l, window, cx));
                }
                _ => {}
            }),
            cx.subscribe_in(&ide, window, |this, ide, event: &IdeEvent, window, cx| {
                if *event == IdeEvent::WorkspaceChanged
                    && let Some(ws) = &ide.read(cx).workspace
                {
                    let path = ws.path.clone();
                    this.cwd_input.update(cx, |s, cx| s.set_value(path, window, cx));
                }
            }),
        ];

        Self {
            store,
            ide,
            active: false,
            experiments: Vec::new(),
            selected: None,
            loading: false,
            starting: false,
            show_form: false,
            apply_host_env: true,
            name_input,
            cmd_input,
            cwd_input,
            cuda_input,
            log_input,
            tail_id: None,
            tail_offset: None,
            tail_text: String::new(),
            tail_total_bytes: 0,
            tail_reset: false,
            tail_paused: false,
            tail_scroll: ScrollHandle::new(),
            tail_task: None,
            _subscriptions: subscriptions,
        }
    }

    /// Number of currently running detached experiments (for the tab badge).
    pub fn running_count(&self) -> usize {
        self.experiments.iter().filter(|e| e.status == ExperimentStatus::Running).count()
    }

    /// Enable or pause polling when switching into or out of the Experiments bottom tab.
    pub fn set_active(&mut self, active: bool, cx: &mut Context<Self>) {
        if self.active == active {
            return;
        }
        self.active = active;
        if active {
            self.refresh(cx);
            if let Some(id) = self.selected.clone() {
                self.start_tail(id, cx);
            }
        } else {
            self.tail_task = None;
        }
        cx.notify();
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.loading {
            return;
        }
        self.loading = true;
        cx.notify();
        let call = self.store.read(cx).call("experiments.list", json!({}));
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let result = call.await;
            let _ = this.update(cx, |this, cx| {
                this.loading = false;
                if let Ok(Ok(resp)) = result.map(serde_json::from_value::<ExperimentListResponse>) {
                    this.experiments = resp.experiments;
                    if let Some(sel) = &this.selected
                        && !this.experiments.iter().any(|e| &e.id == sel)
                    {
                        this.selected = None;
                        this.stop_tail();
                    }
                    if this.selected.is_none()
                        && let Some(first) = this.experiments.first()
                    {
                        let id = first.id.clone();
                        this.select(id, cx);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn select(&mut self, id: String, cx: &mut Context<Self>) {
        if self.selected.as_deref() == Some(id.as_str()) && self.tail_task.is_some() {
            return;
        }
        self.selected = Some(id.clone());
        self.tail_id = Some(id.clone());
        self.tail_offset = None;
        self.tail_text.clear();
        self.tail_total_bytes = 0;
        self.tail_reset = false;
        self.tail_paused = false;
        if self.active {
            self.start_tail(id, cx);
        }
        cx.notify();
    }

    fn stop_tail(&mut self) {
        self.tail_task = None;
        self.tail_id = None;
        self.tail_offset = None;
        self.tail_text.clear();
        self.tail_total_bytes = 0;
        self.tail_reset = false;
    }

    fn start_tail(&mut self, id: String, cx: &mut Context<Self>) {
        self.tail_id = Some(id.clone());
        self.tail_task = Some(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let mut tick: u32 = 0;
            loop {
                let request = this
                    .update(cx, |this, cx| {
                        if !this.active || this.tail_id.as_deref() != Some(id.as_str()) {
                            return None;
                        }
                        if this.tail_paused {
                            return Some(None);
                        }
                        let mut params = json!({
                            "id": id,
                            "maxBytes": TAIL_MAX_BYTES,
                        });
                        if let Some(offset) = this.tail_offset {
                            params["offset"] = json!(offset);
                        }
                        let call = this.store.read(cx).call("experiments.tail", params);
                        Some(Some(call))
                    })
                    .ok()
                    .flatten();

                match request {
                    None => break,
                    Some(None) => {}
                    Some(Some(call)) => {
                        if let Ok(Ok(tail)) = call.await.map(serde_json::from_value::<ExperimentTailResult>) {
                            let _ = this.update(cx, |this, cx| {
                                if this.tail_id.as_deref() != Some(id.as_str()) {
                                    return;
                                }
                                if tail.reset {
                                    this.tail_text.clear();
                                    this.tail_reset = true;
                                }
                                if !tail.text.is_empty() {
                                    append_bounded_text(&mut this.tail_text, &tail.text);
                                    if !this.tail_paused {
                                        this.tail_scroll.scroll_to_bottom();
                                    }
                                }
                                this.tail_offset = Some(tail.next_offset);
                                this.tail_total_bytes = tail.total_bytes;
                                cx.notify();
                            });
                        }
                    }
                }
                // Refresh experiment statuses every 5 seconds while tailing so exited processes update promptly.
                tick = tick.wrapping_add(1);
                if tick % 5 == 0 {
                    let _ = this.update(cx, |this, cx| {
                        if this.active && !this.loading {
                            this.refresh(cx);
                        }
                    });
                }
                cx.background_executor().timer(TAIL_INTERVAL).await;
            }
        }));
    }

    fn open_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.show_form = true;
        if self.cwd_input.read(cx).value().trim().is_empty()
            && let Some(ws) = &self.ide.read(cx).workspace
        {
            let path = ws.path.clone();
            self.cwd_input.update(cx, |s, cx| s.set_value(path, window, cx));
        }
        cx.notify();
    }

    fn submit_new(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.starting {
            return;
        }
        let command = self.cmd_input.read(cx).value().trim().to_owned();
        let cwd = self.cwd_input.read(cx).value().trim().to_owned();
        if command.is_empty() || cwd.is_empty() {
            window.push_notification(Notification::warning(tr(cx, "experiments.commandRequired")), cx);
            return;
        }
        let name = self.name_input.read(cx).value().trim().to_owned();
        let cuda = self.cuda_input.read(cx).value().trim().to_owned();
        let log_path = self.log_input.read(cx).value().trim().to_owned();
        let mut env = BTreeMap::new();
        if self.apply_host_env
            && let Some(profile) = self.store.read(cx).current_ssh_profile()
        {
            env = profile.env;
        }
        let params = ExperimentStartParams {
            name: (!name.is_empty()).then_some(name),
            cwd,
            command,
            cuda_visible_devices: (!cuda.is_empty()).then_some(cuda),
            env,
            log_path: (!log_path.is_empty()).then_some(log_path),
        };
        let payload = serde_json::to_value(&params).unwrap_or_default();
        self.starting = true;
        cx.notify();
        let call = self.store.read(cx).call("experiments.start", payload);
        cx.spawn_in(window, async move |this, cx| {
            let result = call.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.starting = false;
                match result.and_then(|v| {
                    serde_json::from_value::<ExperimentRecord>(v)
                        .map_err(|e| rainy_protocol::RpcError { code: "invalid".into(), message: e.to_string(), data: None })
                }) {
                    Ok(record) => {
                        let display = record.name.clone();
                        let id = record.id.clone();
                        this.show_form = false;
                        this.name_input.update(cx, |s, cx| s.set_value(String::new(), window, cx));
                        this.cmd_input.update(cx, |s, cx| s.set_value(String::new(), window, cx));
                        this.log_input.update(cx, |s, cx| s.set_value(String::new(), window, cx));
                        this.experiments.retain(|e| e.id != id);
                        this.experiments.insert(0, record);
                        this.select(id, cx);
                        let note = trf(cx, "experiments.started", &[("name", &display)]);
                        window.push_notification(Notification::success(note), cx);
                    }
                    Err(error) => {
                        window.push_notification(crate::ui::error_notification(cx, &error), cx);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn stop_experiment(&mut self, id: String, name: String, force: bool, window: &mut Window, cx: &mut Context<Self>) {
        let call = self.store.read(cx).call("experiments.stop", json!({ "id": id, "force": force }));
        cx.spawn_in(window, async move |this, cx| {
            let result = call.await;
            let _ = this.update_in(cx, |this, window, cx| match result {
                Ok(value) => {
                    let record = serde_json::from_value::<ExperimentRecord>(value).ok();
                    let still_running = record.as_ref().is_some_and(|r| r.status == ExperimentStatus::Running);
                    if let Some(updated) = record {
                        if let Some(slot) = this.experiments.iter_mut().find(|e| e.id == updated.id) {
                            *slot = updated;
                        }
                    }
                    if still_running {
                        let note = trf(cx, "experiments.stillRunning", &[("name", &name)]);
                        window.push_notification(Notification::warning(note), cx);
                    } else {
                        let note = trf(cx, "experiments.stopped", &[("name", &name)]);
                        window.push_notification(Notification::info(note), cx);
                    }
                    this.refresh(cx);
                    cx.notify();
                }
                Err(error) => window.push_notification(crate::ui::error_notification(cx, &error), cx),
            });
        })
        .detach();
    }

    fn remove_experiment(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let call = self.store.read(cx).call("experiments.remove", json!({ "id": id }));
        cx.spawn_in(window, async move |this, cx| {
            let result = call.await;
            let _ = this.update_in(cx, |this, window, cx| match result {
                Ok(_) => {
                    this.experiments.retain(|e| e.id != id);
                    if this.selected.as_deref() == Some(id.as_str()) {
                        this.selected = None;
                        this.stop_tail();
                    }
                    window.push_notification(Notification::info(tr(cx, "experiments.removed")), cx);
                    cx.notify();
                }
                Err(error) => window.push_notification(crate::ui::error_notification(cx, &error), cx),
            });
        })
        .detach();
    }

    fn quote_tail_to_chat(&self, window: &mut Window, cx: &mut App) {
        let trimmed = self.tail_text.trim();
        if trimmed.is_empty() {
            return;
        }
        let lines: Vec<&str> = trimmed.lines().collect();
        let slice = if lines.len() > QUOTE_LINES { &lines[lines.len() - QUOTE_LINES..] } else { &lines[..] };
        let mut quoted = String::from("> ```text\n");
        for line in slice {
            quoted.push_str("> ");
            quoted.push_str(line);
            quoted.push('\n');
        }
        quoted.push_str("> ```");
        crate::ui::insert_into_composer(quoted, window, cx);
    }

    fn render_form(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme().clone();
        let env_count = self.store.read(cx).current_ssh_profile().map_or(0, |p| p.env.len());
        let apply_env = self.apply_host_env;
        v_flex()
            .p_2()
            .gap_1p5()
            .border_b_1()
            .border_color(theme.border)
            .bg(theme.secondary.opacity(0.45))
            // The buttons sit on the title row so they are in view however short the panel is.
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .child(div().text_xs().font_semibold().child(tr(cx, "experiments.formTitle")))
                    .child(div().flex_1())
                    .child(
                        Button::new("exp-cancel")
                            .ghost()
                            .xsmall()
                            .label(tr(cx, "experiments.cancel"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.show_form = false;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("exp-start")
                            .primary()
                            .xsmall()
                            .loading(self.starting)
                            .label(tr(cx, "experiments.start"))
                            .on_click(cx.listener(|this, _, window, cx| this.submit_new(window, cx))),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_0p5()
                            .child(div().text_xs().text_color(theme.muted_foreground).child(tr(cx, "experiments.nameLabel")))
                            .child(Input::new(&self.name_input).xsmall()),
                    )
                    .child(
                        v_flex()
                            .w(px(170.))
                            .gap_0p5()
                            .child(div().text_xs().text_color(theme.muted_foreground).child(tr(cx, "experiments.cudaLabel")))
                            .child(Input::new(&self.cuda_input).xsmall()),
                    ),
            )
            .child(
                v_flex()
                    .gap_0p5()
                    .child(div().text_xs().text_color(theme.muted_foreground).child(tr(cx, "experiments.commandLabel")))
                    .child(Input::new(&self.cmd_input).xsmall()),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_0p5()
                            .child(div().text_xs().text_color(theme.muted_foreground).child(tr(cx, "experiments.cwdLabel")))
                            .child(Input::new(&self.cwd_input).xsmall()),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_0p5()
                            .child(div().text_xs().text_color(theme.muted_foreground).child(tr(cx, "experiments.logPathLabel")))
                            .child(Input::new(&self.log_input).xsmall()),
                    ),
            )
            .when(env_count > 0, |this| {
                this.child(
                    Checkbox::new("exp-apply-env")
                        .checked(apply_env)
                        .label(trf(cx, "experiments.applyHostEnv", &[("count", &env_count)]))
                        .on_click(cx.listener(|this, checked: &bool, _, cx| {
                            this.apply_host_env = *checked;
                            cx.notify();
                        })),
                )
            })
    }
}

impl Render for ExperimentsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let now_ms = now_epoch_ms();
        let selected_record = self
            .selected
            .as_ref()
            .and_then(|id| self.experiments.iter().find(|e| &e.id == id))
            .cloned();

        let toolbar = h_flex()
            .h(px(28.))
            .flex_shrink_0()
            .px_2()
            .gap_1p5()
            .items_center()
            .border_b_1()
            .border_color(theme.border)
            .child(
                Button::new("exp-new")
                    .outline()
                    .xsmall()
                    .icon(IconName::Plus)
                    .label(tr(cx, "experiments.newExperiment"))
                    .on_click(cx.listener(|this, _, window, cx| this.open_form(window, cx))),
            )
            .child(
                Button::new("exp-refresh")
                    .ghost()
                    .xsmall()
                    .icon(Icon::new(gpui_kit::assets::IconName::RotateCcw))
                    .tooltip(tr(cx, "experiments.refresh"))
                    .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
            )
            .when(self.loading, |this| this.child(Spinner::new().xsmall()));

        let form = self.show_form.then(|| self.render_form(cx));

        let rows: Vec<AnyElement> = self
            .experiments
            .iter()
            .enumerate()
            .map(|(idx, exp)| {
                let is_selected = self.selected.as_deref() == Some(exp.id.as_str());
                let (dot_color, status_text) = match exp.status {
                    ExperimentStatus::Running => (theme.success, tr(cx, "experiments.statusRunning")),
                    ExperimentStatus::Exited => {
                        let label = match exp.exit_code {
                            Some(code) => trf(cx, "experiments.exitCode", &[("code", &code)]),
                            None => tr(cx, "experiments.statusExited"),
                        };
                        (theme.muted_foreground, label)
                    }
                    ExperimentStatus::Stopped => (theme.warning, tr(cx, "experiments.statusStopped")),
                    ExperimentStatus::Unknown => (theme.muted_foreground, tr(cx, "experiments.statusUnknown")),
                };
                let end_ms = exp.ended_at.unwrap_or(now_ms);
                let elapsed = format_elapsed(end_ms.saturating_sub(exp.started_at));
                let mut meta = vec![status_text.to_string(), elapsed];
                if let Some(gpu) = exp.cuda_visible_devices.as_deref().filter(|g| !g.trim().is_empty()) {
                    meta.push(format!("GPU {gpu}"));
                }
                if let Some(pid) = exp.pid {
                    meta.push(format!("PID {pid}"));
                }

                let id_sel = exp.id.clone();
                let id_stop = exp.id.clone();
                let id_kill = exp.id.clone();
                let id_rm = exp.id.clone();
                let name_stop = exp.name.clone();
                let name_kill = exp.name.clone();
                let running = exp.status == ExperimentStatus::Running;

                h_flex()
                    .id(("exp-row", idx))
                    .px_2()
                    .py_1p5()
                    .gap_2()
                    .items_center()
                    .rounded(theme.radius)
                    .cursor_pointer()
                    .when(is_selected, |this| this.bg(theme.list_active))
                    .when(!is_selected, |this| this.hover(|style| style.bg(theme.list_hover)))
                    .child(div().size(px(7.)).flex_shrink_0().rounded_full().bg(dot_color))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_0p5()
                            .child(div().text_xs().font_medium().truncate().child(exp.name.clone()))
                            .child(div().text_xs().text_color(theme.muted_foreground).truncate().child(meta.join(" · ")))
                            .child(
                                div()
                                    .text_xs()
                                    .font_family(theme.mono_font_family.clone())
                                    .text_color(theme.muted_foreground)
                                    .truncate()
                                    .child(exp.command.clone()),
                            ),
                    )
                    .when(running, |this| {
                        this.child(
                            Button::new(("exp-stop", idx))
                                .outline()
                                .xsmall()
                                .label(tr(cx, "experiments.stop"))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.stop_experiment(id_stop.clone(), name_stop.clone(), false, window, cx);
                                })),
                        )
                        .child(
                            Button::new(("exp-kill", idx))
                                .ghost()
                                .xsmall()
                                .label(tr(cx, "experiments.forceStop"))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.stop_experiment(id_kill.clone(), name_kill.clone(), true, window, cx);
                                })),
                        )
                    })
                    .when(!running, |this| {
                        this.child(
                            Button::new(("exp-remove", idx))
                                .ghost()
                                .xsmall()
                                .icon(IconName::Close)
                                .tooltip(tr(cx, "experiments.remove"))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.remove_experiment(id_rm.clone(), window, cx);
                                })),
                        )
                    })
                    .on_click(cx.listener(move |this, _, _, cx| this.select(id_sel.clone(), cx)))
                    .into_any_element()
            })
            .collect();

        let has_selected = selected_record.is_some();
        let list_pane = v_flex()
            .id("experiments-list")
            .when(has_selected, |this| this.w(px(250.)).min_w(px(180.)).flex_shrink_0().border_r_1().border_color(theme.border))
            .when(!has_selected, |this| this.flex_1().min_w_0())
            .h_full()
            .overflow_y_scrollbar()
            .p_1p5()
            .gap_1()
            .when(rows.is_empty(), |this| {
                this.child(div().p_2().text_xs().text_color(theme.muted_foreground).child(tr(cx, "experiments.empty")))
            })
            .children(rows);

        let tail_pane = if let Some(exp) = selected_record {
            let paused = self.tail_paused;
            let size_label = format_size(self.tail_total_bytes);
            let lines: Vec<AnyElement> = self
                .tail_text
                .lines()
                .map(|line| div().whitespace_nowrap().child(line.to_owned()).into_any_element())
                .collect();
            let empty = lines.is_empty();
            v_flex()
                .flex_1()
                .min_w_0()
                .h_full()
                .child(
                    h_flex()
                        .h(px(26.))
                        .flex_shrink_0()
                        .px_2()
                        .gap_1p5()
                        .items_center()
                        .border_b_1()
                        .border_color(theme.border)
                        .text_xs()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .font_family(theme.mono_font_family.clone())
                                .text_color(theme.muted_foreground)
                                .child(format!("{} ({size_label})", exp.log_path)),
                        )
                        .child(
                            // An icon, so the log path keeps some room when the panel is narrow.
                            Button::new("exp-tail-pause")
                                .ghost()
                                .xsmall()
                                .icon(if paused { IconName::Play } else { IconName::Pause })
                                .tooltip(tr(cx, if paused { "experiments.tailResume" } else { "experiments.tailPause" }))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.tail_paused = !this.tail_paused;
                                    if !this.tail_paused {
                                        this.tail_scroll.scroll_to_bottom();
                                    }
                                    cx.notify();
                                })),
                        )
                        .child(
                            Button::new("exp-tail-clear")
                                .ghost()
                                .xsmall()
                                .label(tr(cx, "experiments.tailClear"))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.tail_text.clear();
                                    this.tail_reset = false;
                                    cx.notify();
                                })),
                        )
                        .child(
                            Button::new("exp-tail-quote")
                                .ghost()
                                .xsmall()
                                .label(tr(cx, "experiments.tailSendToChat"))
                                .disabled(empty)
                                .on_click(cx.listener(|this, _, window, cx| this.quote_tail_to_chat(window, cx))),
                        ),
                )
                .when(self.tail_reset, |this| {
                    this.child(
                        div()
                            .px_2()
                            .py_0p5()
                            .bg(theme.warning.opacity(0.10))
                            .text_xs()
                            .text_color(crate::theme::warning_text(cx))
                            .child(tr(cx, "experiments.tailResetNotice")),
                    )
                })
                .child(
                    div()
                        .relative()
                        .flex_1()
                        .min_h_0()
                        .child(
                            v_flex()
                                .id("exp-tail-body")
                                .size_full()
                                .overflow_scroll()
                                .track_scroll(&self.tail_scroll)
                                .p_2()
                                .font_family(theme.mono_font_family.clone())
                                .text_xs()
                                .when(empty, |this| this.child(div().text_color(theme.muted_foreground).child(tr(cx, "experiments.tailEmpty"))))
                                .children(lines),
                        )
                        .vertical_scrollbar(&self.tail_scroll),
                )
                .into_any_element()
        } else {
            // Nothing selected: the list takes the whole width and says itself when it is empty.
            div().into_any_element()
        };

        v_flex()
            .size_full()
            .child(toolbar)
            // In a short panel the form shrinks and scrolls instead of pushing its buttons out of view.
            .children(form.map(|form| div().id("exp-form").h_auto().flex_shrink(1.).child(form).overflow_y_scrollbar()))
            .child(h_flex().flex_1().min_h_0().child(list_pane).child(tail_pane))
    }
}

fn now_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Append incoming log text while capping memory to `MAX_TAIL_LINES` lines and `MAX_TAIL_BYTES` bytes.
pub(crate) fn append_bounded_text(buf: &mut String, chunk: &str) {
    buf.push_str(chunk);
    if buf.len() > MAX_TAIL_BYTES {
        let mut cut = buf.len().saturating_sub(MAX_TAIL_BYTES);
        while cut < buf.len() && !buf.is_char_boundary(cut) {
            cut += 1;
        }
        if let Some(nl) = buf[cut..].find('\n') {
            cut += nl + 1;
        }
        buf.drain(..cut);
    }
    let line_count = buf.bytes().filter(|b| *b == b'\n').count();
    if line_count > MAX_TAIL_LINES {
        let drop_lines = line_count - MAX_TAIL_LINES;
        let mut seen = 0usize;
        let mut cut = 0usize;
        for (idx, b) in buf.bytes().enumerate() {
            if b == b'\n' {
                seen += 1;
                if seen == drop_lines {
                    cut = idx + 1;
                    break;
                }
            }
        }
        if cut > 0 && cut <= buf.len() {
            buf.drain(..cut);
        }
    }
}

pub(crate) fn format_size(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = 1024.0 * 1024.0;
    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
    let b = bytes as f64;
    if b >= GIB {
        format!("{:.2} GiB", b / GIB)
    } else if b >= MIB {
        format!("{:.1} MiB", b / MIB)
    } else if b >= KIB {
        format!("{:.1} KiB", b / KIB)
    } else {
        format!("{bytes} B")
    }
}

fn format_elapsed(ms: u64) -> String {
    let secs = ms / 1000;
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
    }
}

#[cfg(test)]
mod tests {
    use ::core::prelude::v1::test;

    use super::*;

    #[test]
    fn bounded_log_buffer_caps_lines_and_keeps_tail() {
        let mut buf = String::new();
        for i in 0..(MAX_TAIL_LINES + 200) {
            append_bounded_text(&mut buf, &format!("epoch line {i}\n"));
        }
        assert!(buf.lines().count() <= MAX_TAIL_LINES + 1);
        assert!(buf.contains(&format!("epoch line {}", MAX_TAIL_LINES + 199)));
        assert!(!buf.contains("epoch line 0\n"));
    }

    #[test]
    fn format_elapsed_and_size() {
        assert_eq!(format_elapsed(45_000), "45s");
        assert_eq!(format_elapsed(125_000), "2m 5s");
        assert_eq!(format_elapsed(7_500_000), "2h 5m");
        assert_eq!(format_size(512), "512 B");
        assert_eq!(format_size(2 * 1024 * 1024), "2.0 MiB");
    }
}
