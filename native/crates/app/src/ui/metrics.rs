//! Compact status-bar resource strip and detail popover for remote (SSH / WSL) hosts.
//! Polls `host.metrics` every 5 seconds only while the window is in the foreground and connected
//! to a remote target (zero polling in background or on local Windows).

use std::collections::HashSet;
use std::time::{Duration, Instant};

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::popover::Popover;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, StyledExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use rainy_carrier::ConnectionState;
use rainy_protocol::{GpuInfo, HostMetrics};
use serde_json::json;

use crate::i18n::{self, Locale};
use crate::store::ide::{Ide, IdeEvent};
use crate::store::{Store, StoreEvent};
use crate::ui::tr;

/// Foreground polling interval for lightweight status-bar metrics.
const POLL_INTERVAL: Duration = Duration::from_secs(5);
/// Minimum gap between automatic detailed refreshes when opening the popover.
const DETAIL_TTL: Duration = Duration::from_secs(4);
/// GPUs at or below this memory footprint and utilization are counted as idle in the summary.
const GPU_IDLE_MEM_MIB: u64 = 1024;
const GPU_IDLE_UTIL_PCT: u32 = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Severity {
    Normal,
    Warning,
    Danger,
}

pub struct MetricsView {
    store: Entity<Store>,
    ide: Entity<Ide>,
    visible: bool,
    window_active: bool,
    /// Last observed `(is_remote, connection, has_rpc, locale)` so streaming tokens do not wake this view.
    store_sig: (bool, ConnectionState, bool, Locale),
    loading_detail: bool,
    last_detail_attempt: Option<Instant>,
    detailed: Option<(Instant, HostMetrics)>,
    error: Option<SharedString>,
    poll: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl MetricsView {
    pub fn new(store: Entity<Store>, ide: Entity<Ide>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let window_active = window.is_window_active();
        let store_sig = Self::store_signature(store.read(cx));
        let subscriptions = vec![
            cx.subscribe(&store, |this, _, event: &StoreEvent, cx| match event {
                StoreEvent::HostReconnected(_) => {
                    this.detailed = None;
                    this.last_detail_attempt = None;
                    this.error = None;
                    this.store_sig = Self::store_signature(this.store.read(cx));
                    this.sync_polling(cx);
                    cx.notify();
                }
                StoreEvent::HostExited(_) => {
                    this.poll = None;
                    this.detailed = None;
                    this.last_detail_attempt = None;
                    this.store_sig = Self::store_signature(this.store.read(cx));
                    cx.notify();
                }
                _ => {}
            }),
            cx.observe(&store, |this, store, cx| {
                let sig = Self::store_signature(store.read(cx));
                if sig != this.store_sig {
                    this.store_sig = sig;
                    this.sync_polling(cx);
                    cx.notify();
                }
            }),
            cx.subscribe(&ide, |this, _, event: &IdeEvent, cx| {
                if *event == IdeEvent::WorkspaceChanged {
                    this.detailed = None;
                    this.last_detail_attempt = None;
                    if this.should_poll(cx) {
                        this.fetch(false, cx);
                    }
                }
            }),
        ];
        let mut view = Self {
            store,
            ide,
            visible: true,
            window_active,
            store_sig,
            loading_detail: false,
            last_detail_attempt: None,
            detailed: None,
            error: None,
            poll: None,
            _subscriptions: subscriptions,
        };
        view.sync_polling(cx);
        view
    }

    fn store_signature(store: &Store) -> (bool, ConnectionState, bool, Locale) {
        (store.is_remote(), store.connection, store.rpc.is_some(), store.locale())
    }

    /// Pause polling while the status bar is hidden (for example in Agent layout); resume when visible again.
    pub fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if self.visible == visible {
            return;
        }
        self.visible = visible;
        self.sync_polling(cx);
    }

    /// Pause polling while the window is in the background; resume immediately on foreground focus.
    pub fn set_window_active(&mut self, active: bool, cx: &mut Context<Self>) {
        if self.window_active == active {
            return;
        }
        self.window_active = active;
        self.sync_polling(cx);
    }

    fn should_poll(&self, cx: &App) -> bool {
        let store = self.store.read(cx);
        self.visible && self.window_active && store.is_remote() && store.connection == ConnectionState::Open && store.rpc.is_some()
    }

    fn sync_polling(&mut self, cx: &mut Context<Self>) {
        if !self.should_poll(cx) {
            if self.poll.is_some() {
                self.poll = None;
            }
            return;
        }
        if self.poll.is_some() {
            return;
        }
        self.fetch(false, cx);
        self.poll = Some(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            loop {
                cx.background_executor().timer(POLL_INTERVAL).await;
                let keep_going = this
                    .update(cx, |this, cx| {
                        if !this.should_poll(cx) {
                            this.poll = None;
                            return false;
                        }
                        this.fetch(false, cx);
                        true
                    })
                    .unwrap_or(false);
                if !keep_going {
                    break;
                }
            }
        }));
    }

    /// Collect workspace root and SSH host bookmark paths to watch disk usage for.
    fn watch_paths(&self, cx: &App) -> Vec<String> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        let mut push = |path: String| {
            let trimmed = path.trim();
            if !trimmed.is_empty() && seen.insert(trimmed.to_owned()) {
                out.push(trimmed.to_owned());
            }
        };
        if let Some(ws) = &self.ide.read(cx).workspace {
            push(ws.path.clone());
        }
        if let Some(profile) = self.store.read(cx).current_ssh_profile() {
            if let Some(default_ws) = profile.default_workspace {
                push(default_ws);
            }
            for bookmark in profile.bookmarks {
                push(bookmark);
            }
        }
        out
    }

    /// Fetch `host.metrics` from the Host; `include_processes = true` is requested on-demand when the popover opens.
    pub fn fetch(&mut self, include_processes: bool, cx: &mut Context<Self>) {
        let store = self.store.read(cx);
        if !store.is_remote() || store.connection != ConnectionState::Open {
            return;
        }
        let target_tag = store.target.as_ref().map(|t| format!("{t:?}"));
        let locale = store.locale();
        let watch_paths = self.watch_paths(cx);
        let call = self.store.read(cx).call(
            "host.metrics",
            json!({
                "watchPaths": watch_paths,
                "includeGpuProcesses": include_processes,
            }),
        );
        if include_processes {
            self.loading_detail = true;
            self.last_detail_attempt = Some(Instant::now());
            cx.notify();
        }
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let result = call.await;
            let _ = this.update(cx, |this, cx| {
                // Drop stale metrics if the user hot-swapped targets while this RPC was in flight.
                let current_tag = this.store.read(cx).target.as_ref().map(|t| format!("{t:?}"));
                if current_tag != target_tag || !this.store.read(cx).is_remote() {
                    if include_processes {
                        this.loading_detail = false;
                        cx.notify();
                    }
                    return;
                }
                if include_processes {
                    this.loading_detail = false;
                }
                match result.and_then(|value| {
                    serde_json::from_value::<HostMetrics>(value)
                        .map_err(|error| rainy_protocol::RpcError { code: "invalid".into(), message: error.to_string(), data: None })
                }) {
                    Ok(metrics) => {
                        this.error = None;
                        if include_processes {
                            this.detailed = Some((Instant::now(), metrics.clone()));
                        }
                        this.store.update(cx, |store, _| {
                            store.host_metrics = Some(metrics);
                        });
                        cx.notify();
                    }
                    Err(error) => {
                        if include_processes {
                            this.error = Some(i18n::error_text(locale, &error).into());
                            cx.notify();
                        }
                    }
                }
            });
        })
        .detach();
    }

    fn ensure_detail_fresh(&mut self, cx: &mut Context<Self>) {
        if self.loading_detail {
            return;
        }
        let cooled = self.last_detail_attempt.is_none_or(|at| at.elapsed() >= DETAIL_TTL);
        let stale = self.detailed.as_ref().is_none_or(|(at, _)| at.elapsed() >= DETAIL_TTL);
        if cooled && stale {
            self.fetch(true, cx);
        }
    }
}

impl Render for MetricsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let store = self.store.read(cx);
        if !store.is_remote() || store.connection != ConnectionState::Open {
            return div().into_any_element();
        }
        let locale = store.locale();
        let metrics = store.host_metrics.clone();
        let theme = cx.theme().clone();
        let (summary, severity) = match &metrics {
            Some(m) => (format_compact_summary(m, locale), max_severity(m)),
            None => (tr(cx, "metrics.loading").to_string(), Severity::Normal),
        };
        let text_color = match severity {
            Severity::Danger => theme.danger,
            Severity::Warning => crate::theme::warning_text(cx),
            Severity::Normal => theme.muted_foreground,
        };
        let trigger = Button::new("host-metrics-trigger")
            .ghost()
            .xsmall()
            .min_w_0()
            .flex_shrink_1()
            .max_w(px(340.))
            .tooltip(tr(cx, "metrics.tooltip"))
            .child(
                h_flex()
                    .min_w_0()
                    .gap_1()
                    .items_center()
                    .text_xs()
                    .text_color(text_color)
                    .child(div().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().child(summary)),
            );

        let entity = cx.entity();
        let notes = store
            .current_ssh_profile()
            .and_then(|profile| profile.notes)
            .filter(|notes| !notes.trim().is_empty());

        Popover::new("host-metrics-popover")
            .anchor(Anchor::BottomRight)
            .w(px(380.))
            .p_3()
            .rounded(theme.radius_lg)
            .trigger(trigger)
            .content(move |_, window, cx| {
                crate::web::mark_popover_open(cx);
                let entity_refresh = entity.clone();
                cx.defer(move |cx| {
                    entity_refresh.update(cx, |view, cx| view.ensure_detail_fresh(cx));
                });
                let view = entity.read(cx);
                let snapshot = view
                    .detailed
                    .as_ref()
                    .map(|(_, m)| m.clone())
                    .or_else(|| view.store.read(cx).host_metrics.clone());
                let max_h = (f32::from(window.viewport_size().height) - 160.).clamp(200., 460.);
                render_popover_body(
                    entity.clone(),
                    snapshot.as_ref(),
                    view.loading_detail,
                    view.error.clone(),
                    notes.clone(),
                    locale,
                    max_h,
                    cx,
                )
            })
            .into_any_element()
    }
}

#[allow(clippy::too_many_arguments)]
fn render_popover_body(
    entity: Entity<MetricsView>,
    metrics: Option<&HostMetrics>,
    loading: bool,
    error: Option<SharedString>,
    notes: Option<String>,
    locale: Locale,
    max_height: f32,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme().clone();
    let header = h_flex()
        .items_center()
        .gap_2()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .text_sm()
                .font_semibold()
                .child(match metrics {
                    Some(m) if !m.hostname.is_empty() => format!("{} · {}", i18n::text(locale, "metrics.title"), m.hostname),
                    _ => i18n::text(locale, "metrics.title").to_owned(),
                }),
        )
        .when(loading, |this| this.child(Spinner::new().xsmall()))
        .child(
            Button::new("metrics-refresh")
                .ghost()
                .xsmall()
                .icon(Icon::new(gpui_kit::assets::IconName::RotateCcw))
                .tooltip(i18n::text(locale, "metrics.refresh"))
                .on_click(move |_, _, cx| {
                    entity.update(cx, |view, cx| view.fetch(true, cx));
                }),
        );

    let Some(m) = metrics else {
        return v_flex()
            .w_full()
            .gap_2()
            .child(header)
            .child(
                div()
                    .text_xs()
                    .text_color(if error.is_some() { theme.danger } else { theme.muted_foreground })
                    .child(error.unwrap_or_else(|| i18n::text(locale, "metrics.unavailable").into())),
            )
            .into_any_element();
    };

    // CPU & cgroup summary line.
    let effective = format_cores(m.effective_cpu_cores);
    let cpu_label = if m.cgroup.as_ref().and_then(|c| c.cpu_quota_cores).is_some() {
        i18n::format(locale, "metrics.cgroupCpu", &[("effective", &effective), ("host", &m.cpu_count)])
    } else {
        i18n::format(locale, "metrics.hostCpu", &[("cores", &m.cpu_count)])
    };
    let load_label = m.load_avg_1m.map(|load| i18n::format(locale, "metrics.load1m", &[("load", &format!("{load:.2}"))]));
    let cpu_pct = m.cpu_percent.map(|pct| format!("{pct:.0}%"));

    // Memory bar.
    let mem_ratio = ratio(m.memory_used_bytes as f64, m.memory_total_bytes as f64);
    let mem_sev = severity_of(mem_ratio);
    let mem_cgroup_tag = m
        .cgroup
        .as_ref()
        .filter(|c| c.memory_limit_bytes.is_some())
        .map(|c| i18n::format(locale, "metrics.cgroupMemoryHint", &[("version", &c.version)]));

    let mut content = v_flex()
        .id("metrics-popover-scroll")
        .max_h(px(max_height))
        .overflow_y_scroll()
        .gap_2p5()
        // CPU row
        .child(
            h_flex()
                .items_center()
                .gap_2()
                .text_xs()
                .child(div().flex_1().min_w_0().font_medium().child(cpu_label))
                .when_some(cpu_pct, |this, pct| {
                    this.child(div().font_family(theme.mono_font_family.clone()).text_color(theme.muted_foreground).child(pct))
                })
                .when_some(load_label, |this, load| {
                    this.child(div().text_color(theme.muted_foreground).child(load))
                }),
        )
        // Memory block
        .child(
            v_flex()
                .gap_1()
                .child(
                    h_flex()
                        .items_center()
                        .gap_1p5()
                        .text_xs()
                        .child(div().font_medium().child(i18n::text(locale, "metrics.memoryTitle")))
                        .when_some(mem_cgroup_tag, |this, tag| {
                            this.child(
                                div()
                                    .px_1()
                                    .rounded(theme.radius)
                                    .bg(theme.secondary)
                                    .text_color(theme.muted_foreground)
                                    .child(tag),
                            )
                        })
                        .child(div().flex_1())
                        .child(
                            div()
                                .font_family(theme.mono_font_family.clone())
                                .text_color(severity_color(mem_sev, cx))
                                .child(format!("{} / {}", format_bytes(m.memory_used_bytes), format_bytes(m.memory_total_bytes))),
                        ),
                )
                .child(usage_bar(mem_ratio, mem_sev, cx)),
        );

    // GPUs section.
    if !m.gpus.is_empty() {
        content = content.child(
            v_flex()
                .pt_1p5()
                .border_t_1()
                .border_color(theme.border)
                .gap_1p5()
                .child(
                    h_flex()
                        .items_center()
                        .justify_between()
                        .text_xs()
                        .child(div().font_semibold().child(i18n::text(locale, "metrics.gpusTitle")))
                        .child(
                            div()
                                .text_color(theme.muted_foreground)
                                .child(i18n::format(locale, "metrics.gpuFreeSummary", &[("free", &idle_gpu_count(&m.gpus)), ("total", &m.gpus.len())])),
                        ),
                )
                .children(m.gpus.iter().map(|gpu| render_gpu_row(gpu, locale, cx))),
        );
    }

    // Disks section.
    if !m.disks.is_empty() {
        content = content.child(
            v_flex()
                .pt_1p5()
                .border_t_1()
                .border_color(theme.border)
                .gap_1p5()
                .child(div().text_xs().font_semibold().child(i18n::text(locale, "metrics.disksTitle")))
                .children(m.disks.iter().map(|disk| {
                    let used = disk.total_bytes.saturating_sub(disk.available_bytes);
                    let r = ratio(used as f64, disk.total_bytes as f64);
                    let sev = severity_of(r);
                    let avail = format_bytes(disk.available_bytes);
                    let total = format_bytes(disk.total_bytes);
                    v_flex()
                        .gap_0p5()
                        .child(
                            h_flex()
                                .items_center()
                                .gap_2()
                                .text_xs()
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .overflow_hidden()
                                        .text_ellipsis()
                                        .whitespace_nowrap()
                                        .font_family(theme.mono_font_family.clone())
                                        .child(disk.path.clone()),
                                )
                                .child(
                                    div()
                                        .flex_shrink_0()
                                        .font_family(theme.mono_font_family.clone())
                                        .text_color(severity_color(sev, cx))
                                        .child(i18n::format(locale, "metrics.diskAvail", &[("avail", &avail), ("total", &total)])),
                                ),
                        )
                        .child(usage_bar(r, sev, cx))
                })),
        );
    }

    // Optional SSH host safety notes from the host research profile.
    if let Some(notes) = notes {
        content = content.child(
            div()
                .pt_1p5()
                .border_t_1()
                .border_color(theme.border)
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(i18n::format(locale, "metrics.hostNotes", &[("notes", &notes)])),
        );
    }

    v_flex().w_full().gap_2().child(header).child(content).into_any_element()
}

fn render_gpu_row(gpu: &GpuInfo, locale: Locale, cx: &App) -> impl IntoElement + use<> {
    let theme = cx.theme();
    let mem_ratio = ratio(gpu.memory_used_mib as f64, gpu.memory_total_mib as f64);
    let sev = severity_of(mem_ratio);
    let idle = is_gpu_idle(gpu);
    let status_text = i18n::text(locale, if idle { "metrics.gpuIdle" } else { "metrics.gpuBusy" });
    let status_color = if idle { theme.success } else { severity_color(sev, cx) };
    let mut stat_parts = Vec::new();
    if let Some(util) = gpu.utilization_percent {
        stat_parts.push(format!("{util}%"));
    }
    if let Some(temp) = gpu.temperature_c {
        stat_parts.push(format!("{temp}°C"));
    }
    stat_parts.push(format!("{:.1}/{:.0}G", gpu.memory_used_mib as f64 / 1024.0, gpu.memory_total_mib as f64 / 1024.0));
    let stats = stat_parts.join(" · ");
    let procs = gpu.processes.clone();

    v_flex()
        .p_1p5()
        .gap_1()
        .rounded(theme.radius)
        .bg(theme.secondary.opacity(0.6))
        .child(
            h_flex()
                .items_center()
                .gap_1p5()
                .text_xs()
                .child(div().font_semibold().flex_shrink_0().child(format!("#{}", gpu.index)))
                .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().child(gpu.name.clone()))
                .child(div().flex_shrink_0().text_color(status_color).child(status_text))
                .child(div().flex_shrink_0().font_family(theme.mono_font_family.clone()).text_color(theme.muted_foreground).child(stats)),
        )
        .child(usage_bar(mem_ratio, sev, cx))
        .when(procs.is_empty(), |this| {
            this.child(div().text_xs().text_color(theme.muted_foreground).child(i18n::text(locale, "metrics.gpuNoProcesses")))
        })
        .when(!procs.is_empty(), |this| {
            this.child(v_flex().gap_0p5().children(procs.into_iter().take(4).map(|proc| {
                let user = proc.user.as_deref().unwrap_or("-");
                let line = format!("{user} · pid {} · {}", proc.pid, proc.process_name);
                let mem = format!("{} MiB", proc.used_memory_mib);
                h_flex()
                    .items_center()
                    .gap_2()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().font_family(theme.mono_font_family.clone()).child(line))
                    .child(div().flex_shrink_0().font_family(theme.mono_font_family.clone()).child(mem))
            })))
        })
}

fn usage_bar(ratio: f32, severity: Severity, cx: &App) -> impl IntoElement + use<> {
    let theme = cx.theme();
    let clamped = ratio.clamp(0.0, 1.0);
    let fill = match severity {
        Severity::Danger => theme.danger,
        Severity::Warning => theme.warning,
        Severity::Normal => theme.primary.opacity(0.75),
    };
    div()
        .w_full()
        .h(px(4.))
        .rounded_full()
        .overflow_hidden()
        .bg(theme.muted)
        .child(div().h_full().w(relative(clamped)).bg(fill))
}

fn severity_color(severity: Severity, cx: &App) -> Hsla {
    match severity {
        Severity::Danger => cx.theme().danger,
        Severity::Warning => crate::theme::warning_text(cx),
        Severity::Normal => cx.theme().muted_foreground,
    }
}

fn ratio(used: f64, total: f64) -> f32 {
    if total <= 0.0 {
        0.0
    } else {
        (used / total).clamp(0.0, 1.0) as f32
    }
}

fn severity_of(ratio: f32) -> Severity {
    if ratio >= 0.95 {
        Severity::Danger
    } else if ratio >= 0.85 {
        Severity::Warning
    } else {
        Severity::Normal
    }
}

fn is_gpu_idle(gpu: &GpuInfo) -> bool {
    gpu.memory_used_mib <= GPU_IDLE_MEM_MIB && gpu.utilization_percent.unwrap_or(0) <= GPU_IDLE_UTIL_PCT
}

fn idle_gpu_count(gpus: &[GpuInfo]) -> usize {
    gpus.iter().filter(|gpu| is_gpu_idle(gpu)).count()
}

fn max_severity(m: &HostMetrics) -> Severity {
    let mut worst = ratio(m.memory_used_bytes as f64, m.memory_total_bytes as f64);
    for disk in &m.disks {
        let used = disk.total_bytes.saturating_sub(disk.available_bytes);
        worst = worst.max(ratio(used as f64, disk.total_bytes as f64));
    }
    for gpu in &m.gpus {
        worst = worst.max(ratio(gpu.memory_used_mib as f64, gpu.memory_total_mib as f64));
    }
    severity_of(worst)
}

fn format_cores(cores: f64) -> String {
    if (cores - cores.round()).abs() < 0.05 {
        format!("{}", cores.round() as u64)
    } else {
        format!("{cores:.1}")
    }
}

fn format_bytes(bytes: u64) -> String {
    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
    const MIB: f64 = 1024.0 * 1024.0;
    const TIB: f64 = GIB * 1024.0;
    let b = bytes as f64;
    if b >= TIB {
        format!("{:.1}T", b / TIB)
    } else if b >= 10.0 * GIB {
        format!("{:.0}G", b / GIB)
    } else if b >= GIB {
        format!("{:.1}G", b / GIB)
    } else {
        format!("{:.0}M", b / MIB)
    }
}

/// Compact single-line status-bar text, e.g. `GPU 4/6空闲 · 14/36C · 48/192G`.
fn format_compact_summary(m: &HostMetrics, locale: Locale) -> String {
    let mut parts = Vec::new();
    if !m.gpus.is_empty() {
        let free = idle_gpu_count(&m.gpus);
        parts.push(i18n::format(locale, "metrics.gpuFreeSummary", &[("free", &free), ("total", &m.gpus.len())]));
    }
    let effective = m.effective_cpu_cores.max(1.0);
    if let Some(pct) = m.cpu_percent {
        let busy_cores = (effective * (pct / 100.0)).round() as u64;
        parts.push(format!("{busy_cores}/{}C", format_cores(effective)));
    } else {
        parts.push(format!("{}C", format_cores(effective)));
    }
    if m.memory_total_bytes > 0 {
        parts.push(format!("{}/{}", format_bytes(m.memory_used_bytes), format_bytes(m.memory_total_bytes)));
    }
    parts.join(" · ")
}

#[cfg(test)]
mod tests {
    use ::core::prelude::v1::test;

    use super::*;
    use rainy_protocol::{CgroupLimits, DiskUsage};

    fn sample_gpu(index: u32, mem_used: u64, mem_total: u64, util: u32) -> GpuInfo {
        GpuInfo {
            index,
            name: "L20".into(),
            utilization_percent: Some(util),
            memory_used_mib: mem_used,
            memory_total_mib: mem_total,
            temperature_c: Some(45),
            processes: Vec::new(),
        }
    }

    #[test]
    fn compact_summary_formats_gpus_cpu_and_memory() {
        let metrics = HostMetrics {
            collected_at: 1,
            hostname: "gpu-node".into(),
            platform: "linux".into(),
            cpu_count: 128,
            effective_cpu_cores: 36.0,
            cpu_percent: Some(39.0),
            load_avg_1m: Some(8.5),
            memory_used_bytes: 48 * 1024 * 1024 * 1024,
            memory_total_bytes: 192 * 1024 * 1024 * 1024,
            host_memory_total_bytes: 512 * 1024 * 1024 * 1024,
            cgroup: Some(CgroupLimits {
                version: 2,
                cpu_quota_cores: Some(36.0),
                memory_limit_bytes: Some(192 * 1024 * 1024 * 1024),
                memory_current_bytes: Some(48 * 1024 * 1024 * 1024),
            }),
            gpus: vec![
                sample_gpu(0, 128, 46080, 0),
                sample_gpu(1, 20480, 46080, 92),
                sample_gpu(2, 0, 46080, 2),
            ],
            disks: vec![DiskUsage {
                path: "/root/kube-user".into(),
                mount: Some("/root/kube-user".into()),
                available_bytes: 200 * 1024 * 1024 * 1024,
                total_bytes: 500 * 1024 * 1024 * 1024,
            }],
        };
        let zh = format_compact_summary(&metrics, Locale::Zh);
        assert!(zh.contains("GPU 2/3空闲"));
        assert!(zh.contains("14/36C"));
        assert!(zh.contains("48G/192G"));
        assert_eq!(max_severity(&metrics), Severity::Normal);
    }

    #[test]
    fn severity_escalates_at_85_and_95_percent() {
        assert_eq!(severity_of(0.84), Severity::Normal);
        assert_eq!(severity_of(0.85), Severity::Warning);
        assert_eq!(severity_of(0.94), Severity::Warning);
        assert_eq!(severity_of(0.95), Severity::Danger);
    }
}
