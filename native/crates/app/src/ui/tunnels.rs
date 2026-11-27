//! Dynamic SSH `-L` port forwarding popover (status bar control, SSH targets only).
//! Manages local forwards through `carrier::ssh_tunnel::TunnelManager` without interrupting
//! the primary Host connection, and discovers remote listening ports via `host.ports`.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::popover::Popover;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _, Size, StyledExt as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use rainy_carrier::ssh_tunnel::{PortForwardRule, TunnelState};
use rainy_protocol::{HostPortsResponse, ListeningPort};
use serde_json::json;

use crate::i18n::{self, Locale};
use crate::store::{Store, StoreEvent};
use crate::ui::{motion, tr, trf};

/// Common research service presets offered as one-click pills.
const QUICK_PRESETS: [(u16, &str); 3] = [(6006, "TensorBoard"), (8888, "Jupyter"), (7860, "Gradio")];

/// Minimum cooldown between automatic remote port scans when opening the popover.
const SCAN_COOLDOWN: Duration = Duration::from_secs(5);

#[derive(PartialEq, Eq)]
struct StoreSig {
    is_ssh: bool,
    locale: Locale,
    state: String,
    rules: Vec<(String, u16, u16, Option<String>)>,
}

impl StoreSig {
    fn capture(store: &Store) -> Self {
        if !store.is_ssh() {
            return Self { is_ssh: false, locale: store.locale(), state: String::new(), rules: Vec::new() };
        }
        let state = format!("{:?}", store.tunnel_status.state);
        let rules = store
            .tunnel_status
            .rules
            .iter()
            .map(|r| (r.id.clone(), r.local_port, r.remote_port, r.label.clone()))
            .collect();
        Self { is_ssh: true, locale: store.locale(), state, rules }
    }
}

pub struct TunnelsView {
    store: Entity<Store>,
    store_sig: StoreSig,
    discovered: Vec<ListeningPort>,
    scanning: bool,
    last_scan: Option<Instant>,
    applying: bool,
    local_input: Entity<InputState>,
    remote_input: Entity<InputState>,
    label_input: Entity<InputState>,
    _subscriptions: Vec<Subscription>,
}

impl TunnelsView {
    pub fn new(store: Entity<Store>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let local_input = cx.new(|cx| InputState::new(window, cx).placeholder(tr(cx, "ssh.portsLocalPlaceholder")));
        let remote_input = cx.new(|cx| InputState::new(window, cx).placeholder(tr(cx, "ssh.portsRemotePlaceholder")));
        let label_input = cx.new(|cx| InputState::new(window, cx).placeholder(tr(cx, "ssh.portsLabelPlaceholder")));
        let store_sig = StoreSig::capture(store.read(cx));
        let subscriptions = vec![
            cx.observe(&store, |this, store, cx| {
                let store = store.read(cx);
                // Fast path on local Windows / WSL targets: never allocate or notify during chat streaming.
                if !store.is_ssh() && !this.store_sig.is_ssh {
                    return;
                }
                let next = StoreSig::capture(store);
                if next != this.store_sig {
                    this.store_sig = next;
                    cx.notify();
                }
            }),
            cx.subscribe_in(&store, window, |this, _, event: &StoreEvent, window, cx| match event {
                StoreEvent::HostReconnected(_) => {
                    this.discovered.clear();
                    this.scanning = false;
                    this.last_scan = None;
                    this.store_sig = StoreSig::capture(this.store.read(cx));
                    cx.notify();
                }
                StoreEvent::PrefsChanged => {
                    let local_ph = tr(cx, "ssh.portsLocalPlaceholder");
                    let remote_ph = tr(cx, "ssh.portsRemotePlaceholder");
                    let label_ph = tr(cx, "ssh.portsLabelPlaceholder");
                    this.local_input.update(cx, |s, cx| s.set_placeholder(local_ph, window, cx));
                    this.remote_input.update(cx, |s, cx| s.set_placeholder(remote_ph, window, cx));
                    this.label_input.update(cx, |s, cx| s.set_placeholder(label_ph, window, cx));
                }
                _ => {}
            }),
        ];
        Self {
            store,
            store_sig,
            discovered: Vec::new(),
            scanning: false,
            last_scan: None,
            applying: false,
            local_input,
            remote_input,
            label_input,
            _subscriptions: subscriptions,
        }
    }

    pub fn scan_ports(&mut self, cx: &mut Context<Self>) {
        if self.scanning || !self.store.read(cx).is_ssh() {
            return;
        }
        self.scanning = true;
        self.last_scan = Some(Instant::now());
        cx.notify();
        let call = self.store.read(cx).call("host.ports", json!({}));
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let result = call.await;
            let _ = this.update(cx, |this, cx| {
                this.scanning = false;
                if let Ok(Ok(resp)) = result.map(serde_json::from_value::<HostPortsResponse>) {
                    this.discovered = resp.ports;
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn apply_rules(
        &mut self,
        next_rules: Vec<PortForwardRule>,
        open_url: Option<String>,
        toast: Option<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.applying {
            return;
        }
        self.applying = true;
        cx.notify();
        let task = self.store.update(cx, |store, cx| store.apply_tunnel_rules(next_rules, cx));
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.applying = false;
                match result {
                    Ok(_) => {
                        if let Some(note) = toast {
                            window.push_notification(Notification::success(note), cx);
                        }
                        if let Some(url) = open_url {
                            open_in_browser(&url, cx);
                        }
                    }
                    Err(error) => {
                        let text = trf(cx, "ssh.portsFailed", &[("error", &error)]);
                        window.push_notification(Notification::error(text), cx);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn add_rule(&mut self, local: u16, remote: u16, label: Option<String>, open_after: bool, window: &mut Window, cx: &mut Context<Self>) {
        let mut rules = self.store.read(cx).tunnel_status.rules.clone();
        let mut rule = PortForwardRule::new(local, remote);
        rule.label = label;
        let url = rule.local_url();
        rules = merge_rule(rules, rule);
        let note = trf(cx, "ssh.portsApplied", &[("local", &local), ("remote", &remote)]);
        self.apply_rules(rules, open_after.then_some(url), Some(note), window, cx);
    }

    fn remove_rule(&mut self, id: &str, local: u16, window: &mut Window, cx: &mut Context<Self>) {
        let mut rules = self.store.read(cx).tunnel_status.rules.clone();
        rules.retain(|rule| rule.id != id);
        let note = trf(cx, "ssh.portsRemoved", &[("local", &local)]);
        self.apply_rules(rules, None, Some(note), window, cx);
    }

    fn add_from_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let local_str = self.local_input.read(cx).value().to_string();
        let remote_str = self.remote_input.read(cx).value().to_string();
        let label_str = self.label_input.read(cx).value().to_string();
        let Some(local) = parse_port(&local_str) else {
            window.push_notification(Notification::warning(tr(cx, "ssh.portsInvalidPort")), cx);
            return;
        };
        let remote = if remote_str.trim().is_empty() {
            local
        } else {
            match parse_port(&remote_str) {
                Some(port) => port,
                None => {
                    window.push_notification(Notification::warning(tr(cx, "ssh.portsInvalidPort")), cx);
                    return;
                }
            }
        };
        let label = (!label_str.trim().is_empty()).then(|| label_str.trim().to_owned());
        self.local_input.update(cx, |s, cx| s.set_value(String::new(), window, cx));
        self.remote_input.update(cx, |s, cx| s.set_value(String::new(), window, cx));
        self.label_input.update(cx, |s, cx| s.set_value(String::new(), window, cx));
        self.add_rule(local, remote, label, false, window, cx);
    }

    fn load_host_presets(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(profile) = self.store.read(cx).current_ssh_profile() else { return };
        if profile.forwards.is_empty() {
            return;
        }
        let mut rules = self.store.read(cx).tunnel_status.rules.clone();
        for preset in profile.forwards {
            rules = merge_rule(rules, preset);
        }
        self.apply_rules(rules, None, None, window, cx);
    }
}

impl Render for TunnelsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let store = self.store.read(cx);
        if !store.is_ssh() {
            return div().into_any_element();
        }
        let locale = store.locale();
        let status = store.tunnel_status.clone();
        let active_count = status.rules.len();
        let theme = cx.theme().clone();
        let dot_color = match &status.state {
            TunnelState::Active if active_count > 0 => Some(theme.success),
            TunnelState::Error(_) => Some(theme.danger),
            _ => None,
        };
        let label: SharedString = if active_count > 0 {
            trf(cx, "ssh.portsButtonCount", &[("count", &active_count)])
        } else {
            tr(cx, "ssh.portsButton")
        };
        let trigger = Button::new("ssh-tunnels-trigger")
            .ghost()
            .xsmall()
            .flex_shrink_0()
            .tooltip(tr(cx, "ssh.portsTooltip"))
            .child(
                h_flex()
                    .gap_1()
                    .items_center()
                    .text_xs()
                    .when_some(dot_color, |this, color| this.child(div().size(px(6.)).rounded_full().bg(color)))
                    .child(label),
            );

        let entity = cx.entity();
        Popover::new("ssh-tunnels-popover")
            .anchor(Anchor::BottomRight)
            .w(px(368.))
            .p_3()
            .rounded(theme.radius_lg)
            .trigger(trigger)
            .content(move |_, window, cx| {
                crate::web::mark_popover_open(cx);
                let entity_scan = entity.clone();
                let should_scan = {
                    let view = entity.read(cx);
                    !view.scanning && view.last_scan.is_none_or(|at| at.elapsed() >= SCAN_COOLDOWN)
                };
                if should_scan {
                    cx.defer(move |cx| {
                        entity_scan.update(cx, |view, cx| view.scan_ports(cx));
                    });
                }
                let max_h = (f32::from(window.viewport_size().height) - 220.).clamp(160., 360.);
                render_popover_body(entity.clone(), locale, max_h, cx)
            })
            .into_any_element()
    }
}

fn render_popover_body(entity: Entity<TunnelsView>, locale: Locale, max_height: f32, cx: &App) -> AnyElement {
    let view = entity.read(cx);
    let store = view.store.read(cx);
    let status = store.tunnel_status.clone();
    let host_presets_count = store.current_ssh_profile().map_or(0, |p| p.forwards.len());
    let discovered = view.discovered.clone();
    let scanning = view.scanning;
    let applying = view.applying;
    let local_input = view.local_input.clone();
    let remote_input = view.remote_input.clone();
    let label_input = view.label_input.clone();
    let theme = cx.theme().clone();

    let active_remotes: HashSet<u16> = status.rules.iter().map(|r| r.remote_port).collect();
    let used_locals: HashSet<u16> = status.rules.iter().map(|r| r.local_port).collect();

    let entity_refresh = entity.clone();
    let header = h_flex()
        .items_center()
        .gap_2()
        .child(div().flex_1().text_sm().font_semibold().child(i18n::text(locale, "ssh.portsTitle")))
        .when(scanning || applying, |this| this.child(motion::spinner(Size::XSmall, cx)))
        .child(
            Button::new("tunnels-refresh")
                .ghost()
                .xsmall()
                .icon(Icon::new(gpui_kit::assets::IconName::RotateCcw))
                .tooltip(i18n::text(locale, "ssh.portsRefresh"))
                .on_click(move |_, _, cx| {
                    entity_refresh.update(cx, |view, cx| view.scan_ports(cx));
                }),
        );

    let err_banner = match &status.state {
        TunnelState::Error(msg) => Some(
            div()
                .px_2()
                .py_1()
                .rounded(theme.radius)
                .bg(theme.danger.opacity(0.12))
                .text_xs()
                .text_color(theme.danger)
                .child(msg.clone()),
        ),
        _ => None,
    };

    // Active forwards list.
    let active_rows: Vec<AnyElement> = status
        .rules
        .iter()
        .enumerate()
        .map(|(idx, rule)| {
            let url = rule.local_url();
            let url_open = url.clone();
            let url_copy = url.clone();
            let rule_id = rule.id.clone();
            let local_port = rule.local_port;
            let entity_rm = entity.clone();
            let title = format!("localhost:{} → {}", rule.local_port, rule.remote_port);
            h_flex()
                .id(("active-tunnel", idx))
                .px_2()
                .py_1()
                .gap_1p5()
                .items_center()
                .rounded(theme.radius)
                .bg(theme.secondary.opacity(0.65))
                .text_xs()
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(div().font_family(theme.mono_font_family.clone()).truncate().child(title))
                        .when_some(rule.label.clone(), |this, label| {
                            this.child(div().text_color(theme.muted_foreground).truncate().child(label))
                        }),
                )
                .child(
                    Button::new(("tunnel-open", idx))
                        .ghost()
                        .xsmall()
                        .label(i18n::text(locale, "ssh.portsOpenBrowser"))
                        .on_click(move |_, _, cx| open_in_browser(&url_open, cx)),
                )
                .child(
                    Button::new(("tunnel-copy", idx))
                        .ghost()
                        .xsmall()
                        .icon(IconName::Copy)
                        .tooltip(i18n::text(locale, "ssh.portsCopyUrl"))
                        .on_click(move |_, window, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(url_copy.clone()));
                            window.push_notification(Notification::info(tr(cx, "external.copied")), cx);
                        }),
                )
                .child(
                    Button::new(("tunnel-stop", idx))
                        .ghost()
                        .xsmall()
                        .icon(IconName::Close)
                        .tooltip(i18n::text(locale, "ssh.portsStop"))
                        .disabled(applying)
                        .on_click(move |_, window, cx| {
                            entity_rm.update(cx, |view, cx| view.remove_rule(&rule_id, local_port, window, cx));
                        }),
                )
                .into_any_element()
        })
        .collect();

    // Discovered listening ports on remote host.
    let mut unforwarded: Vec<&ListeningPort> = discovered.iter().filter(|p| !active_remotes.contains(&p.port)).collect();
    // Only the first eight show: services the user runs (their process is known) on unprivileged ports come before
    // system ones such as DNS or mail, so TensorBoard on 6006 is not crowded out by port 25 and 53.
    unforwarded.sort_by_key(|p| (p.port < 1024, p.process_name.is_none() && p.label.is_none(), p.port));
    let discovered_rows: Vec<AnyElement> = unforwarded
        .iter()
        .take(8)
        .enumerate()
        .map(|(idx, port)| {
            let remote = port.port;
            let local = pick_local_port(remote, &used_locals);
            let label = port.label.clone().or_else(|| port.process_name.clone());
            let display_label = label.clone().unwrap_or_else(|| port.address.clone());
            let entity_add = entity.clone();
            h_flex()
                .id(("discovered-port", idx))
                .px_2()
                .py_1()
                .gap_2()
                .items_center()
                .rounded(theme.radius)
                .hover(|style| style.bg(theme.list_hover))
                .text_xs()
                .child(div().flex_shrink_0().font_family(theme.mono_font_family.clone()).font_medium().child(format!(":{remote}")))
                .child(div().flex_1().min_w_0().truncate().text_color(theme.muted_foreground).child(display_label))
                .child(
                    Button::new(("forward-open", idx))
                        .outline()
                        .xsmall()
                        .label(i18n::text(locale, "ssh.portsForwardAndOpen"))
                        .disabled(applying)
                        .on_click(move |_, window, cx| {
                            entity_add.update(cx, |view, cx| view.add_rule(local, remote, label.clone(), true, window, cx));
                        }),
                )
                .into_any_element()
        })
        .collect();

    // Quick presets + host profile presets.
    let entity_presets = entity.clone();
    let preset_pills = h_flex()
        .gap_1()
        .flex_wrap()
        .when(host_presets_count > 0, |this| {
            this.child(
                Button::new("load-host-presets")
                    .outline()
                    .xsmall()
                    .label(i18n::format(locale, "ssh.portsLoadHostPresets", &[("count", &host_presets_count)]))
                    .disabled(applying)
                    .on_click(move |_, window, cx| {
                        entity_presets.update(cx, |view, cx| view.load_host_presets(window, cx));
                    }),
            )
        })
        .children(QUICK_PRESETS.iter().enumerate().map(|(idx, (port, name))| {
            let remote = *port;
            let local = pick_local_port(remote, &used_locals);
            let entity_pill = entity.clone();
            let already = active_remotes.contains(&remote);
            Button::new(("quick-preset", idx))
                .ghost()
                .xsmall()
                .disabled(applying || already)
                .label(format!("{remote} {name}"))
                .on_click(move |_, window, cx| {
                    entity_pill.update(cx, |view, cx| view.add_rule(local, remote, Some((*name).to_owned()), false, window, cx));
                })
        }));

    // Manual add row.
    let entity_add = entity.clone();
    let add_form = v_flex()
        .pt_1p5()
        .border_t_1()
        .border_color(theme.border)
        .gap_1()
        .child(div().text_xs().font_medium().text_color(theme.muted_foreground).child(i18n::text(locale, "ssh.portsAddTitle")))
        .child(
            h_flex()
                .gap_1()
                .items_center()
                .child(div().w(px(76.)).child(Input::new(&local_input).xsmall()))
                .child(div().text_xs().text_color(theme.muted_foreground).child("→"))
                .child(div().w(px(76.)).child(Input::new(&remote_input).xsmall()))
                .child(div().flex_1().min_w_0().child(Input::new(&label_input).xsmall()))
                .child(
                    Button::new("tunnel-manual-add")
                        .primary()
                        .xsmall()
                        .label(i18n::text(locale, "ssh.portsAdd"))
                        .disabled(applying)
                        .on_click(move |_, window, cx| {
                            entity_add.update(cx, |view, cx| view.add_from_form(window, cx));
                        }),
                ),
        );

    v_flex()
        .w_full()
        .gap_2()
        .child(header)
        .children(err_banner)
        .child(
            v_flex()
                .id("tunnels-scroll")
                .max_h(px(max_height))
                .overflow_y_scroll()
                .gap_2()
                // Active section
                .child(
                    v_flex()
                        .gap_1()
                        .child(div().text_xs().font_medium().text_color(theme.muted_foreground).child(i18n::text(locale, "ssh.portsActive")))
                        .when(active_rows.is_empty(), |this| {
                            this.child(div().text_xs().text_color(theme.muted_foreground).child(i18n::text(locale, "ssh.portsNone")))
                        })
                        .children(active_rows),
                )
                // Discovered section
                .child(
                    v_flex()
                        .pt_1p5()
                        .border_t_1()
                        .border_color(theme.border)
                        .gap_1()
                        .child(div().text_xs().font_medium().text_color(theme.muted_foreground).child(i18n::text(locale, "ssh.portsDiscovered")))
                        .when(scanning && discovered_rows.is_empty(), |this| {
                            this.child(div().text_xs().text_color(theme.muted_foreground).child(i18n::text(locale, "ssh.portsDiscovering")))
                        })
                        .when(!scanning && discovered_rows.is_empty(), |this| {
                            this.child(div().text_xs().text_color(theme.muted_foreground).child(i18n::text(locale, "ssh.portsNoneDiscovered")))
                        })
                        .children(discovered_rows),
                )
                // Presets
                .child(
                    v_flex()
                        .pt_1p5()
                        .border_t_1()
                        .border_color(theme.border)
                        .gap_1()
                        .child(div().text_xs().font_medium().text_color(theme.muted_foreground).child(i18n::text(locale, "ssh.portsPresets")))
                        .child(preset_pills),
                ),
        )
        .child(add_form)
        .into_any_element()
}

fn parse_port(raw: &str) -> Option<u16> {
    let port = raw.trim().parse::<u16>().ok()?;
    (port >= 1).then_some(port)
}

/// Choose an available local port for `remote`, incrementing if already claimed by another rule.
fn pick_local_port(remote: u16, used_locals: &HashSet<u16>) -> u16 {
    let mut candidate = remote;
    for _ in 0..64 {
        if !used_locals.contains(&candidate) {
            return candidate;
        }
        candidate = candidate.saturating_add(1).max(1024);
    }
    remote
}

/// Insert or replace a forward rule matching the same local port or id.
fn merge_rule(mut rules: Vec<PortForwardRule>, rule: PortForwardRule) -> Vec<PortForwardRule> {
    rules.retain(|existing| existing.id != rule.id && existing.local_port != rule.local_port);
    rules.push(rule);
    rules
}

/// Open a local forwarded `http://127.0.0.1:<port>` or `http://localhost:<port>` URL via GPUI's shell handler.
fn open_in_browser(url: &str, cx: &App) {
    if !url.starts_with("http://127.0.0.1:") && !url.starts_with("http://localhost:") {
        return;
    }
    cx.open_url(url);
}

#[cfg(test)]
mod tests {
    use ::core::prelude::v1::test;

    use super::*;

    #[test]
    fn pick_local_port_avoids_existing_bindings() {
        let used: HashSet<u16> = [6006, 6007].into_iter().collect();
        assert_eq!(pick_local_port(6006, &used), 6008);
        assert_eq!(pick_local_port(8888, &used), 8888);
    }

    #[test]
    fn merge_rule_replaces_conflicting_local_port() {
        let mut r1 = PortForwardRule::new(6006, 6006);
        r1.label = Some("Old".into());
        let mut r2 = PortForwardRule::new(6006, 6007);
        r2.label = Some("New".into());
        let merged = merge_rule(vec![r1], r2);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].remote_port, 6007);
        assert_eq!(merged[0].label.as_deref(), Some("New"));
    }

    #[test]
    fn parse_port_rejects_zero_and_overflow() {
        assert_eq!(parse_port("0"), None);
        assert_eq!(parse_port("65536"), None);
        assert_eq!(parse_port(" 6006 "), Some(6006));
    }
}
