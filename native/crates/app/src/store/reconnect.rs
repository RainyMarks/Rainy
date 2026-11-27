//! Silent SSH auto-reconnect, manual SSH reconnect, and in-place execution target hot-swap state machine.

use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use gpui_kit::*;
use rainy_carrier::ssh_tunnel::{PortForwardRule, TunnelState, TunnelStatus};
use rainy_carrier::{ConnectionState, ExecutionTarget, HostProcess, RpcClient, RpcEvent};

use crate::boot;
use crate::store::{QUIT_FLUSH_TIMEOUT, RuntimePrompt, SSH_RECONNECT_BACKOFFS_SECS, SshAutoReconnect, Store, StoreEvent, ide};

/// Freshly connected Host/RPC bundle handed into [`Store::install_connected_host`].
struct ConnectedHost {
    generation: u64,
    target: ExecutionTarget,
    host: Arc<HostProcess>,
    rpc: RpcClient,
    rpc_events: async_channel::Receiver<RpcEvent>,
    exit_rx: async_channel::Receiver<Option<i32>>,
    same_target: bool,
    restore_tunnels: Vec<PortForwardRule>,
}

/// Tear down an obsolete Host/RPC pair when a newer generation superseded it before installation.
fn drop_stale_host(host: Arc<HostProcess>, rpc: RpcClient, cx: &mut AsyncApp) {
    rpc.close();
    cx.background_spawn(async move {
        host.stop();
    })
    .detach();
}

impl Store {
    /// Return true when an SSH failure should alert the user immediately instead of retrying in the background.
    fn is_non_retryable_ssh_error(detail: &str) -> bool {
        let lower = detail.to_ascii_lowercase();
        [
            "permission denied",
            "authentication failed",
            "host key verification failed",
            "bad configuration",
            "cancelled",
            "canceled",
        ]
        .iter()
        .any(|needle| lower.contains(needle))
    }

    /// Install a freshly connected Host/RPC pair, refresh all subsystems via `StoreEvent::HostReconnected`,
    /// and restore or auto-start SSH port-forwarding tunnels.
    fn install_connected_host(&mut self, connected: ConnectedHost, cx: &mut Context<Self>) {
        self.reconnecting = false;
        self.swapping_target = None;
        self.ssh_reconnect = SshAutoReconnect::Idle;
        self.ssh_retry_now = None;
        self.ssh_reconnect_task = None;
        self.host = Some(connected.host);
        self.target = Some(connected.target.clone());
        self.rpc = Some(connected.rpc);
        self.pending_reconnected = true;
        if !connected.same_target {
            self.summaries.clear();
            self.chats.clear();
            self.chat_used.clear();
            self.reading.clear();
            self.selected = None;
            self.selections += 1;
            self.models = None;
            self.external_agents.clear();
            self.pending_external = None;
            self.pending_external_model = None;
            self.clear_all_teams();
            self.host_metrics = None;
            self.tunnel_status = TunnelStatus { target: None, rules: Vec::new(), state: TunnelState::Stopped };
            cx.emit(StoreEvent::SummariesChanged);
            cx.emit(StoreEvent::SelectionChanged);
        }
        self.spawn_exit_watcher(connected.generation, connected.exit_rx, cx);
        self.spawn_rpc_pump(connected.generation, connected.rpc_events, cx);
        if matches!(connected.target, ExecutionTarget::Ssh { .. }) {
            if connected.same_target && !connected.restore_tunnels.is_empty() {
                self.apply_tunnel_rules(connected.restore_tunnels, cx).detach();
            } else {
                self.start_auto_tunnels(cx);
            }
        }
        cx.notify();
    }

    /// Cancel any background silent SSH reconnect countdown without touching the active Host.
    pub fn cancel_ssh_auto_reconnect(&mut self, cx: &mut Context<Self>) {
        self.ssh_reconnect_task = None;
        self.ssh_retry_now = None;
        if self.ssh_reconnect != SshAutoReconnect::Idle {
            self.ssh_reconnect = SshAutoReconnect::Idle;
            cx.notify();
        }
    }

    /// Wake a sleeping SSH reconnect countdown immediately, or start a fresh reconnect if idle/exhausted.
    pub fn retry_ssh_now(&mut self, window: AnyWindowHandle, cx: &mut Context<Self>) {
        match &self.ssh_reconnect {
            SshAutoReconnect::Waiting { .. } => {
                if let Some(tx) = &self.ssh_retry_now {
                    let _ = tx.try_send(());
                }
            }
            SshAutoReconnect::Attempting { .. } => {}
            SshAutoReconnect::Idle | SshAutoReconnect::Exhausted { .. } => {
                self.reconnect_ssh(window, cx);
            }
        }
    }

    /// Non-blocking silent SSH reconnect with `1s -> 3s -> 8s` backoff. Alerts the user via dialog only if
    /// interactive auth fails or all 3 attempts are exhausted.
    pub fn start_ssh_auto_reconnect(&mut self, window: AnyWindowHandle, cx: &mut Context<Self>) {
        if self.quitting || self.stopping || self.reconnecting {
            return;
        }
        if matches!(self.ssh_reconnect, SshAutoReconnect::Waiting { .. } | SshAutoReconnect::Attempting { .. }) {
            return;
        }
        let Some(target @ ExecutionTarget::Ssh { .. }) = self.target.clone() else {
            return;
        };
        let Some(askpass) = self.ensure_askpass() else {
            self.restart(cx);
            return;
        };
        let saved_tunnels = self.tunnel_status.rules.clone();
        let tunnels = self.tunnels.clone();
        self.tunnel_status = TunnelStatus { target: None, rules: Vec::new(), state: TunnelState::Stopped };
        cx.background_spawn(async move { tunnels.stop_all() }).detach();

        if let Some(rpc) = self.rpc.take() {
            rpc.close();
        }
        let previous = self
            .host
            .take()
            .and_then(|host| host.remote_ssh().map(|remote| (remote.host.clone(), host.ready().pid)));
        self.connection = ConnectionState::Closed;

        let (wake_tx, wake_rx) = async_channel::bounded::<()>(1);
        self.ssh_retry_now = Some(wake_tx);
        let start_generation = self.host_generation;
        let paths = self.paths.clone();
        let modules = self.modules.manager.clone();
        let locale = self.locale();
        let label = target.label().to_owned();

        let task = cx.spawn(async move |this: WeakEntity<Store>, cx: &mut AsyncApp| {
            let max = SSH_RECONNECT_BACKOFFS_SECS.len() as u8;
            let mut last_error: Option<String> = None;

            for (idx, delay_secs) in SSH_RECONNECT_BACKOFFS_SECS.iter().copied().enumerate() {
                let attempt = (idx + 1) as u8;
                let retry_at = Instant::now() + Duration::from_secs(delay_secs);
                let alive = this
                    .update(cx, |store, cx| {
                        if store.stopping || store.quitting || store.host_generation != start_generation {
                            return false;
                        }
                        store.ssh_reconnect = SshAutoReconnect::Waiting {
                            attempt,
                            max,
                            retry_at,
                            last_error: last_error.clone(),
                        };
                        cx.notify();
                        true
                    })
                    .unwrap_or(false);
                if !alive {
                    return;
                }

                // Wait until `retry_at` or an immediate wake signal from "Retry now", ticking so the countdown updates.
                while Instant::now() < retry_at {
                    if wake_rx.try_recv().is_ok() {
                        break;
                    }
                    let remaining = retry_at.saturating_duration_since(Instant::now());
                    cx.background_executor().timer(remaining.min(Duration::from_millis(250))).await;
                    let still_same = this
                        .update(cx, |store, cx| {
                            if store.stopping || store.quitting || store.host_generation != start_generation {
                                return false;
                            }
                            cx.notify();
                            true
                        })
                        .unwrap_or(false);
                    if !still_same {
                        return;
                    }
                }
                while wake_rx.try_recv().is_ok() {}

                let prepared = this
                    .update(cx, |store, cx| {
                        if store.stopping || store.quitting || store.host_generation != start_generation {
                            return None;
                        }
                        store.reconnecting = true;
                        store.connection = ConnectionState::Connecting;
                        store.ssh_reconnect = SshAutoReconnect::Attempting { attempt, max };
                        cx.notify();
                        Some(store.wire_carrier_events(cx))
                    })
                    .ok()
                    .flatten();
                let Some((events, exit_rx)) = prepared else {
                    return;
                };

                let (paths_c, target_c, askpass_c, modules_c, prev_c) =
                    (paths.clone(), target.clone(), askpass.clone(), modules.clone(), previous.clone());
                let outcome = cx
                    .background_spawn(async move {
                        let prev_ref = prev_c.as_ref().map(|(h, pid)| (h.as_str(), *pid));
                        boot::reconnect_ssh(&paths_c, &target_c, events, &askpass_c, &modules_c, prev_ref)
                    })
                    .await;

                match outcome {
                    Ok(rainy_carrier::ssh::ReconnectOutcome::Connected(host)) => {
                        let host = Arc::new(host);
                        match RpcClient::connect(host.ready()) {
                            Ok((rpc, rpc_events)) => {
                                let host_fallback = host.clone();
                                let rpc_fallback = rpc.clone();
                                let installed = this
                                    .update(cx, |store, cx| {
                                        if store.stopping || store.quitting || store.host_generation != start_generation {
                                            return false;
                                        }
                                        store.host_generation = store.host_generation.wrapping_add(1);
                                        let next_gen = store.host_generation;
                                        store.install_connected_host(
                                            ConnectedHost {
                                                generation: next_gen,
                                                target: target.clone(),
                                                host,
                                                rpc,
                                                rpc_events,
                                                exit_rx,
                                                same_target: true,
                                                restore_tunnels: saved_tunnels.clone(),
                                            },
                                            cx,
                                        );
                                        true
                                    })
                                    .unwrap_or(false);
                                if installed {
                                    let _ = window.update(cx, |_, window, cx| {
                                        use gpui_kit::component::WindowExt as _;
                                        let text = crate::i18n::format(locale, "native.sshReconnected", &[("target", &label)]);
                                        window.push_notification(
                                            gpui_kit::component::notification::Notification::success(text),
                                            cx,
                                        );
                                    });
                                } else {
                                    drop_stale_host(host_fallback, rpc_fallback, cx);
                                }
                                return;
                            }
                            Err(error) => {
                                cx.background_spawn(async move { host.stop(); }).detach();
                                last_error = Some(format!("{error:#}"));
                            }
                        }
                    }
                    Ok(rainy_carrier::ssh::ReconnectOutcome::NeedsInstall { .. }) => {
                        last_error = Some(crate::i18n::text(locale, "ssh.needsInstall").to_owned());
                        let _ = this.update(cx, |store, cx| {
                            if store.host_generation == start_generation {
                                store.reconnecting = false;
                                store.connection = ConnectionState::Closed;
                                cx.notify();
                            }
                        });
                        break;
                    }
                    Ok(rainy_carrier::ssh::ReconnectOutcome::Unsupported(system)) => {
                        last_error = Some(system);
                        let _ = this.update(cx, |store, cx| {
                            if store.host_generation == start_generation {
                                store.reconnecting = false;
                                store.connection = ConnectionState::Closed;
                                cx.notify();
                            }
                        });
                        break;
                    }
                    Err(error) => {
                        let detail = format!("{error:#}");
                        let stop_now = Self::is_non_retryable_ssh_error(&detail);
                        last_error = Some(detail);
                        let _ = this.update(cx, |store, cx| {
                            if store.host_generation == start_generation {
                                store.reconnecting = false;
                                store.connection = ConnectionState::Closed;
                                cx.notify();
                            }
                        });
                        if stop_now {
                            break;
                        }
                    }
                }
            }

            let detail = last_error.unwrap_or_else(|| crate::i18n::text(locale, "app.connectionLost").to_owned());
            let should_alert = this
                .update(cx, |store, cx| {
                    if store.stopping || store.quitting || store.host_generation != start_generation {
                        return false;
                    }
                    store.reconnecting = false;
                    store.connection = ConnectionState::Closed;
                    store.ssh_retry_now = None;
                    store.ssh_reconnect = SshAutoReconnect::Exhausted { last_error: detail.clone() };
                    cx.notify();
                    true
                })
                .unwrap_or(false);
            if should_alert {
                let _ = window.update(cx, |_, window, cx| {
                    crate::ui::ssh::host_exited_exhausted(window, cx, detail);
                });
            }
        });
        self.ssh_reconnect_task = Some(task);
        cx.notify();
    }

    /// Reconnect the current SSH Host in place without restarting `RainyAgent.exe`.
    /// Falls back to an in-place target start when the server needs a fresh runtime installation.
    pub fn reconnect_ssh(&mut self, window: AnyWindowHandle, cx: &mut Context<Self>) {
        if self.reconnecting || self.quitting {
            return;
        }
        let Some(target @ ExecutionTarget::Ssh { .. }) = self.target.clone() else {
            self.restart(cx);
            return;
        };
        let Some(askpass) = self.ensure_askpass() else {
            self.restart(cx);
            return;
        };
        self.cancel_ssh_auto_reconnect(cx);
        self.reconnecting = true;
        self.host_generation = self.host_generation.wrapping_add(1);
        let generation = self.host_generation;
        let locale = self.locale();
        let label = target.label().to_owned();
        let saved_tunnels = self.tunnel_status.rules.clone();
        let tunnels = self.tunnels.clone();
        self.tunnel_status = TunnelStatus { target: None, rules: Vec::new(), state: TunnelState::Stopped };
        cx.background_spawn(async move { tunnels.stop_all() }).detach();

        if let Some(rpc) = self.rpc.take() {
            rpc.close();
        }
        let previous = self
            .host
            .take()
            .and_then(|host| host.remote_ssh().map(|remote| (remote.host.clone(), host.ready().pid)));
        self.connection = ConnectionState::Connecting;
        self.ssh_reconnect = SshAutoReconnect::Attempting { attempt: 1, max: 1 };
        cx.notify();
        let _ = window.update(cx, |_, window, cx| {
            use gpui_kit::component::WindowExt as _;
            let note = crate::i18n::format(locale, "native.sshReconnecting", &[("target", &label)]);
            window.push_notification(gpui_kit::component::notification::Notification::info(note), cx);
        });
        let (events, exit_rx) = self.wire_carrier_events(cx);
        let paths = self.paths.clone();
        let modules = self.modules.manager.clone();
        let reconnecting = cx.background_spawn({
            let target = target.clone();
            async move {
                let prev_ref = previous.as_ref().map(|(h, pid)| (h.as_str(), *pid));
                boot::reconnect_ssh(&paths, &target, events, &askpass, &modules, prev_ref)
            }
        });
        let label_done = label.clone();
        let task = cx.spawn(async move |this: WeakEntity<Store>, cx: &mut AsyncApp| {
            let outcome = reconnecting.await;
            let host = match outcome {
                Ok(rainy_carrier::ssh::ReconnectOutcome::Connected(host)) => Arc::new(host),
                Ok(rainy_carrier::ssh::ReconnectOutcome::NeedsInstall { .. }) => {
                    let _ = this.update(cx, |store, cx| {
                        if store.host_generation != generation {
                            return;
                        }
                        store.reconnecting = false;
                        store.ssh_reconnect = SshAutoReconnect::Idle;
                        store.swap_target_in_place(target.clone(), window, cx);
                    });
                    return;
                }
                Ok(rainy_carrier::ssh::ReconnectOutcome::Unsupported(system)) => {
                    let still_active = this
                        .update(cx, |store, cx| {
                            if store.host_generation != generation {
                                return false;
                            }
                            store.reconnecting = false;
                            store.connection = ConnectionState::Closed;
                            store.ssh_reconnect = SshAutoReconnect::Exhausted { last_error: system.clone() };
                            cx.notify();
                            true
                        })
                        .unwrap_or(false);
                    if still_active {
                        let _ = window.update(cx, |_, window, cx| {
                            use gpui_kit::component::WindowExt as _;
                            let text = crate::i18n::format(locale, "native.sshReconnectFailed", &[("target", &label_done), ("error", &system)]);
                            window.push_notification(gpui_kit::component::notification::Notification::error(text), cx);
                        });
                    }
                    return;
                }
                Err(error) => {
                    let detail = format!("{error:#}");
                    let still_active = this
                        .update(cx, |store, cx| {
                            if store.host_generation != generation {
                                return false;
                            }
                            store.reconnecting = false;
                            store.connection = ConnectionState::Closed;
                            store.ssh_reconnect = SshAutoReconnect::Exhausted { last_error: detail.clone() };
                            cx.notify();
                            true
                        })
                        .unwrap_or(false);
                    if still_active {
                        let _ = window.update(cx, |_, window, cx| {
                            use gpui_kit::component::WindowExt as _;
                            let text = crate::i18n::format(locale, "native.sshReconnectFailed", &[("target", &label_done), ("error", &detail)]);
                            window.push_notification(gpui_kit::component::notification::Notification::error(text), cx);
                        });
                    }
                    return;
                }
            };
            let (rpc, rpc_events) = match RpcClient::connect(host.ready()) {
                Ok(pair) => pair,
                Err(error) => {
                    cx.background_spawn(async move { host.stop(); }).detach();
                    let detail = format!("{error:#}");
                    let still_active = this
                        .update(cx, |store, cx| {
                            if store.host_generation != generation {
                                return false;
                            }
                            store.reconnecting = false;
                            store.connection = ConnectionState::Closed;
                            store.ssh_reconnect = SshAutoReconnect::Exhausted { last_error: detail.clone() };
                            cx.notify();
                            true
                        })
                        .unwrap_or(false);
                    if still_active {
                        let _ = window.update(cx, |_, window, cx| {
                            use gpui_kit::component::WindowExt as _;
                            let text = crate::i18n::format(locale, "native.sshReconnectFailed", &[("target", &label_done), ("error", &detail)]);
                            window.push_notification(gpui_kit::component::notification::Notification::error(text), cx);
                        });
                    }
                    return;
                }
            };
            let host_fallback = host.clone();
            let rpc_fallback = rpc.clone();
            let installed = this
                .update(cx, |store, cx| {
                    if store.stopping || store.quitting || store.host_generation != generation {
                        return false;
                    }
                    store.install_connected_host(
                        ConnectedHost {
                            generation,
                            target,
                            host,
                            rpc,
                            rpc_events,
                            exit_rx,
                            same_target: true,
                            restore_tunnels: saved_tunnels,
                        },
                        cx,
                    );
                    true
                })
                .unwrap_or(false);
            if installed {
                let _ = window.update(cx, |_, window, cx| {
                    use gpui_kit::component::WindowExt as _;
                    let text = crate::i18n::format(locale, "native.sshReconnected", &[("target", &label_done)]);
                    window.push_notification(gpui_kit::component::notification::Notification::success(text), cx);
                });
            } else {
                drop_stale_host(host_fallback, rpc_fallback, cx);
            }
        });
        self.active_swap_task = Some(task);
    }

    /// Hot-swap the execution target (Windows <-> WSL <-> SSH) in place without restarting `RainyAgent.exe`.
    /// Flushes unsaved editor state, stops old tunnels & Host, starts the new Host, and refreshes every subsystem.
    pub fn swap_target_in_place(&mut self, target: ExecutionTarget, window: AnyWindowHandle, cx: &mut Context<Self>) {
        if self.reconnecting || self.quitting {
            return;
        }
        let same_target = self.target.as_ref() == Some(&target);
        let settings = self.paths.desktop_settings();
        let current_host = self.host.clone();
        let host_alive = current_host.as_ref().is_some_and(|host| !host.has_exited());
        let has_rpc = self.rpc.is_some() && host_alive;
        let locale = self.locale();
        let label = target.label().to_owned();

        // Mark `reconnecting` immediately before awaiting freeze/flush so rapid double-clicks cannot re-enter.
        self.cancel_ssh_auto_reconnect(cx);
        self.reconnecting = true;
        self.swapping_target = Some(label.clone().into());
        if matches!(target, ExecutionTarget::Ssh { .. }) {
            let _ = self.ensure_askpass();
        }
        cx.notify();

        let task = cx.spawn(async move |this: WeakEntity<Store>, cx: &mut AsyncApp| {
            // Check whether the current Host is mid-turn before tearing it down.
            if host_alive && let Some(host) = current_host.clone() {
                let busy = cx
                    .background_spawn({
                        let host = host.clone();
                        async move { host.inspect_activity("freeze").unwrap_or(false) }
                    })
                    .await;
                if busy {
                    cx.background_spawn(async move { host.inspect_activity("resume") }).detach();
                    let _ = this.update(cx, |store, cx| {
                        store.reconnecting = false;
                        store.swapping_target = None;
                        cx.notify();
                    });
                    let text = crate::i18n::text(locale, "native.busyBeforeSwitch");
                    let _ = window.update(cx, |_, window, cx| {
                        use gpui_kit::component::WindowExt as _;
                        window.push_notification(gpui_kit::component::notification::Notification::warning(text), cx);
                    });
                    return;
                }
            }

            // Flush unsaved IDE state while the old Host is still reachable.
            if has_rpc {
                let flush = cx.update(|cx| ide::Ide::global(cx).update(cx, |ide, cx| ide.flush_now(cx)));
                let _ = flush.with_timeout(QUIT_FLUSH_TIMEOUT, cx.background_executor()).await;
            }

            if let Err(error) = rainy_carrier::targets::save_target(&settings, &target) {
                log::warn!("The execution target was not saved: {error:#}");
            }

            let prepared = this
                .update(cx, |store, cx| {
                    if store.quitting || store.stopping {
                        store.reconnecting = false;
                        store.swapping_target = None;
                        return None;
                    }
                    store.host_generation = store.host_generation.wrapping_add(1);
                    let generation = store.host_generation;
                    store.connection = ConnectionState::Connecting;
                    store.host_metrics = None;
                    let saved_tunnels = if same_target { store.tunnel_status.rules.clone() } else { Vec::new() };
                    store.tunnel_status = TunnelStatus { target: None, rules: Vec::new(), state: TunnelState::Stopped };
                    if let Some(rpc) = store.rpc.take() {
                        rpc.close();
                    }
                    let old_host = store.host.take();
                    let tunnels = store.tunnels.clone();
                    let (events, exit_rx) = store.wire_carrier_events(cx);
                    let paths = store.paths.clone();
                    let askpass = store.askpass.clone();
                    let modules = store.modules.manager.clone();
                    let prompt = store.prompt.clone();
                    cx.notify();
                    Some((generation, old_host, tunnels, saved_tunnels, events, exit_rx, paths, askpass, modules, prompt))
                })
                .ok()
                .flatten();
            let Some((generation, old_host, tunnels, saved_tunnels, events, exit_rx, paths, askpass, modules, prompt)) = prepared else {
                return;
            };

            let _ = window.update(cx, |_, window, cx| {
                use gpui_kit::component::WindowExt as _;
                let text = crate::i18n::format(locale, "ssh.swapping", &[("target", &label)]);
                window.push_notification(gpui_kit::component::notification::Notification::info(text), cx);
            });

            // Stop old tunnels and old Host off the UI thread before launching the new target.
            cx.background_spawn(async move {
                tunnels.stop_all();
                if let Some(old_host) = old_host {
                    old_host.stop();
                }
            })
            .await;

            let (progress_tx, progress_rx) = async_channel::unbounded::<boot::Step>();
            let label_for_progress = label.clone();
            cx.spawn({
                let this = this.clone();
                async move |cx| {
                    while let Ok(step) = progress_rx.recv().await {
                        let _ = this.update(cx, |store, cx| {
                            if store.host_generation == generation && store.reconnecting {
                                store.swapping_target = Some(format!("{label_for_progress} · {}", step.text).into());
                                cx.notify();
                            }
                        });
                    }
                }
            })
            .detach();

            let target_start = target.clone();
            let started = cx
                .background_spawn(async move {
                    let report = |step: boot::Step| {
                        let _ = progress_tx.try_send(step);
                    };
                    let ask = |error: String| {
                        let (reply, answer) = mpsc::channel();
                        if prompt.send_blocking(RuntimePrompt { error, reply }).is_err() {
                            return boot::RuntimeChoice::Quit;
                        }
                        answer.recv().unwrap_or(boot::RuntimeChoice::Quit)
                    };
                    boot::start_target(&paths, target_start, events, askpass.as_ref(), &modules, &ask, &report)
                        .map(|started| (started.target, Arc::new(started.host)))
                })
                .await;

            let (new_target, host) = match started {
                Ok(pair) => pair,
                Err(error) => {
                    let detail = format!("{error:#}");
                    let still_active = this
                        .update(cx, |store, cx| {
                            if store.host_generation != generation {
                                return false;
                            }
                            store.reconnecting = false;
                            store.swapping_target = None;
                            store.connection = ConnectionState::Closed;
                            store.target = Some(target.clone());
                            if matches!(target, ExecutionTarget::Ssh { .. }) {
                                store.ssh_reconnect = SshAutoReconnect::Exhausted { last_error: detail.clone() };
                            }
                            cx.notify();
                            true
                        })
                        .unwrap_or(false);
                    if still_active {
                        let _ = window.update(cx, |_, window, cx| {
                            use gpui_kit::component::WindowExt as _;
                            let text = crate::i18n::format(locale, "ssh.swapFailed", &[("target", &label), ("error", &detail)]);
                            window.push_notification(gpui_kit::component::notification::Notification::error(text), cx);
                        });
                    }
                    return;
                }
            };

            let (rpc, rpc_events) = match RpcClient::connect(host.ready()) {
                Ok(pair) => pair,
                Err(error) => {
                    cx.background_spawn(async move { host.stop(); }).detach();
                    let detail = format!("{error:#}");
                    let still_active = this
                        .update(cx, |store, cx| {
                            if store.host_generation != generation {
                                return false;
                            }
                            store.reconnecting = false;
                            store.swapping_target = None;
                            store.connection = ConnectionState::Closed;
                            cx.notify();
                            true
                        })
                        .unwrap_or(false);
                    if still_active {
                        let _ = window.update(cx, |_, window, cx| {
                            use gpui_kit::component::WindowExt as _;
                            let text = crate::i18n::format(locale, "ssh.swapFailed", &[("target", &label), ("error", &detail)]);
                            window.push_notification(gpui_kit::component::notification::Notification::error(text), cx);
                        });
                    }
                    return;
                }
            };

            let host_fallback = host.clone();
            let rpc_fallback = rpc.clone();
            let installed = this
                .update(cx, |store, cx| {
                    if store.stopping || store.quitting || store.host_generation != generation {
                        return false;
                    }
                    store.install_connected_host(
                        ConnectedHost {
                            generation,
                            target: new_target,
                            host,
                            rpc,
                            rpc_events,
                            exit_rx,
                            same_target,
                            restore_tunnels: saved_tunnels,
                        },
                        cx,
                    );
                    true
                })
                .unwrap_or(false);
            if installed {
                let _ = window.update(cx, |_, window, cx| {
                    use gpui_kit::component::WindowExt as _;
                    let text = crate::i18n::format(locale, "ssh.swapped", &[("target", &label)]);
                    window.push_notification(gpui_kit::component::notification::Notification::success(text), cx);
                });
            } else {
                drop_stale_host(host_fallback, rpc_fallback, cx);
            }
        });
        self.active_swap_task = Some(task);
    }

    /// Remember `target` and hot-swap into it in place; refused while the Host is busy.
    pub fn switch_target(&mut self, target: ExecutionTarget, window: AnyWindowHandle, cx: &mut Context<Self>) {
        self.swap_target_in_place(target, window, cx);
    }
}
