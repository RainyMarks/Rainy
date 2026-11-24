//! External agents' usage limits (Codex's five-hour and weekly windows, Antigravity's per model group) as the model
//! card, the composer's model button and Settings show them.

use chrono::{Local, TimeZone as _};
use gpui_kit::component::{ActiveTheme as _, StyledExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use rainy_protocol::{ExternalAgentStatus, ExternalQuota, QuotaWindow};
use serde_json::json;

use crate::i18n::{self, Locale};
use crate::store::Store;

/// Below this share left, the composer's model button shows what is left.
pub const LOW: u8 = 20;
/// Below this share left, a window is drawn in the danger color.
const VERY_LOW: u8 = 5;

/// Whether Rainy can read the limits of an agent of `kind`.
pub fn readable(kind: &str) -> bool {
    matches!(kind, "codex" | "antigravity")
}

/// Ask the Host to read an agent's limits again (one read under a minute ago is kept unless `force`); the store takes
/// them from the `external.changed` that follows.
pub fn refresh(store: &Entity<Store>, kind: &str, force: bool, cx: &mut App) {
    if !readable(kind) {
        return;
    }
    let call = store.read(cx).call("external.quota", json!({ "kind": kind, "force": force }));
    cx.foreground_executor()
        .spawn(async move {
            let _ = call.await;
        })
        .detach();
}

/// The least share left of the windows that count `model`, when its agent reported any.
pub fn lowest(agent: &ExternalAgentStatus, model: Option<&str>) -> Option<u8> {
    agent.quota.as_ref()?.windows_for(model).iter().map(|window| window.remaining).min()
}

/// A window's name: "5 小时", "每周", or its length in hours.
fn window_name(locale: Locale, window: &QuotaWindow) -> String {
    match window.minutes {
        Some(300) => i18n::text(locale, "external.quotaFiveHours").to_owned(),
        Some(1440) => i18n::text(locale, "external.quotaDaily").to_owned(),
        Some(10080) => i18n::text(locale, "external.quotaWeekly").to_owned(),
        Some(minutes) if minutes % 60 == 0 => i18n::format(locale, "external.quotaHours", &[("hours", &(minutes / 60))]),
        _ => i18n::text(locale, "external.quotaWindow").to_owned(),
    }
}

/// When a window starts over, in local time: "13:14" today, "明天 13:14", else the date.
pub fn reset_text(locale: Locale, resets_at: i64) -> Option<String> {
    let time = Local.timestamp_opt(resets_at, 0).single()?;
    let days = (time.date_naive() - Local::now().date_naive()).num_days();
    let clock = time.format("%H:%M").to_string();
    let when = match days {
        ..=0 => clock,
        1 => i18n::format(locale, "external.quotaTomorrow", &[("time", &clock)]),
        _ => match locale {
            Locale::Zh => time.format("%-m月%-d日").to_string(),
            Locale::En => time.format("%b %-d").to_string(),
        },
    };
    Some(i18n::format(locale, "external.quotaResets", &[("time", &when)]))
}

/// A model group as the agent names it, in the interface's language ("Gemini Models" → "Gemini 模型").
pub fn group_name(locale: Locale, group: &str) -> String {
    match locale {
        Locale::En => group.to_owned(),
        Locale::Zh => {
            let base = group.strip_suffix(" Models").or_else(|| group.strip_suffix(" models")).map_or_else(|| group.to_owned(), |base| format!("{base} 模型"));
            base.replace(" and ", " 和 ")
        }
    }
}

/// The plan as the account names it: "plus" → "Plus".
fn plan_name(plan: &str) -> String {
    let mut chars = plan.chars();
    chars.next().map_or_else(String::new, |first| first.to_uppercase().chain(chars).collect())
}

/// One window: its name, a thin bar of what is left, the share and when it starts over. The share and the time keep
/// fixed widths, so the bars of a group line up.
fn window_row(window: &QuotaWindow, locale: Locale, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let left = window.remaining.min(100);
    let (fill, text) = if left < VERY_LOW {
        (theme.danger, theme.danger)
    } else if left < LOW {
        (theme.warning, crate::theme::warning_text(cx))
    } else {
        (theme.muted_foreground.opacity(0.55), theme.foreground)
    };
    let reset = window.resets_at.and_then(|at| reset_text(locale, at)).unwrap_or_default();
    h_flex()
        .gap_2()
        .items_center()
        .text_xs()
        .child(div().w(px(40.)).flex_shrink_0().text_color(theme.muted_foreground).whitespace_nowrap().child(window_name(locale, window)))
        .child(
            div()
                .flex_1()
                .min_w(px(24.))
                .h(px(4.))
                .rounded_full()
                .bg(theme.muted)
                .child(div().h_full().rounded_full().w(relative(f32::from(left) / 100.)).bg(fill)),
        )
        .child(div().min_w(px(42.)).flex_shrink_0().text_right().whitespace_nowrap().text_color(text).child(i18n::format(locale, "external.quotaLeft", &[("percent", &left)])))
        .child(div().min_w(px(70.)).flex_shrink_0().text_right().whitespace_nowrap().text_color(theme.muted_foreground).child(reset))
        .into_any_element()
}

/// `windows` by the group they count, groups in the order the agent lists them.
fn grouped<'a>(quota: &'a ExternalQuota, windows: &[&'a QuotaWindow]) -> Vec<(Option<&'a str>, Vec<&'a QuotaWindow>)> {
    let mut groups: Vec<(Option<&str>, Vec<&QuotaWindow>)> = Vec::new();
    for group in quota.windows.iter().map(|window| window.group.as_deref()) {
        if !groups.iter().any(|(known, _)| *known == group) {
            groups.push((group, Vec::new()));
        }
    }
    for window in windows {
        if let Some((_, members)) = groups.iter_mut().find(|(group, _)| *group == window.group.as_deref()) {
            members.push(window);
        }
    }
    groups.retain(|(_, members)| !members.is_empty());
    groups
}

/// The limits of `agent` that count `model` (all of them when `model` is `None` or fits no group): under a small
/// heading naming the plan, or the group when only one is shown; several groups each under their name. While none
/// were read, what happens instead.
pub fn section(agent: &ExternalAgentStatus, model: Option<&str>, locale: Locale, cx: &App) -> Option<AnyElement> {
    if !readable(&agent.kind) || !agent.installed {
        return None;
    }
    let theme = cx.theme();
    let muted = theme.muted_foreground;
    let heading = |aside: Option<String>| {
        h_flex()
            .gap_2()
            .items_center()
            .text_xs()
            .text_color(muted)
            .child(div().flex_shrink_0().font_medium().child(i18n::text(locale, "external.quotaTitle")))
            .when_some(aside, |this, aside| this.child(div().flex_1().min_w_0().text_right().overflow_hidden().text_ellipsis().whitespace_nowrap().child(aside)))
    };
    let Some(quota) = agent.quota.as_ref() else {
        return Some(v_flex().gap_1().child(heading(None)).child(div().text_xs().text_color(muted).child(i18n::text(locale, "external.quotaReading"))).into_any_element());
    };
    let windows = quota.windows_for(model);
    if windows.is_empty() {
        let text = match quota.error.as_deref() {
            Some(error) => i18n::format(locale, "external.quotaFailed", &[("error", &error)]),
            None => i18n::text(locale, "external.quotaNone").to_owned(),
        };
        return Some(v_flex().gap_1().child(heading(None)).child(div().text_xs().text_color(muted).child(text)).into_any_element());
    }
    let groups = grouped(quota, &windows);
    let several = groups.len() > 1;
    let plan = quota.plan.as_deref().map(plan_name);
    let aside = if several { plan } else { groups.first().and_then(|(group, _)| *group).map(|group| group_name(locale, group)).or(plan) };
    let blocks = groups.into_iter().map(|(group, windows)| {
        v_flex()
            .gap_1()
            .when_some(group.filter(|_| several), |this, group| this.child(div().pt_0p5().text_xs().text_color(muted).child(group_name(locale, group))))
            .children(windows.into_iter().map(|window| window_row(window, locale, cx)))
    });
    Some(
        v_flex()
            .gap_1()
            .child(heading(aside))
            .children(blocks)
            .when_some(quota.error.as_deref(), |this, error| {
                this.child(div().text_xs().text_color(muted).overflow_hidden().text_ellipsis().whitespace_nowrap().child(i18n::format(locale, "external.quotaStale", &[("error", &error)])))
            })
            .into_any_element(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    // `gpui_kit::*` brings GPUI's own `test` macro; use Rust's.
    use ::core::prelude::v1::test;

    #[test]
    fn names_windows_groups_and_plans() {
        let window = |minutes| QuotaWindow { minutes, ..QuotaWindow::default() };
        assert_eq!(window_name(Locale::Zh, &window(Some(300))), "5 小时");
        assert_eq!(window_name(Locale::En, &window(Some(10080))), "Weekly");
        assert_eq!(window_name(Locale::Zh, &window(Some(120))), "2 小时");
        assert_eq!(window_name(Locale::Zh, &window(None)), "额度");
        assert_eq!(group_name(Locale::Zh, "Gemini Models"), "Gemini 模型");
        assert_eq!(group_name(Locale::Zh, "Claude and GPT models"), "Claude 和 GPT 模型");
        assert_eq!(group_name(Locale::En, "Gemini Models"), "Gemini Models");
        assert_eq!(plan_name("plus"), "Plus");
    }

    #[test]
    fn says_when_a_window_starts_over() {
        let now = Local::now().timestamp();
        let soon = reset_text(Locale::Zh, now + 60).unwrap();
        assert!(soon.ends_with("重置"), "{soon}");
        let later = reset_text(Locale::Zh, now + 6 * 86_400).unwrap();
        assert!(later.contains('月') && later.contains('日'), "{later}");
        assert!(reset_text(Locale::En, now + 6 * 86_400).unwrap().starts_with("resets "));
    }

    #[test]
    fn finds_the_least_left_for_a_model() {
        let window = |group: &str, minutes, remaining| QuotaWindow { group: Some(group.into()), minutes: Some(minutes), remaining, resets_at: None };
        let agent = ExternalAgentStatus {
            kind: "antigravity".into(),
            quota: Some(ExternalQuota {
                windows: vec![window("Gemini Models", 10080, 90), window("Gemini Models", 300, 12), window("Claude and GPT models", 300, 70)],
                ..ExternalQuota::default()
            }),
            ..ExternalAgentStatus::default()
        };
        assert_eq!(lowest(&agent, Some("gemini-3.1-pro")), Some(12));
        assert_eq!(lowest(&agent, Some("claude-opus-5-5")), Some(70));
        assert_eq!(lowest(&agent, None), Some(12));
        assert_eq!(lowest(&ExternalAgentStatus::default(), None), None);
        let quota = agent.quota.as_ref().unwrap();
        let all = quota.windows_for(None);
        let groups = grouped(quota, &all);
        assert_eq!(groups.iter().map(|(group, members)| (*group, members.len())).collect::<Vec<_>>(), [(Some("Gemini Models"), 2), (Some("Claude and GPT models"), 1)]);
        assert_eq!(groups[0].1[0].minutes, Some(300));
    }
}
