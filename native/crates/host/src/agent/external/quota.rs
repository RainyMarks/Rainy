//! External agents' usage limits: Codex's `account/rateLimits` (five-hour and weekly windows) and the table
//! `agy -p /usage` prints (per model group).

use rainy_protocol::{ExternalQuota, QuotaWindow};
use serde_json::Value;

/// Unix milliseconds now.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_millis() as i64)
}

/// The limits in Codex's `account/rateLimits/read` result or `account/rateLimits/updated` params: `rateLimits` with a
/// `primary` and a `secondary` window, each `{usedPercent, windowDurationMins, resetsAt}`.
pub fn codex(value: &Value) -> Option<ExternalQuota> {
    let limits = value.get("rateLimits").filter(|limits| limits.is_object())?;
    let windows: Vec<QuotaWindow> = ["primary", "secondary"]
        .iter()
        .filter_map(|name| limits.get(*name).filter(|window| window.is_object()))
        .filter_map(|window| {
            let used = window.get("usedPercent").and_then(Value::as_f64)?;
            Some(QuotaWindow {
                group: None,
                minutes: window.get("windowDurationMins").and_then(Value::as_u64).and_then(|minutes| u32::try_from(minutes).ok()),
                remaining: (100. - used).round().clamp(0., 100.) as u8,
                resets_at: window.get("resetsAt").and_then(Value::as_i64),
            })
        })
        .collect();
    let plan = limits.get("planType").and_then(Value::as_str).filter(|plan| !plan.is_empty()).map(str::to_owned);
    (!windows.is_empty()).then(|| ExternalQuota { fetched_at: now_ms(), plan, windows, error: None })
}

/// The table `agy -p /usage` prints, one window per line: `group<TAB>name<TAB>98%<TAB>2026-11-30T20:04:56Z`.
pub fn agy_usage(text: &str) -> Option<ExternalQuota> {
    let windows: Vec<QuotaWindow> = text
        .lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split('\t').map(str::trim).collect();
            let [group, name, percent, reset] = fields.as_slice() else { return None };
            let remaining = percent.strip_suffix('%')?.trim().parse::<f64>().ok()?;
            let lower = name.to_ascii_lowercase();
            let minutes = if lower.contains("five hour") || lower.contains("5 hour") || lower.contains("5-hour") {
                Some(300)
            } else if lower.contains("weekly") || lower.contains("week") {
                Some(10080)
            } else if lower.contains("daily") || lower.contains("day") {
                Some(1440)
            } else {
                None
            };
            Some(QuotaWindow {
                group: (!group.is_empty()).then(|| (*group).to_owned()),
                minutes,
                remaining: remaining.round().clamp(0., 100.) as u8,
                resets_at: utc_seconds(reset),
            })
        })
        .collect();
    (!windows.is_empty()).then(|| ExternalQuota { fetched_at: now_ms(), plan: None, windows, error: None })
}

/// Unix seconds of a UTC time written `2026-11-30T20:04:56Z` (fractions of a second allowed, `+00:00` too).
pub fn utc_seconds(text: &str) -> Option<i64> {
    let text = text.trim();
    let text = text.strip_suffix('Z').or_else(|| text.strip_suffix("+00:00"))?;
    let (date, time) = text.split_once('T')?;
    let mut date = date.split('-').map(|part| part.parse::<i64>().ok());
    let (year, month, day) = (date.next()??, date.next()??, date.next()??);
    let time = time.split('.').next()?;
    let mut time = time.split(':').map(|part| part.parse::<i64>().ok());
    let (hour, minute, second) = (time.next()??, time.next()??, time.next().flatten().unwrap_or(0));
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    // Days from 1970-01-01 to the date (Howard Hinnant's days_from_civil).
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let of_era = year - era * 400;
    let of_year = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    let of_era_days = of_era * 365 + of_era / 4 - of_era / 100 + of_year;
    let days = era * 146_097 + of_era_days - 719_468;
    Some(days * 86_400 + hour * 3600 + minute * 60 + second)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_codexs_two_windows() {
        // What codex 0.170 answers to `account/rateLimits/read`.
        let quota = codex(&json!({ "ordinaryUsageAllowed": true, "rateLimits": {
            "limitId": "codex", "primary": { "usedPercent": 2, "windowDurationMins": 300, "resetsAt": 1795541370 },
            "secondary": { "usedPercent": 2.6, "windowDurationMins": 10080, "resetsAt": 1795971390 },
            "credits": { "hasCredits": false, "unlimited": false, "balance": "0" }, "planType": "plus" } }))
        .unwrap();
        assert_eq!(quota.plan.as_deref(), Some("plus"));
        assert_eq!(quota.windows.len(), 2);
        assert_eq!((quota.windows[0].minutes, quota.windows[0].remaining, quota.windows[0].resets_at), (Some(300), 98, Some(1795541370)));
        assert_eq!((quota.windows[1].minutes, quota.windows[1].remaining), (Some(10080), 97));
        assert!(codex(&json!({ "rateLimits": { "primary": null } })).is_none());
        assert!(codex(&json!({})).is_none());
    }

    #[test]
    fn reads_the_table_agy_prints() {
        // `agy -p /usage` 1.5.16.
        let text = "Gemini Models\tWeekly Limit Remaining\t98%\t2026-11-30T20:04:56Z\n\
                    Gemini Models\tFive Hour Limit Remaining\t98%\t2026-11-24T13:14:38Z\n\
                    Claude and GPT models\tWeekly Limit Remaining\t100%\t2026-12-01T12:48:58Z\n\
                    Claude and GPT models\tFive Hour Limit Remaining\t100%\t2026-11-24T17:48:58Z\n";
        let quota = agy_usage(text).unwrap();
        assert_eq!(quota.windows.len(), 4);
        assert_eq!(quota.windows[0].group.as_deref(), Some("Gemini Models"));
        assert_eq!((quota.windows[0].minutes, quota.windows[0].remaining), (Some(10080), 98));
        assert_eq!(quota.windows[1].minutes, Some(300));
        assert_eq!(quota.windows[1].resets_at, Some(1_795_526_078));
        assert_eq!(quota.group_of("gemini-4-neon"), Some("Gemini Models"));
        assert_eq!(quota.group_of("claude-opus-5-5"), Some("Claude and GPT models"));
        assert_eq!(quota.group_of("gpt-oss-120b-medium"), Some("Claude and GPT models"));
        assert_eq!(quota.group_of("gemini-pro-orchestrator"), Some("Gemini Models"));
        let gemini = quota.windows_for(Some("gemini-4-neon"));
        assert_eq!(gemini.iter().map(|window| window.minutes).collect::<Vec<_>>(), [Some(300), Some(10080)]);
        assert_eq!(quota.windows_for(None).len(), 4);
        assert_eq!(quota.windows_for(Some("some-other-model")).len(), 4);
        assert!(agy_usage("Fetching usage...\nsomething else\n").is_none());
    }

    #[test]
    fn utc_times() {
        assert_eq!(utc_seconds("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(utc_seconds("2000-03-01T00:00:00Z"), Some(951_868_800));
        assert_eq!(utc_seconds("2026-11-24T13:14:38.512Z"), utc_seconds("2026-11-24T13:14:38+00:00"));
        assert_eq!(utc_seconds("2026-11-24 13:14:38"), None);
        assert_eq!(utc_seconds("2026-13-01T00:00:00Z"), None);
    }
}
