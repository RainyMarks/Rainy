//! Claude models Rainy knows (pi-ai's Anthropic catalog): prices, reasoning levels and request options. A model the
//! catalog does not list is sent as a plain model without reasoning options.

use super::{Cost, LevelMap};

/// How requests to a Claude model are built.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ClaudeOptions {
    /// Claude decides how much to think (`thinking: adaptive` with an effort) instead of a token budget.
    pub adaptive: bool,
    /// The effort can change within a conversation: each reply's level is replayed before it (`output_config` system
    /// messages), so a changed level keeps the prompt cache.
    pub effort_messages: bool,
}

/// One catalog entry.
#[derive(Clone, Debug, PartialEq)]
pub struct Claude {
    pub reasoning: bool,
    pub levels: LevelMap,
    pub cost: Cost,
    pub options: ClaudeOptions,
}

const fn cost(input: f64, output: f64, cache_read: f64, cache_write: f64) -> Cost {
    Cost { input, output, cache_read, cache_write }
}

const ALL_EFFORTS: LevelMap = LevelMap(&[("off", None), ("minimal", None), ("low", Some("low")), ("medium", Some("medium")), ("high", Some("high")), ("xhigh", Some("xhigh")), ("max", Some("max"))]);
const NO_OFF: LevelMap = LevelMap(&[("off", None), ("xhigh", Some("xhigh")), ("max", Some("max"))]);
const XHIGH: LevelMap = LevelMap(&[("xhigh", Some("xhigh")), ("max", Some("max"))]);
const MAX: LevelMap = LevelMap(&[("max", Some("max"))]);
const BUDGET: LevelMap = LevelMap(&[]);

const MANAGED: ClaudeOptions = ClaudeOptions { adaptive: true, effort_messages: true };
const ADAPTIVE: ClaudeOptions = ClaudeOptions { adaptive: true, effort_messages: false };
const BUDGETED: ClaudeOptions = ClaudeOptions { adaptive: false, effort_messages: false };

/// The catalog entry of a Claude model id.
pub fn claude(id: &str) -> Option<Claude> {
    let (levels, cost, options) = match id {
        "claude-opus-5-5" => (ALL_EFFORTS, cost(4.0, 20.0, 0.2, 5.0), MANAGED),
        "claude-sonnet-5-5" => (ALL_EFFORTS, cost(2.0, 10.0, 0.2, 2.5), MANAGED),
        // Newer than pi-ai's catalog: described from Claude Sonnet 5.5 with its own price.
        "claude-haiku-5-5" => (ALL_EFFORTS, cost(0.1, 0.5, 0.01, 0.125), MANAGED),
        "claude-fable-5-1" => (NO_OFF, cost(10.0, 50.0, 0.25, 12.5), MANAGED),
        "claude-opus-5" => (NO_OFF, cost(5.0, 25.0, 0.5, 6.25), MANAGED),
        "claude-fable-5" => (NO_OFF, cost(10.0, 50.0, 1.0, 12.5), ADAPTIVE),
        "claude-sonnet-5" => (XHIGH, cost(2.0, 10.0, 0.2, 2.5), ADAPTIVE),
        "claude-opus-4-8" | "claude-opus-4-7" => (XHIGH, cost(5.0, 25.0, 0.5, 6.25), ADAPTIVE),
        "claude-opus-4-6" => (MAX, cost(5.0, 25.0, 0.5, 6.25), ADAPTIVE),
        "claude-sonnet-4-6" => (MAX, cost(3.0, 15.0, 0.3, 3.75), ADAPTIVE),
        "claude-opus-4-5" | "claude-opus-4-5-20251101" => (BUDGET, cost(5.0, 25.0, 0.5, 6.25), BUDGETED),
        "claude-sonnet-4-5" | "claude-sonnet-4-5-20250929" => (BUDGET, cost(3.0, 15.0, 0.3, 3.75), BUDGETED),
        "claude-haiku-4-5" | "claude-haiku-4-5-20251001" => (BUDGET, cost(1.0, 5.0, 0.1, 1.25), BUDGETED),
        _ => return None,
    };
    Some(Claude { reasoning: true, levels, cost, options })
}
