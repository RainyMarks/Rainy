//! How much the chats were used (`usage.stats`): counted from the chat files the Host keeps, nothing else.

use serde::{Deserialize, Serialize};

/// Days the activity map can cover, ending today: 53 weeks, so a full year fits however the weeks fall. The map shows
/// as many of them as its width allows.
pub const ACTIVITY_DAYS: usize = 371;

/// What `usage.stats` counts.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct UsageStatsParams {
    /// One project's chats; absent for every chat.
    #[serde(default)]
    pub workspace_id: Option<String>,
    /// The last this many days, today included; 0 for all time.
    #[serde(default)]
    pub days: u32,
    /// The interface's offset from UTC in minutes (east positive), so days start at the user's midnight.
    #[serde(default)]
    pub tz_offset_minutes: i32,
}

/// One model's share.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ModelUsage {
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub model: String,
    /// Replies the model gave.
    #[serde(default)]
    pub replies: u64,
    #[serde(default)]
    pub tokens: f64,
}

/// The counts for the chosen chats and days.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct UsageStats {
    /// Chats with a message in the range.
    #[serde(default)]
    pub sessions: u64,
    /// The user's messages and the model's replies.
    #[serde(default)]
    pub messages: u64,
    #[serde(default)]
    pub input: f64,
    #[serde(default)]
    pub output: f64,
    #[serde(default)]
    pub cache_read: f64,
    #[serde(default)]
    pub cache_write: f64,
    /// Days with a message.
    #[serde(default)]
    pub active_days: u64,
    /// Models by tokens, most first.
    #[serde(default)]
    pub models: Vec<ModelUsage>,
    /// Messages per day for the [`ACTIVITY_DAYS`] days ending today, oldest first, whatever the range.
    #[serde(default)]
    pub activity: Vec<u32>,
}

impl UsageStats {
    /// Every token sent and received.
    pub fn tokens(&self) -> f64 {
        self.input + self.output + self.cache_read + self.cache_write
    }

    /// The share of input read from the provider's cache; `None` before any input.
    pub fn cache_hit_rate(&self) -> Option<f64> {
        let input = self.input + self.cache_read + self.cache_write;
        (input > 0.0).then(|| self.cache_read / input)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_stats_defaults_on_partial_json() {
        let stats: UsageStats = serde_json::from_str("{}").unwrap();
        assert_eq!(stats, UsageStats::default());
        assert_eq!(stats.tokens(), 0.0);
        assert_eq!(stats.cache_hit_rate(), None);
        let model: ModelUsage = serde_json::from_str("{}").unwrap();
        assert_eq!(model, ModelUsage::default());
    }
}
