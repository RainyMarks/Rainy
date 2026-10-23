//! When and how long a run waits before it asks the model again after a request failed for a passing reason
//! (3.x `retry.ts`).

use std::sync::LazyLock;

use rainy_protocol::chat::AssistantMessage;
use regex::Regex;

use super::llm::retryable;

/// Automatic retries of one run before it gives up and leaves the retry to the user.
pub const AUTO_RETRIES: u32 = 3;
/// Wait before the first automatic retry; each further one waits twice as long.
pub const BASE_MS: u64 = 5_000;
/// Longest wait the run accepts, including one the provider asks for; a longer one is left to the user.
pub const MAX_MS: u64 = 120_000;

/// Whether a failed reply is worth requesting again unchanged: overload, rate limits, dropped connections, timeouts.
/// Quota, billing, authentication and malformed requests are not.
pub fn is_passing_failure(message: &AssistantMessage) -> bool {
    static PERMANENT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^(?:error:\s*)?(4\d\d)\b").unwrap());
    // Our own timeout notice and Chinese provider wording the English patterns miss.
    static PASSING_TEXT: LazyLock<Regex> = LazyLock::new(|| Regex::new("超时|繁忙|稍后再?试|限流|过载").unwrap());
    if message.stop_reason != "error" {
        return false;
    }
    let text = message.error_message.as_deref().unwrap_or_default();
    if let Some(status) = PERMANENT.captures(text.trim())
        && !matches!(&status[1], "408" | "409" | "425" | "429")
    {
        return false;
    }
    retryable(message) || PASSING_TEXT.is_match(text)
}

/// Wait before automatic retry `attempt` (1 for the first): the provider's requested delay when the error carries one,
/// else exponential backoff with ±15 % jitter (`random` in [0, 1)); `None` when the provider asks for too long.
pub fn delay(attempt: u32, error: &str, random: f64) -> Option<u64> {
    static REQUESTED: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)requested (\d+(?:\.\d+)?)s retry delay|retry[- ]after[^\d]{0,12}(\d+(?:\.\d+)?)\s*(?:s\b|sec|second)").unwrap());
    if let Some(found) = REQUESTED.captures(error) {
        let seconds: f64 = found.get(1).or(found.get(2)).and_then(|value| value.as_str().parse().ok()).unwrap_or(0.0);
        let ms = (seconds * 1000.0) as u64;
        return (ms <= MAX_MS).then_some(ms.max(BASE_MS));
    }
    let backoff = BASE_MS.saturating_mul(1 << attempt.saturating_sub(1).min(16)).min(MAX_MS);
    Some((backoff as f64 * (0.85 + random * 0.3)).round() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delays() {
        assert_eq!(delay(1, "x", 0.5), Some(5000));
        assert_eq!(delay(3, "x", 0.5), Some(20000));
        assert_eq!(delay(9, "x", 0.5), Some(MAX_MS));
        assert_eq!(delay(1, "Please retry after 30 seconds", 0.0), Some(30000));
        assert_eq!(delay(1, "requested 600s retry delay", 0.0), None);
    }
}
