//! Claude models Rainy prices (pi-ai's Anthropic catalog): dollars per million tokens. A model listed here takes
//! images unless its profile says otherwise. Reasoning levels and request options are in
//! `rainy_protocol::reasoning`, shared with the interface.

use super::Cost;

const fn cost(input: f64, output: f64, cache_read: f64, cache_write: f64) -> Cost {
    Cost { input, output, cache_read, cache_write }
}

/// The price of a Claude model id.
pub fn claude_price(id: &str) -> Option<Cost> {
    Some(match id {
        "claude-opus-5-5" => cost(4.0, 20.0, 0.2, 5.0),
        "claude-sonnet-5-5" => cost(2.0, 10.0, 0.2, 2.5),
        // The rate for prompts up to 100K tokens; longer ones cost five times as much.
        "claude-haiku-5-5" => cost(0.1, 0.5, 0.01, 0.125),
        "claude-fable-5-1" | "claude-mythos-5-1" => cost(10.0, 50.0, 0.25, 12.5),
        "claude-opus-5" => cost(5.0, 25.0, 0.5, 6.25),
        "claude-fable-5" | "claude-mythos-5" => cost(10.0, 50.0, 1.0, 12.5),
        "claude-sonnet-5" => cost(2.0, 10.0, 0.2, 2.5),
        "claude-opus-4-8" | "claude-opus-4-7" | "claude-opus-4-6" => cost(5.0, 25.0, 0.5, 6.25),
        "claude-sonnet-4-6" => cost(3.0, 15.0, 0.3, 3.75),
        "claude-opus-4-5" | "claude-opus-4-5-20251101" => cost(5.0, 25.0, 0.5, 6.25),
        "claude-sonnet-4-5" | "claude-sonnet-4-5-20250929" => cost(3.0, 15.0, 0.3, 3.75),
        "claude-haiku-4-5" | "claude-haiku-4-5-20251001" => cost(1.0, 5.0, 0.1, 1.25),
        _ => return None,
    })
}
