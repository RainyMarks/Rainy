//! Skills card inside Settings → Skills & MCP.

use gpui_kit::component::button::Button;
use gpui_kit::component::tag::Tag;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, Size, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::ExtensionsPage;
use crate::store::ide::Ide;
use crate::ui::settings::models_extra::card;
use crate::ui::{motion, tr, trf};

impl ExtensionsPage {
    pub(super) fn render_skills(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let project = Ide::global(cx).read(cx).workspace.as_ref().map(|workspace| workspace.path.clone());
        let name = tr(cx, "settings.skillsName");
        let root = project.unwrap_or_else(|| tr(cx, "settings.skillsProject").to_string());
        let separator = if root.contains('\\') && !root.contains('/') { '\\' } else { '/' };
        let project_path = [root.trim_end_matches(['\\', '/']), ".rainy", "skills", &name, "SKILL.md"].join(&separator.to_string());
        let user_path = format!("~/.rainy-agent/skills/{name}/SKILL.md");
        let mut body = vec![div().text_xs().text_color(theme.muted_foreground).child(trf(cx, "settings.skillsHint", &[("project", &project_path), ("user", &user_path)])).into_any_element()];
        match &self.status {
            None => body.push(h_flex().child(motion::spinner(Size::Small, cx)).into_any_element()),
            Some(status) if status.skills.is_empty() => body.push(div().text_sm().text_color(theme.muted_foreground).child(tr(cx, "settings.settingsNoSkills")).into_any_element()),
            Some(status) => {
                for skill in &status.skills {
                    let scope = tr(cx, if skill.scope == "project" { "settings.skillScopeProject" } else { "settings.skillScopeUser" });
                    body.push(
                        v_flex()
                            .id(SharedString::from(format!("skill-{}", skill.id)))
                            .gap_0p5()
                            .py_1()
                            .child(h_flex().gap_2().items_center().text_sm().child(skill.name.clone()).child(Tag::info().small().child(scope)))
                            .when(!skill.description.is_empty(), |this| this.child(div().text_xs().text_color(theme.muted_foreground).child(skill.description.clone())))
                            .child(div().text_xs().font_family(theme.mono_font_family.clone()).text_color(theme.muted_foreground).child(skill.path.clone()))
                            .into_any_element(),
                    );
                }
            }
        }
        body.insert(
            0,
            h_flex()
                .justify_end()
                .child(Button::new("skills-refresh").outline().small().label(tr(cx, "settings.settingsRefresh")).disabled(self.guard.busy()).on_click(cx.listener(|this, _, window, cx| this.read(window, cx))))
                .into_any_element(),
        );
        card(cx, "settings.skillsTitle", None, body)
    }
}
