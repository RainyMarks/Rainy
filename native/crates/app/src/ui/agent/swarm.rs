//! The swarm pane of Agent mode: the board of the selected chat's swarm, newest post at the bottom, and its members
//! with their state; a member opens its chat. It follows whichever chat is selected, coordinator or worker.
//!
//! For an external agent's chat it shows the team that agent runs instead (an Antigravity swarm or teamwork, Codex
//! sub-agents; `external.team`): the board, every member in the tree they were started in with what it does now
//! (a member opens its latest steps, `external.teamMember`), and the swarm's plan. That team is the external agent's
//! own, so the pane only shows it.

use std::collections::HashSet;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Icon, Selectable as _, Sizable as _, StyledExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use rainy_protocol::{ExternalTeam, SessionSummary, TeamActivity, TeamMember, TeamPost};
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
    Plan,
}

/// Posts of an external team drawn at most; older ones stay in the store.
const TEAM_POSTS_SHOWN: usize = 150;
/// Characters of a long post shown before it is opened.
const POST_PREVIEW: usize = 280;

pub struct SwarmView {
    store: Entity<Store>,
    /// The coordinator whose board is shown.
    coordinator: Option<String>,
    posts: Vec<Post>,
    /// The external agent's chat whose team is shown instead.
    team: Option<String>,
    /// The team member whose latest steps are open, and those steps once read.
    open_member: Option<String>,
    steps: Option<Vec<TeamActivity>>,
    /// Monotonic guard so out-of-order `external.teamMember` responses cannot bleed across chats/members.
    steps_seq: u64,
    /// Last signature of the open member's activity so 1.5s team polls do not refetch unchanged steps.
    steps_signature: Option<(String, Option<String>, Option<String>, usize)>,
    /// Long posts the user opened.
    open_posts: HashSet<u64>,
    /// Frames left that scroll the board to its newest post: a pane that just opened lays out more than once.
    settle: u8,
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
            StoreEvent::HostReconnected(_) => this.reload(cx),
            StoreEvent::TeamChanged(id) => {
                if this.team.as_deref() == Some(id.as_str()) {
                    this.team_changed(cx);
                } else {
                    this.follow(cx);
                }
            }
            StoreEvent::Host { name, data } if name == "swarm.post" => this.received(data, cx),
            _ => {}
        })];
        let mut view = Self {
            store,
            coordinator: None,
            posts: Vec::new(),
            team: None,
            open_member: None,
            steps: None,
            steps_seq: 0,
            steps_signature: None,
            open_posts: HashSet::new(),
            settle: 0,
            tab: Tab::Board,
            scroll: ScrollHandle::new(),
            _subscriptions: subscriptions,
        };
        view.follow(cx);
        view
    }

    fn selected_coordinator(&self, cx: &App) -> Option<String> {
        let store = self.store.read(cx);
        let id = store.selected.as_ref()?;
        store.summaries.iter().find(|summary| summary.id == *id).and_then(coordinator_of)
    }

    /// The selected chat when an external agent runs a team for it.
    fn selected_team(&self, cx: &App) -> Option<String> {
        let store = self.store.read(cx);
        let id = store.selected.as_ref()?;
        (store.has_team(id) && store.summaries.iter().any(|summary| summary.id == *id && summary.swarm.is_none())).then(|| id.clone())
    }

    /// Show the swarm of the selected chat, reading its board when it changed.
    fn follow(&mut self, cx: &mut Context<Self>) {
        let team = self.selected_team(cx);
        if team != self.team {
            self.team = team.clone();
            self.open_member = None;
            self.steps = None;
            self.steps_seq = self.steps_seq.wrapping_add(1);
            self.steps_signature = None;
            self.open_posts.clear();
            // Another team opens on its board.
            self.tab = Tab::Board;
            self.scroll.scroll_to_bottom();
            self.settle = 3;
        }
        if team.is_some() {
            self.coordinator = None;
            self.posts.clear();
            cx.notify();
            return;
        }
        let coordinator = self.selected_coordinator(cx);
        if coordinator == self.coordinator {
            cx.notify();
            return;
        }
        self.coordinator = coordinator.clone();
        self.posts.clear();
        cx.notify();
        if let Some(coordinator) = coordinator {
            self.load_board(coordinator, cx);
        }
    }

    /// Re-sync after a Host reconnect so posts sent while disconnected are backfilled even if the selected coordinator did not change.
    fn reload(&mut self, cx: &mut Context<Self>) {
        if let Some(member) = self.open_member.clone()
            && self.team.is_some()
        {
            self.steps_signature = None;
            self.read_steps(member, cx);
            cx.notify();
            return;
        }
        let coordinator = self.selected_coordinator(cx);
        self.coordinator = coordinator.clone();
        if let Some(coordinator) = coordinator {
            self.load_board(coordinator, cx);
        } else {
            self.posts.clear();
            cx.notify();
        }
    }

    fn load_board(&mut self, coordinator: String, cx: &mut Context<Self>) {
        let call = self.store.read(cx).call("swarm.get", json!({ "sessionId": coordinator }));
        cx.spawn(async move |this, cx| {
            let Ok(value) = call.await else { return };
            let posts: Vec<Post> = serde_json::from_value(value["posts"].clone()).unwrap_or_default();
            let _ = this.update(cx, |this, cx| {
                if this.coordinator.as_deref() == Some(coordinator.as_str()) {
                    let was_empty = this.posts.is_empty();
                    let follow = was_empty || this.board_at_bottom();
                    this.posts = posts;
                    if follow {
                        this.scroll.scroll_to_bottom();
                    }
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
        let follow = self.tab == Tab::Board && self.board_at_bottom();
        self.posts.push(post);
        if follow {
            self.scroll.scroll_to_bottom();
        }
        cx.notify();
    }

    /// The shown team changed: new posts scroll into view unless the user scrolled up to read, an open member's
    /// steps are read again when its activity signature changed.
    fn team_changed(&mut self, cx: &mut Context<Self>) {
        if self.tab == Tab::Board && self.board_at_bottom() {
            self.scroll.scroll_to_bottom();
            self.settle = 2;
        }
        if self.tab == Tab::Members
            && let Some(member) = self.open_member.clone()
            && let Some(session) = self.team.as_deref()
        {
            let sig = self
                .store
                .read(cx)
                .teams
                .get(session)
                .and_then(|team| team.members.iter().find(|m| m.id == member))
                .map(|m| (m.status.clone(), m.focus.clone(), m.activity.clone(), m.steps.len()));
            if sig != self.steps_signature {
                self.steps_signature = sig;
                self.read_steps(member, cx);
            }
        }
        cx.notify();
    }

    /// Whether the board shows its newest post.
    fn board_at_bottom(&self) -> bool {
        let max = self.scroll.max_offset().y;
        max <= px(1.) || self.scroll.offset().y <= -max + px(32.)
    }

    fn read_steps(&mut self, member: String, cx: &mut Context<Self>) {
        let Some(session) = self.team.clone() else { return };
        self.steps_seq = self.steps_seq.wrapping_add(1);
        let seq = self.steps_seq;
        let call = self.store.read(cx).call("external.teamMember", json!({ "sessionId": session, "memberId": member }));
        cx.spawn(async move |this, cx| {
            let steps: Vec<TeamActivity> = call.await.ok().and_then(|value| serde_json::from_value(value).ok()).unwrap_or_default();
            let _ = this.update(cx, |this, cx| {
                if this.steps_seq == seq
                    && this.team.as_deref() == Some(session.as_str())
                    && this.open_member.as_deref() == Some(member.as_str())
                {
                    this.steps = Some(steps);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn toggle_member(&mut self, member: String, cx: &mut Context<Self>) {
        if self.open_member.as_deref() == Some(member.as_str()) {
            self.open_member = None;
            self.steps = None;
            self.steps_seq = self.steps_seq.wrapping_add(1);
            self.steps_signature = None;
        } else {
            self.open_member = Some(member.clone());
            self.steps = None;
            self.steps_signature = self.team.as_deref().and_then(|session| {
                self.store
                    .read(cx)
                    .teams
                    .get(session)
                    .and_then(|team| team.members.iter().find(|m| m.id == member))
                    .map(|m| (m.status.clone(), m.focus.clone(), m.activity.clone(), m.steps.len()))
            });
            self.read_steps(member, cx);
        }
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
            .child(div().min_w_0().truncate().text_sm().font_semibold().child(if lead { tr(cx, "agent.swarmCoordinator").to_string() } else { format!("@{name}") }))
            .when(member.worktree.is_some(), |this| this.child(Icon::new(gpui_kit::assets::IconName::GitBranch).xsmall().text_color(cx.theme().muted_foreground)))
            .child(div().flex_1().min_w_0().truncate().text_xs().text_color(cx.theme().muted_foreground).child(if lead { member.title.clone() } else { String::new() }))
            .child(div().flex_none().text_xs().text_color(cx.theme().muted_foreground).child(tr(cx, state)))
            .on_click(cx.listener(move |this, _, _, cx| this.open(id.clone(), cx)))
            .into_any_element()
    }

    // ── an external agent's team ──

    /// The color and label of a member's state.
    fn team_state(status: &str, cx: &App) -> (Hsla, &'static str) {
        let theme = cx.theme();
        match status {
            "running" => (theme.warning, "agent.teamRunning"),
            "waiting" => (theme.link, "agent.teamWaiting"),
            "done" => (theme.success, "agent.teamDone"),
            "error" => (theme.danger, "agent.teamError"),
            "killed" => (theme.muted_foreground.opacity(0.5), "agent.teamKilled"),
            _ => (theme.success, "agent.teamIdle"),
        }
    }

    fn kind_label(kind: &str) -> Option<&'static str> {
        Some(match kind {
            "worker" => "agent.kindWorker",
            "verifier" => "agent.kindVerifier",
            "monitor" => "agent.kindMonitor",
            "teamwork" => "agent.kindTeamwork",
            "orchestrator" => "agent.kindOrchestrator",
            "explorer" => "agent.kindExplorer",
            "challenger" => "agent.kindChallenger",
            "interviewer" => "agent.kindInterviewer",
            "auditor" => "agent.kindAuditor",
            "browser" => "agent.kindBrowser",
            "subagent" => "agent.kindSubagent",
            _ => return None,
        })
    }

    /// `@name` for a swarm or Codex handle, the role's own words for teamwork members, the lead by its title.
    fn member_name(member: &TeamMember, cx: &App) -> String {
        if member.kind == "lead" {
            tr(cx, "agent.teamLead").to_string()
        } else if matches!(member.kind.as_str(), "worker" | "verifier" | "monitor" | "subagent") {
            format!("@{}", member.name)
        } else {
            member.name.clone()
        }
    }

    /// `HH:MM` of a post: the local time the agent wrote, else its timestamp.
    fn post_time(post: &TeamPost) -> String {
        if let Some(time) = post.time.as_deref().and_then(|time| time.get(11..16)) {
            return time.to_owned();
        }
        if post.ts <= 0.0 {
            return String::new();
        }
        chrono::DateTime::from_timestamp_millis(post.ts as i64).map(|time| time.with_timezone(&chrono::Local).format("%H:%M").to_string()).unwrap_or_default()
    }

    fn render_team_post(&self, post: &TeamPost, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let who = |name: &str| if name == "lead" { tr(cx, "agent.teamLead").to_string() } else { format!("@{name}") };
        let to = post.to.iter().map(|name| who(name)).collect::<Vec<_>>().join(" ");
        let long = post.text.chars().count() > POST_PREVIEW;
        let open = self.open_posts.contains(&post.seq);
        let text: String = if long && !open { post.text.chars().take(POST_PREVIEW).chain("…".chars()).collect() } else { post.text.clone() };
        let seq = post.seq;
        v_flex()
            .id(("team-post", post.seq as usize))
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
                    .child(div().flex_none().text_color(theme.muted_foreground).child(format!("#{}", post.seq)))
                    .child(div().font_semibold().text_color(theme.foreground).child(who(&post.from)))
                    .when(!to.is_empty(), |this| this.child(div().text_color(theme.muted_foreground).child("→")))
                    .child(div().min_w_0().flex_1().truncate().text_color(theme.muted_foreground).child(to))
                    .child(div().flex_none().text_color(theme.muted_foreground).child(Self::post_time(post))),
            )
            .child(div().text_sm().whitespace_normal().child(text))
            .when(long, |this| {
                this.child(
                    div()
                        .id(("team-post-more", post.seq as usize))
                        .text_xs()
                        .text_color(theme.link)
                        .cursor_pointer()
                        .child(tr(cx, if open { "agent.teamLess" } else { "agent.teamMore" }))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if !this.open_posts.remove(&seq) {
                                this.open_posts.insert(seq);
                            }
                            cx.notify();
                        })),
                )
            })
            .into_any_element()
    }

    fn render_steps(&self, cx: &App) -> AnyElement {
        let theme = cx.theme();
        let Some(steps) = &self.steps else {
            return div().pl_8().pr_3().py_1().text_xs().text_color(theme.muted_foreground).child(tr(cx, "agent.teamLoading")).into_any_element();
        };
        if steps.is_empty() {
            return div().pl_8().pr_3().py_1().text_xs().text_color(theme.muted_foreground).child(tr(cx, "agent.teamNoActivity")).into_any_element();
        }
        let rows: Vec<AnyElement> = steps
            .iter()
            .rev()
            .take(40)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .map(|step| {
                let color = match step.kind.as_str() {
                    "reply" => theme.foreground,
                    "message" => theme.link,
                    "error" => theme.danger,
                    _ => theme.muted_foreground,
                };
                let time = chrono::DateTime::from_timestamp_millis(step.ts as i64).map(|time| time.with_timezone(&chrono::Local).format("%H:%M").to_string()).unwrap_or_default();
                let detail = step.detail.as_deref().filter(|detail| *detail != step.title).map(|detail| detail.chars().take(400).collect::<String>());
                v_flex()
                    .gap_0p5()
                    .child(
                        h_flex()
                            .gap_1p5()
                            .items_start()
                            .text_xs()
                            .child(div().flex_none().w(px(34.)).text_color(theme.muted_foreground).child(time))
                            .child(div().flex_1().min_w_0().whitespace_normal().text_color(color).child(step.title.clone())),
                    )
                    .children(detail.map(|detail| div().pl(px(40.)).text_xs().whitespace_normal().text_color(theme.muted_foreground.opacity(0.8)).child(detail)))
                    .into_any_element()
            })
            .collect();
        v_flex().pl_6().pr_3().py_1().gap_1().border_l_2().border_color(theme.border).ml_4().children(rows).into_any_element()
    }

    fn render_team_member(&self, member: &TeamMember, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (color, state) = Self::team_state(&member.status, cx);
        let open = self.open_member.as_deref() == Some(member.id.as_str());
        let done = member.steps.iter().filter(|step| step.status == "completed").count();
        let progress = (!member.steps.is_empty()).then(|| trf(cx, "agent.teamSteps", &[("done", &done), ("total", &member.steps.len())]).to_string());
        // What it focuses on, else what it did last.
        let now = member.focus.clone().or_else(|| member.activity.clone());
        let id = member.id.clone();
        let indent = px(12. * member.depth.min(6) as f32);
        let row = v_flex()
            .id(SharedString::from(format!("team-member-{}", member.id)))
            .pl(px(12.) + indent)
            .pr_3()
            .py_1p5()
            .gap_0p5()
            .cursor_pointer()
            .hover(|style| style.bg(cx.theme().list_hover))
            .when(open, |this| this.bg(theme.list_hover))
            .child(
                h_flex()
                    .gap_1p5()
                    .items_center()
                    .child(div().size(px(8.)).flex_none().rounded_full().bg(color))
                    .child(div().min_w_0().truncate().text_sm().font_semibold().when(member.status == "killed", |this| this.text_color(theme.muted_foreground)).child(Self::member_name(member, cx)))
                    .children(Self::kind_label(&member.kind).map(|key| {
                        div().flex_none().px_1().rounded(theme.radius).bg(theme.muted).text_xs().text_color(theme.muted_foreground).child(tr(cx, key))
                    }))
                    .child(div().flex_1().min_w_0().truncate().text_xs().text_color(theme.muted_foreground).child(member.model.clone().unwrap_or_default()))
                    .child(div().flex_none().text_xs().text_color(theme.muted_foreground).child(tr(cx, state))),
            )
            .when(now.is_some() || progress.is_some(), |this| {
                this.child(
                    h_flex()
                        .pl(px(14.))
                        .gap_2()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(div().flex_1().min_w_0().truncate().child(now.clone().unwrap_or_default()))
                        .children(progress.clone().map(|progress| div().flex_none().child(progress))),
                )
            })
            .children(member.task.as_ref().map(|task| {
                div().pl(px(14.)).text_xs().text_color(theme.muted_foreground.opacity(0.8)).truncate().child(trf(cx, "agent.teamTask", &[("task", task)]).to_string())
            }))
            .on_click(cx.listener(move |this, _, _, cx| this.toggle_member(id.clone(), cx)));
        v_flex().child(row).when(open, |this| this.child(self.render_steps(cx))).into_any_element()
    }

    fn render_team(&mut self, session: &str, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let (muted_foreground, border, radius, muted) = {
            let theme = cx.theme();
            (theme.muted_foreground, theme.border, theme.radius, theme.muted)
        };
        // Only what is drawn is copied out of the store: the members, the plan and the newest posts.
        let Some((title, goal, source, mut members, threads, posts, post_total)) = self.store.read(cx).teams.get(session).map(|team: &ExternalTeam| {
            let skip = team.posts.len().saturating_sub(TEAM_POSTS_SHOWN);
            (team.title.clone(), team.goal.clone(), team.source.clone(), team.members.clone(), team.threads.clone(), team.posts[skip..].to_vec(), team.posts.len())
        }) else {
            return v_flex().size_full().p_4().text_sm().text_color(muted_foreground).child(tr(cx, "agent.swarmNone")).into_any_element();
        };
        // The chat's own agent works while the chat runs, whatever the team last said.
        let running = self.store.read(cx).summaries.iter().any(|summary| summary.id == session && (summary.status == "running" || summary.status == "compacting"));
        if let Some(lead) = members.iter_mut().find(|member| member.kind == "lead") {
            if running {
                lead.status = "running".into();
            } else if lead.status == "running" {
                lead.status = "idle".into();
            }
        }
        let workers = members.iter().filter(|member| member.kind != "lead" && member.status != "killed").count();
        let tab = |id: &'static str, value: Tab, label: SharedString| {
            Button::new(id).ghost().xsmall().label(label).selected(self.tab == value).on_click(cx.listener(move |this, _, _, cx| {
                this.tab = value;
                if value == Tab::Board {
                    this.scroll.scroll_to_bottom();
                    this.settle = 2;
                }
                cx.notify();
            }))
        };
        let source_label = tr(cx, if source == "codex" { "agent.teamTitleCodex" } else { "agent.teamTitleAntigravity" });
        let header = h_flex()
            .flex_none()
            .flex_wrap()
            .px_2()
            .py_1()
            .gap_1()
            .items_center()
            .border_b_1()
            .border_color(border)
            .child(tab("team-board", Tab::Board, tr(cx, "agent.swarmBoard")))
            .child(tab("team-members", Tab::Members, trf(cx, "agent.swarmMembers", &[("count", &workers)])))
            .when(!threads.is_empty() || goal.is_some(), |this| this.child(tab("team-plan", Tab::Plan, tr(cx, "agent.teamPlan"))))
            .child(div().flex_1())
            .child(div().flex_none().max_w(px(220.)).truncate().text_xs().text_color(muted_foreground).child(title.clone().unwrap_or_else(|| source_label.to_string())));
        let tab_now = if self.tab == Tab::Plan && threads.is_empty() && goal.is_none() { Tab::Board } else { self.tab };
        let body: AnyElement = match tab_now {
            Tab::Board if posts.is_empty() => div().p_4().text_sm().text_color(muted_foreground).child(tr(cx, "agent.swarmEmpty")).into_any_element(),
            Tab::Board => {
                if self.settle > 0 {
                    self.settle -= 1;
                    self.scroll.scroll_to_bottom();
                    let entity = cx.entity().downgrade();
                    window.defer(cx, move |_, cx| {
                        let _ = entity.update(cx, |this, cx| {
                            this.scroll.scroll_to_bottom();
                            cx.notify();
                        });
                    });
                }
                let older = post_total.saturating_sub(posts.len());
                let rows: Vec<AnyElement> = posts.iter().map(|post| self.render_team_post(post, cx)).collect();
                v_flex()
                    .id("team-board")
                    .size_full()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .when(older > 0, |this| this.child(div().px_3().py_1().text_xs().text_color(muted_foreground).child(format!("… +{older}"))))
                    .children(rows)
                    .into_any_element()
            }
            Tab::Members => {
                let rows: Vec<AnyElement> = members.iter().map(|member| self.render_team_member(member, cx)).collect();
                v_flex()
                    .id("team-member-list")
                    .size_full()
                    .overflow_y_scroll()
                    .py_1()
                    .children(rows)
                    .child(div().px_3().pt_2().pb_3().text_xs().whitespace_normal().text_color(muted_foreground.opacity(0.8)).child(tr(cx, "agent.teamReadOnly")))
                    .into_any_element()
            }
            Tab::Plan => {
                let state_of = |name: &str| members.iter().find(|member| member.name == name).map(|member| member.status.clone()).unwrap_or_default();
                let groups: Vec<AnyElement> = threads
                    .iter()
                    .map(|thread| {
                        let chips: Vec<AnyElement> = thread
                            .members
                            .iter()
                            .map(|name| {
                                let (color, _) = Self::team_state(&state_of(name), cx);
                                h_flex()
                                    .gap_1()
                                    .items_center()
                                    .px_1p5()
                                    .rounded(radius)
                                    .bg(muted)
                                    .text_xs()
                                    .child(div().size(px(6.)).rounded_full().bg(color))
                                    .child(format!("@{name}"))
                                    .into_any_element()
                            })
                            .collect();
                        v_flex()
                            .px_3()
                            .py_2()
                            .gap_1()
                            .border_b_1()
                            .border_color(border.opacity(0.5))
                            .child(div().text_sm().font_semibold().child(thread.title.clone()))
                            .children(thread.goal.clone().map(|goal| div().text_xs().whitespace_normal().text_color(muted_foreground).child(goal)))
                            .child(h_flex().flex_wrap().gap_1().children(chips))
                            .into_any_element()
                    })
                    .collect();
                v_flex()
                    .id("team-plan")
                    .size_full()
                    .overflow_y_scroll()
                    .child(
                        v_flex()
                            .px_3()
                            .py_2()
                            .gap_0p5()
                            .border_b_1()
                            .border_color(border)
                            .children(title.map(|title| div().text_sm().font_semibold().child(title)))
                            .children(goal.map(|goal| div().text_xs().whitespace_normal().text_color(muted_foreground).child(goal))),
                    )
                    .children(groups)
                    .into_any_element()
            }
        };
        v_flex().size_full().child(header).child(div().flex_1().min_h_0().child(body)).into_any_element()
    }
}

impl Render for SwarmView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(session) = self.team.clone() {
            return self.render_team(&session, window, cx);
        }
        let (muted_foreground, border) = {
            let theme = cx.theme();
            (theme.muted_foreground, theme.border)
        };
        let Some(coordinator) = self.coordinator.clone() else {
            return v_flex().size_full().p_4().gap_2().text_sm().text_color(muted_foreground).child(tr(cx, "agent.swarmNone")).into_any_element();
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
        // Wraps in IDE mode's narrow left pane rather than pushing the stop button out.
        let header = h_flex()
            .flex_none()
            .flex_wrap()
            .px_2()
            .py_1()
            .gap_1()
            .items_center()
            .border_b_1()
            .border_color(border)
            .child(tab("swarm-board", Tab::Board, tr(cx, "agent.swarmBoard")))
            .child(tab("swarm-members", Tab::Members, trf(cx, "agent.swarmMembers", &[("count", &workers)])))
            .child(div().flex_1())
            .child(stop);
        let body: AnyElement = match self.tab {
            Tab::Board if self.posts.is_empty() => div().p_4().text_sm().text_color(muted_foreground).child(tr(cx, "agent.swarmEmpty")).into_any_element(),
            Tab::Board | Tab::Plan => {
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
