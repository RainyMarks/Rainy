//! General: language and chat preferences (`GeneralSection.tsx`).

use gpui_kit::component::switch::Switch;
use gpui_kit::component::v_flex;
use gpui_kit::*;
use serde_json::json;

use super::{group, heading, row, segmented, set_prefs};
use crate::store::Store;
use crate::ui::{tr, trf};

pub fn render(store: &Entity<Store>, _: &mut Window, cx: &mut App) -> impl IntoElement + use<> {
    let state = store.read(cx);
    let prefs = state.prefs.clone();
    let version = state.info.as_ref().map(|info| info.version.clone()).unwrap_or_default();
    let t = |key: &str| tr(cx, &format!("settings.{key}"));
    v_flex()
        .gap_4()
        .child(heading(cx, "settings.navGeneral", "settings.generalLead"))
        .child(group(cx, None, vec![row(
            cx,
            t("languageTitle"),
            None,
            segmented("locale", vec![("zh", t("languageZh")), ("en", t("languageEn"))], &prefs.locale, |value, window, cx| {
                set_prefs(json!({ "locale": value }), window, cx);
            }),
        )]))
        .child(group(cx, Some("settings.groupChat"), vec![
            row(
                cx,
                t("busyEnterTitle"),
                Some(t("busyEnterDescription")),
                segmented("busy-enter", vec![("queue", t("busyEnterQueue")), ("steer", t("busyEnterSteer"))], &prefs.busy_enter, |value, window, cx| {
                    set_prefs(json!({ "busyEnter": value }), window, cx);
                }),
            ),
            row(
                cx,
                t("stepDetailTitle"),
                Some(t("stepDetailDescription")),
                segmented(
                    "step-detail",
                    vec![("compact", t("stepDetailCompact")), ("standard", t("stepDetailStandard")), ("detailed", t("stepDetailDetailed"))],
                    &prefs.step_detail,
                    |value, window, cx| set_prefs(json!({ "stepDetail": value }), window, cx),
                ),
            ),
            row(
                cx,
                t("usageTitle"),
                Some(t("usageDescription")),
                Switch::new("show-usage").checked(prefs.show_usage).on_click(|checked: &bool, window, cx| {
                    set_prefs(json!({ "showUsage": *checked }), window, cx);
                }),
            ),
        ]))
        .child(group(cx, Some("settings.groupAbout"), vec![row(cx, trf(cx, "settings.currentVersion", &[("version", &version)]), None, div())]))
}
