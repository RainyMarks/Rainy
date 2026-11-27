//! `usage.stats`: how much the chats were used, counted from the chat files. Each file is read once and then only
//! from where the last count stopped, as searches read them, so asking again costs the lines appended since.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use rainy_protocol::{ACTIVITY_DAYS, ModelUsage, UsageStats, UsageStatsParams};
use serde::Deserialize;

use super::store::ChatStore;
use super::util::JsonlTail;

const DAY_MS: f64 = 86_400_000.0;

/// One message of a chat file: when, and for a reply its model and tokens.
#[derive(Clone, Debug, PartialEq)]
struct Message {
    ts: f64,
    /// Index into [`Tally::models`]; `None` for the user's messages.
    model: Option<usize>,
    tokens: [f64; 4],
}

/// Non-message token usage recorded in compaction or `task` sub-agent entries (counted in tokens, not message count).
#[derive(Clone, Debug, PartialEq)]
struct ExtraUsage {
    ts: f64,
    model: Option<usize>,
    tokens: [f64; 4],
}

/// What was counted of one chat file, and how far it was read.
#[derive(Default)]
pub struct Tally {
    read: u64,
    tail: JsonlTail,
    /// `(provider, model)` of the replies, each once.
    models: Vec<(String, String)>,
    messages: Vec<Message>,
    extra: Vec<ExtraUsage>,
}

#[derive(Deserialize)]
struct Line {
    entry: Entry,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Entry {
    kind: String,
    #[serde(default, deserialize_with = "number")]
    ts: f64,
    #[serde(default)]
    message: Option<Reply>,
    #[serde(default)]
    request: Option<CompactionRequest>,
    #[serde(default)]
    tool_name: Option<String>,
    #[serde(default)]
    details: Option<TaskDetails>,
}

#[derive(Deserialize)]
struct CompactionRequest {
    #[serde(default)]
    usage: Option<Tokens>,
}

#[derive(Deserialize)]
struct TaskDetails {
    #[serde(default)]
    provider: String,
    #[serde(default)]
    model: String,
    #[serde(default)]
    usage: Option<Tokens>,
}

#[derive(Deserialize)]
struct Reply {
    #[serde(default)]
    provider: String,
    #[serde(default)]
    model: String,
    #[serde(default)]
    usage: Tokens,
    #[serde(default, deserialize_with = "number")]
    timestamp: f64,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Tokens {
    #[serde(default, deserialize_with = "number")]
    input: f64,
    #[serde(default, deserialize_with = "number")]
    output: f64,
    #[serde(default, deserialize_with = "number")]
    cache_read: f64,
    #[serde(default, deserialize_with = "number")]
    cache_write: f64,
}

/// A count 3.x may have written as `null`.
fn number<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<f64, D::Error> {
    Ok(Option::<f64>::deserialize(deserializer)?.filter(|value| value.is_finite() && *value >= 0.0).unwrap_or(0.0))
}

impl Tally {
    fn intern_model(&mut self, provider: String, model: String) -> usize {
        let key = (provider, model);
        match self.models.iter().position(|known| *known == key) {
            Some(index) => index,
            None => {
                self.models.push(key);
                self.models.len() - 1
            }
        }
    }

    /// Bring the count up to date with the file at `path`.
    fn update(&mut self, path: &Path) -> std::io::Result<()> {
        let delta = self.tail.update(path)?;
        if delta.reset {
            self.models.clear();
            self.messages.clear();
            self.extra.clear();
        }
        self.read = delta.offset;
        if !delta.added.is_empty() {
            self.add_lines(&delta.added);
        }
        Ok(())
    }

    fn add_lines(&mut self, lines: &str) {
        for line in lines.split('\n') {
            // Most lines are regular tool results and stream records: skip them before parsing.
            if !line.contains("\"entry\"")
                || !(line.contains("\"user\"")
                    || line.contains("\"assistant\"")
                    || line.contains("\"compaction\"")
                    || (line.contains("\"toolResult\"") && line.contains("\"task\"") && line.contains("\"usage\"")))
            {
                continue;
            }
            let Ok(Line { entry }) = serde_json::from_str::<Line>(line) else { continue };
            match entry.kind.as_str() {
                "user" => self.messages.push(Message { ts: entry.ts, model: None, tokens: [0.0; 4] }),
                "assistant" => {
                    let Some(reply) = entry.message else { continue };
                    let model = self.intern_model(reply.provider, reply.model);
                    let usage = reply.usage;
                    let ts = if entry.ts > 0.0 { entry.ts } else { reply.timestamp };
                    self.messages.push(Message { ts, model: Some(model), tokens: [usage.input, usage.output, usage.cache_read, usage.cache_write] });
                }
                "compaction" => {
                    if let Some(usage) = entry.request.and_then(|req| req.usage) {
                        let total = usage.input + usage.output + usage.cache_read + usage.cache_write;
                        if total > 0.0 {
                            self.extra.push(ExtraUsage { ts: entry.ts, model: None, tokens: [usage.input, usage.output, usage.cache_read, usage.cache_write] });
                        }
                    }
                }
                "toolResult" if entry.tool_name.as_deref() == Some("task") => {
                    if let Some(details) = entry.details
                        && let Some(usage) = details.usage
                    {
                        let total = usage.input + usage.output + usage.cache_read + usage.cache_write;
                        if total > 0.0 {
                            let model = (!details.model.is_empty()).then(|| self.intern_model(details.provider, details.model));
                            self.extra.push(ExtraUsage { ts: entry.ts, model, tokens: [usage.input, usage.output, usage.cache_read, usage.cache_write] });
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

/// The day `ts` falls on at the user's offset, counted from 1970-01-01.
fn day_of(ts: f64, offset_ms: f64) -> i64 {
    ((ts + offset_ms) / DAY_MS).floor() as i64
}

impl ChatStore {
    /// Count the chats `params` chooses. Reads the files: call it off the async threads.
    pub fn usage_stats(&self, params: &UsageStatsParams, now_ms: f64) -> UsageStats {
        let offset_ms = f64::from(params.tz_offset_minutes.clamp(-14 * 60, 14 * 60)) * 60_000.0;
        let today = day_of(now_ms, offset_ms);
        let first_day = (params.days > 0).then(|| today - i64::from(params.days) + 1);
        let first_mapped = today - ACTIVITY_DAYS as i64 + 1;
        let chats = self.list();
        let mut tallied = self.tallied.lock().unwrap();
        tallied.retain(|id, _| chats.iter().any(|chat| &chat.id == id));
        let mut stats = UsageStats { activity: vec![0; ACTIVITY_DAYS], ..UsageStats::default() };
        let mut days = HashSet::new();
        let mut models: HashMap<(String, String), ModelUsage> = HashMap::new();
        for chat in chats.iter().filter(|chat| params.workspace_id.is_none() || chat.workspace_id == params.workspace_id) {
            let tally = tallied.entry(chat.id.clone()).or_default();
            if tally.update(&self.chat_path(&chat.id)).is_err() {
                continue;
            }
            let mut counted = false;
            for message in &tally.messages {
                let day = day_of(message.ts, offset_ms);
                if day >= first_mapped && day <= today {
                    stats.activity[(day - first_mapped) as usize] += 1;
                }
                if first_day.is_some_and(|first| day < first) {
                    continue;
                }
                counted = true;
                stats.messages += 1;
                days.insert(day);
                let Some(model) = message.model else { continue };
                let [input, output, cache_read, cache_write] = message.tokens;
                stats.input += input;
                stats.output += output;
                stats.cache_read += cache_read;
                stats.cache_write += cache_write;
                let (provider, name) = &tally.models[model];
                if name.is_empty() {
                    continue;
                }
                let share = models.entry((provider.clone(), name.clone())).or_insert_with(|| ModelUsage { provider: provider.clone(), model: name.clone(), ..ModelUsage::default() });
                share.replies += 1;
                share.tokens += input + output + cache_read + cache_write;
            }
            for extra in &tally.extra {
                let day = day_of(extra.ts, offset_ms);
                if first_day.is_some_and(|first| day < first) {
                    continue;
                }
                let [input, output, cache_read, cache_write] = extra.tokens;
                stats.input += input;
                stats.output += output;
                stats.cache_read += cache_read;
                stats.cache_write += cache_write;
                if let Some(model) = extra.model {
                    let (provider, name) = &tally.models[model];
                    if !name.is_empty() {
                        let share = models.entry((provider.clone(), name.clone())).or_insert_with(|| ModelUsage { provider: provider.clone(), model: name.clone(), ..ModelUsage::default() });
                        share.tokens += input + output + cache_read + cache_write;
                    }
                }
            }
            stats.sessions += u64::from(counted);
        }
        stats.active_days = days.len() as u64;
        stats.models = models.into_values().collect();
        stats.models.sort_by(|left, right| right.tokens.total_cmp(&left.tokens).then_with(|| right.replies.cmp(&left.replies)).then_with(|| left.model.cmp(&right.model)));
        stats
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::store::NewChat;
    use rainy_protocol::TranscriptEntry;
    use serde_json::json;

    fn user(id: &str, ts: f64) -> TranscriptEntry {
        serde_json::from_value(json!({ "kind": "user", "id": id, "ts": ts, "text": "hi" })).unwrap()
    }

    fn reply(id: &str, ts: f64, model: &str, input: f64, cache_read: f64) -> TranscriptEntry {
        serde_json::from_value(json!({
            "kind": "assistant", "id": id, "ts": ts,
            "message": { "content": [{ "type": "text", "text": "ok" }], "provider": "p", "model": model,
                "usage": { "input": input, "output": 10, "cacheRead": cache_read, "cacheWrite": 0 } }
        }))
        .unwrap()
    }

    #[tokio::test]
    async fn counts_messages_tokens_days_and_models_and_reads_only_what_was_appended() {
        let dir = std::env::temp_dir().join(format!("rainy-usage-{}", uuid::Uuid::new_v4()));
        let store = ChatStore::load(dir.clone()).unwrap();
        let now = 1_764_000_000_000.0; // 2025-11-24 16:00 UTC
        let day = DAY_MS;
        let a = store.create(NewChat { cwd: "/w".into(), workspace_id: Some("w1".into()), ..NewChat::default() }).unwrap();
        let b = store.create(NewChat { cwd: "/v".into(), workspace_id: Some("w2".into()), ..NewChat::default() }).unwrap();
        let _empty = store.create(NewChat { cwd: "/v".into(), ..NewChat::default() }).unwrap();
        store.append(&a.id, &[user("u1", now - 40.0 * day), reply("r1", now - 40.0 * day, "big", 100.0, 300.0)]).await.unwrap();
        store.append(&b.id, &[user("u2", now - day), reply("r2", now - day, "small", 50.0, 0.0), reply("r3", now, "small", 50.0, 0.0)]).await.unwrap();

        let all = store.usage_stats(&UsageStatsParams::default(), now);
        assert_eq!((all.sessions, all.messages, all.active_days), (2, 5, 3));
        assert_eq!(all.tokens(), 100.0 + 300.0 + 50.0 + 50.0 + 30.0);
        assert_eq!(all.cache_hit_rate(), Some(300.0 / 500.0));
        assert_eq!(all.models[0].model, "big");
        assert_eq!(all.models[1].replies, 2);
        assert_eq!(all.activity.iter().sum::<u32>(), 5);
        assert_eq!(*all.activity.last().unwrap(), 1);

        let week = store.usage_stats(&UsageStatsParams { days: 7, ..UsageStatsParams::default() }, now);
        assert_eq!((week.sessions, week.messages, week.models.len()), (1, 3, 1));
        // The activity map covers its whole span whatever the range.
        assert_eq!(week.activity.iter().sum::<u32>(), 5);
        let project = store.usage_stats(&UsageStatsParams { workspace_id: Some("w1".into()), ..UsageStatsParams::default() }, now);
        assert_eq!((project.sessions, project.messages), (1, 2));

        // Appended lines are added to what was counted, not counted twice.
        let read = store.tallied.lock().unwrap()[&b.id].read;
        store.append(&b.id, &[user("u3", now)]).await.unwrap();
        let again = store.usage_stats(&UsageStatsParams::default(), now);
        assert_eq!(again.messages, 6);
        assert!(store.tallied.lock().unwrap()[&b.id].read > read);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn reads_messages_however_the_json_is_spaced() {
        let mut tally = Tally::default();
        // A spaced user message, and a tool result whose text is "user": it passes the prefilter and is dropped.
        tally.add_lines("{\"type\": \"entry\", \"entry\": {\"kind\": \"user\", \"id\": \"u\", \"ts\": 5, \"text\": \"hi\"}}\n{\"type\":\"entry\",\"entry\":{\"kind\":\"toolResult\",\"id\":\"t\",\"ts\":6,\"toolCallId\":\"c\",\"toolName\":\"read\",\"content\":[{\"type\":\"text\",\"text\":\"user\"}],\"isError\":false}}");
        assert_eq!(tally.messages, [Message { ts: 5.0, model: None, tokens: [0.0; 4] }]);
    }

    #[test]
    fn days_start_at_the_users_midnight_and_null_counts_are_zero() {
        // 2025-01-01 23:30 UTC is already 2 January at UTC+8.
        let ts = 1_735_774_200_000.0;
        assert_eq!(day_of(ts, 8.0 * 3_600_000.0) - day_of(ts, 0.0), 1);
        let mut tally = Tally::default();
        tally.add_lines(r#"{"type":"entry","entry":{"kind":"assistant","id":"r","ts":1,"message":{"content":[],"model":"m","usage":{"input":null,"output":5}}}}"#);
        assert_eq!(tally.messages[0].tokens, [0.0, 5.0, 0.0, 0.0]);
    }
}
