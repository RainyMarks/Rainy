//! The swarm pane of Agent mode: the board of the selected chat's swarm, newest post at the bottom, and its members
//! with their state; a member opens its chat. It follows whichever chat is selected, coordinator or worker.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Icon, Selectable as _, Sizable as _, StyledExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use rainy_protocol::SessionSummary;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::store::{Store, StoreEvent};
use crate::ui::{tr, trf};

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct Post {
    id: String,
    ts: f64,
    from: String,
    to: Vec<String>,
    text: String,
    #[serde(default)]
    auto: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Board,
    Members,
}

pub struct SwarmView {
    store: Entity<Store>,
    /// The coordinator whose board is shown.
    coordinator: Option<String>,
    posts: Vec<Post>,
    tab: Tab,
    scroll: ScrollHandle,
    _subscriptions: Vec<Subscription>,
}

/// The coordinator of the chat's swarm.
pub fn coordinator_of(summary: &SessionSummary) -> Option<String> {
    summary.swarm.as_ref().map(|link| link.coordinator.clone())
}

impl SwarmView {
    pub fn new(store: Entity<Store>, cx: &mut Context<Self>) -> Self {
        let subscriptions = vec![cx.subscribe(&store, |this, _, event: &StoreEvent, cx| match event {
            StoreEvent::SelectionChanged | StoreEvent::SummariesChanged => this.follow(cx),
            StoreEvent::Host { name, data } if name == "swarm.post" => this.received(data, cx),
            _ => {}
        })];
        let mut view = Self { store, coordinator: None, posts: Vec::new(), tab: Tab::Board, scroll: ScrollHandle::new(), _subscriptions: subscriptions };
        view.follow(cx);
        view
    }

    fn selected_coordinator(&self, cx: &App) -> Option<String> {
        let store = self.store.read(cx);
        let id = store.selected.as_ref()?;
        store.summaries.iter().find(|summary| summary.id == *id).and_then(coordinator_of)
    }

    /// Show the swarm of the selected chat, reading its board when it changed.
    fn follow(&mut self, cx: &mut Context<Self>) {
        let coordinator = self.selected_coordinator(cx);
        if coordinator == self.coordinator {
            cx.notify();
            return;
        }
        self.coordinator = coordinator.clone();
        self.posts.clear();
        cx.notify();
        let Some(coordinator) = coordinator else { return };
        let call = self.store.read(cx).call("swarm.get", json!({ "sessionId": coordinator }));
        cx.spawn(async move |this, cx| {
            let Ok(value) = call.await else { return };
            let posts: Vec<Post> = serde_json::from_value(value["posts"].clone()).unwrap_or_default();
            let _ = this.update(cx, |this, cx| {
                if this.coordinator.as_deref() == Some(coordinator.as_str()) {
                    this.posts = posts;
                    this.scroll.scroll_to_bottom();
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn received(&mut self, data: &Value, cx: &mut Context<Self>) {
        if data["coordinatorId"].as_str() != self.coordinator.as_deref() {
            return;
        }
        let Ok(post) = serde_json::from_value::<Post>(data["post"].clone()) else { return };
        if self.posts.iter().any(|known| known.id == post.id) {
            return;
        }
        self.posts.push(post);
        self.scroll.scroll_to_bottom();
        cx.notify();
    }

    /// The swarm's chats: the coordinator first, then the workers in the order they were added.
    fn members(&self, cx: &App) -> Vec<SessionSummary> {
        let Some(coordinator) = &self.coordinator else { return Vec::new() };
        let mut members: Vec<SessionSummary> = self.store.read(cx).summaries.iter().filter(|summary| coordinator_of(summary).as_ref() == Some(coordinator)).cloned().collect();
        members.sort_by(|left, right| (left.id != *coordinator).cmp(&(right.id != *coordinator)).then(left.created_at.total_cmp(&right.created_at)));
        members
    }

    fn open(&self, id: String, cx: &mut App) {
        self.store.update(cx, |store, cx| store.select(Some(id), cx));
    }

    fn render_post(&self, index: usize, post: &Post, cx: &App) -> AnyElement {
        let theme = cx.theme();
        let to = post.to.iter().map(|to| format!("@{to}")).collect::<Vec<_>>().join(" ");
        let time = chrono::DateTime::from_timestamp_millis(post.ts as i64).map(|time| time.with_timezone(&chrono::Local).format("%H:%M").to_string()).unwrap_or_default();
        v_flex()
            .id(("swarm-post", index))
            .px_3()
            .py_2()
            .gap_1()
            .border_b_1()
            .border_color(theme.border.opacity(0.5))
            .child(
                h_flex()
                    .gap_1p5()
                    .items_center()
                    .text_xs()
                    .child(div().font_semibold().text_color(theme.foreground).child(format!("@{}", post.from)))
                    .child(div().text_color(theme.muted_foreground).child("→"))
                    .child(div().min_w_0().flex_1().truncate().text_color(theme.muted_foreground).child(to))
                    .when(post.auto, |this| this.child(div().px_1().rounded(theme.radius).bg(theme.muted).text_color(theme.muted_foreground).child(tr(cx, "agent.swarmAuto"))))
                    .child(div().flex_none().text_color(theme.muted_foreground).child(time)),
            )
            .child(div().text_sm().whitespace_normal().child(post.text.clone()))
            .into_any_element()
    }

    fn render_member(&self, index: usize, member: &SessionSummary, coordinator: &str, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let name = member.swarm.as_ref().map(|link| link.name.clone()).unwrap_or_default();
        let (color, state) = if member.archived {
            (theme.muted_foreground.opacity(0.5), "agent.swarmRetired")
        } else if member.awaiting_approval == Some(true) {
            (theme.danger, "agent.swarmApproval")
        } else if member.status == "running" || member.status == "compacting" {
            (theme.warning, "agent.swarmRunning")
        } else if member.status == "error" {
            (theme.danger, "agent.swarmError")
        } else {
            (theme.success, "agent.swarmIdle")
        };
        let id = member.id.clone();
        let lead = member.id == coordinator;
        h_flex()
            .id(("swarm-member", index))
            .px_3()
            .py_1p5()
            .gap_2()
            .items_center()
            .cursor_pointer()
            .hover(|style| style.bg(cx.theme().list_hover))
            .child(div().size(px(8.)).flex_none().rounded_full().bg(color))
            .child(div().flex_none().text_sm().font_semibold().child(if lead { tr(cx, "agent.swarmCoordinator").to_string() } else { format!("@{name}") }))
            .when(member.worktree.is_some(), |this| this.child(Icon::new(gpui_kit::assets::IconName::GitBranch).xsmall().text_color(cx.theme().muted_foreground)))
            .child(div().flex_1().min_w_0().truncate().text_xs().text_color(cx.theme().muted_foreground).child(if lead { member.title.clone() } else { String::new() }))
            .child(div().flex_none().text_xs().text_color(cx.theme().muted_foreground).child(tr(cx, state)))
            .on_click(cx.listener(move |this, _, _, cx| this.open(id.clone(), cx)))
            .into_any_element()
    }
}

impl Render for SwarmView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let Some(coordinator) = self.coordinator.clone() else {
            return v_flex().size_full().p_4().gap_2().text_sm().text_color(theme.muted_foreground).child(tr(cx, "agent.swarmNone")).into_any_element();
        };
        let members = self.members(cx);
        let workers = members.iter().filter(|member| member.id != coordinator && !member.archived).count();
        let running = members.iter().any(|member| !member.archived && (member.status == "running" || member.status == "compacting"));
        let tab = |id: &'static str, value: Tab, label: SharedString| {
            Button::new(id).ghost().xsmall().label(label).selected(self.tab == value).on_click(cx.listener(move |this, _, _, cx| {
                this.tab = value;
                if value == Tab::Board {
                    this.scroll.scroll_to_bottom();
                }
                cx.notify();
            }))
        };
        let stop = {
            let coordinator = coordinator.clone();
            Button::new("swarm-stop").ghost().xsmall().icon(gpui_kit::assets::IconName::CircleStop).label(tr(cx, "agent.swarmStop")).disabled(!running).on_click(cx.listener(move |this, _, _, cx| {
                let call = this.store.read(cx).call("swarm.stop", json!({ "sessionId": coordinator }));
                cx.background_spawn(async move { drop(call.await) }).detach();
            }))
        };
        let header = h_flex()
            .flex_none()
            .px_2()
            .py_1()
            .gap_1()
            .items_center()
            .border_b_1()
            .border_color(theme.border)
            .child(tab("swarm-board", Tab::Board, tr(cx, "agent.swarmBoard")))
            .child(tab("swarm-members", Tab::Members, trf(cx, "agent.swarmMembers", &[("count", &workers)])))
            .child(div().flex_1())
            .child(stop);
        let body: AnyElement = match self.tab {
            Tab::Board if self.posts.is_empty() => div().p_4().text_sm().text_color(theme.muted_foreground).child(tr(cx, "agent.swarmEmpty")).into_any_element(),
            Tab::Board => {
                let rows: Vec<AnyElement> = self.posts.iter().enumerate().map(|(index, post)| self.render_post(index, post, cx)).collect();
                v_flex().id("swarm-board").size_full().overflow_y_scroll().track_scroll(&self.scroll).children(rows).into_any_element()
            }
            Tab::Members => {
                let rows: Vec<AnyElement> = members.iter().enumerate().map(|(index, member)| self.render_member(index, member, &coordinator, cx)).collect();
                v_flex().id("swarm-member-list").size_full().overflow_y_scroll().py_1().children(rows).into_any_element()
            }
        };
        v_flex().size_full().child(header).child(div().flex_1().min_h_0().child(body)).into_any_element()
    }
}
