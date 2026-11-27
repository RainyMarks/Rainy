//! Shared state guards, input change bindings and lightweight motion helpers for the Settings pages.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::base::{Easing, Keyframe, Keyframes, Spring, Timing, animate_keyframes, spring};
use gpui_kit::component::input::{InputEvent, InputState, TextareaState};
use gpui_kit::*;

/// Indicator slide spring (critically-damped feel, settles cleanly to 0 frames at rest).
pub(super) const SPRING_INDICATOR: Spring = Spring::new(Duration::from_millis(180)).with_damping(0.92).with_epsilon(0.25);

/// Color swatch selection ring transition duration.
pub(super) const TRANSITION_RING: Duration = Duration::from_millis(120);

/// Tracks concurrent read and write RPCs with a monotonic generation counter so that:
/// - a finishing read never clears a write lock;
/// - stale responses from earlier reads/workspaces/connections are ignored;
/// - switching workspaces or reconnecting cleanly invalidates in-flight reads.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct AsyncPageGuard {
    inflight_reads: usize,
    inflight_writes: usize,
    generation: u64,
}

impl AsyncPageGuard {
    pub fn begin_read(&mut self) -> u64 {
        self.inflight_reads += 1;
        self.generation
    }

    /// Completes a read started at `generation`. Returns `true` if the response is still current.
    pub fn end_read(&mut self, generation: u64) -> bool {
        self.inflight_reads = self.inflight_reads.saturating_sub(1);
        self.generation == generation
    }

    /// Starts a write, advancing `generation` so any older in-flight read cannot overwrite the write's result.
    pub fn begin_write(&mut self) -> u64 {
        self.generation = self.generation.wrapping_add(1);
        self.inflight_writes += 1;
        self.generation
    }

    /// Completes a write started at `generation`. Returns `true` if the generation is still current.
    pub fn end_write(&mut self, generation: u64) -> bool {
        self.inflight_writes = self.inflight_writes.saturating_sub(1);
        self.generation == generation
    }

    /// Invalidates all in-flight reads/writes (e.g. on workspace change or host reconnect).
    pub fn invalidate(&mut self) -> u64 {
        self.generation = self.generation.wrapping_add(1);
        self.generation
    }

    /// Whether any write mutation is currently in flight.
    pub fn busy(&self) -> bool {
        self.inflight_writes > 0
    }

    /// Whether any read refresh is currently in flight.
    #[cfg(test)]
    pub fn loading(&self) -> bool {
        self.inflight_reads > 0
    }
}

/// Subscribe to `InputEvent::Change` on a single-line input so derived button states refresh immediately.
pub(super) fn bind_input_change<T: 'static>(input: &Entity<InputState>, window: &mut Window, cx: &mut Context<T>) -> Subscription {
    cx.subscribe_in(input, window, |_, _, event: &InputEvent, _, cx| {
        if matches!(event, InputEvent::Change) {
            cx.notify();
        }
    })
}

/// Subscribe to `InputEvent::Change` on a multi-line textarea so counters and save buttons refresh immediately.
pub(super) fn bind_textarea_change<T: 'static>(input: &Entity<TextareaState>, window: &mut Window, cx: &mut Context<T>) -> Subscription {
    cx.subscribe_in(input, window, |_, _, event: &InputEvent, _, cx| {
        if matches!(event, InputEvent::Change) {
            cx.notify();
        }
    })
}

/// Samples a one-shot `0.0 -> 1.0` ease-out progress for `id`. Returns `1.0` immediately when reduced motion is active
/// and requests zero frames once settled.
pub(super) fn enter_progress(id: impl Into<gpui_kit::base::TransitionId>, duration: Duration, window: &mut Window, cx: &mut App) -> f32 {
    if cx.reduce_motion() || duration.is_zero() {
        return 1.0;
    }
    let keyframes = Keyframes::try_new([Keyframe::new(0.0, 0.0_f32).ease(Easing::EaseOut), Keyframe::new(1.0, 1.0_f32)]).expect("valid keyframes");
    animate_keyframes(id, &keyframes, Timing::new(duration), window, cx).value.clamp(0.0, 1.0)
}

/// Measured container and active item bounds for a sliding indicator (`on_prepaint` + `spring`).
#[derive(Default)]
pub(super) struct SlidingBounds {
    pub container: Option<Bounds<Pixels>>,
    pub active_index: usize,
    pub active_bounds: Option<Bounds<Pixels>>,
    pub placed: bool,
}

impl SlidingBounds {
    pub fn shared(id: impl Into<ElementId>, window: &mut Window, cx: &mut App) -> Rc<RefCell<Self>> {
        let state = window.use_keyed_state(id.into(), cx, |_, _| Rc::new(RefCell::new(Self::default())));
        state.read(cx).clone()
    }

    pub fn horizontal_target(&self, active_index: usize) -> Option<(Pixels, Pixels)> {
        if self.active_index != active_index {
            return None;
        }
        let container = self.container?;
        let active = self.active_bounds?;
        if container.size.width <= px(0.) || active.size.width <= px(0.) {
            return None;
        }
        Some((active.origin.x - container.origin.x, active.size.width))
    }

    pub fn vertical_target(&self, active_index: usize) -> Option<(Pixels, Pixels)> {
        if self.active_index != active_index {
            return None;
        }
        let container = self.container?;
        let active = self.active_bounds?;
        if container.size.height <= px(0.) || active.size.height <= px(0.) {
            return None;
        }
        Some((active.origin.y - container.origin.y, active.size.height))
    }
}

/// Computes the spring-interpolated `(offset, extent)` along one axis from previously prepainted bounds.
/// Returns `None` on the very first frame before bounds are measured; callers should fall back to static highlight.
pub(super) fn spring_slide(
    id: SharedString,
    target: Option<(Pixels, Pixels)>,
    window: &mut Window,
    cx: &mut App,
) -> Option<(Pixels, Pixels)> {
    let (to_pos, to_len) = target?;
    let pos = spring((id.clone(), "pos"), to_pos, SPRING_INDICATOR, window, cx);
    let len = spring((id, "len"), to_len, SPRING_INDICATOR, window, cx);
    Some((pos, len))
}

#[cfg(test)]
mod tests {
    use ::core::prelude::v1::test;

    use super::*;

    #[test]
    fn async_page_guard_prevents_read_from_clearing_write_lock_and_drops_stale_reads() {
        let mut guard = AsyncPageGuard::default();
        assert!(!guard.busy());
        assert!(!guard.loading());

        let r1 = guard.begin_read();
        assert!(guard.loading());
        assert!(!guard.busy());

        // Starting a write advances generation so the older in-flight read becomes stale.
        let w1 = guard.begin_write();
        assert!(guard.busy());
        assert_ne!(r1, w1);

        // Old read returns while write is still in flight: stale AND write lock stays active.
        assert!(!guard.end_read(r1));
        assert!(guard.busy());
        assert!(!guard.loading());

        // A newer read started during the write shares w1's generation; finishing it still keeps busy=true.
        let r2 = guard.begin_read();
        assert!(guard.end_read(r2));
        assert!(guard.busy());

        // Write completes cleanly.
        assert!(guard.end_write(w1));
        assert!(!guard.busy());

        // Invalidation (workspace change / reconnect) marks earlier in-flight requests stale.
        let r3 = guard.begin_read();
        let w2 = guard.begin_write();
        guard.invalidate();
        assert!(!guard.end_read(r3));
        assert!(!guard.end_write(w2));
        assert!(!guard.busy());
        assert!(!guard.loading());
    }
}
