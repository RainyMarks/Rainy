//! External agent team state (`external.team`) with per-session sequence guards, in-flight deduplication and bounded LRU eviction.

use gpui_kit::*;
use rainy_protocol::ExternalTeam;
use serde_json::json;

use crate::store::{Store, StoreEvent};

/// Maximum number of external agent teams kept cached simultaneously.
const MAX_CACHED_TEAMS: usize = 16;

impl Store {
    /// Whether the chat's external agent runs a team: anyone besides itself, or a board.
    pub fn has_team(&self, id: &str) -> bool {
        self.teams.get(id).is_some_and(|team| team.members.len() > 1 || !team.posts.is_empty())
    }

    pub(super) fn clear_team(&mut self, id: &str) {
        self.teams.remove(id);
        self.team_fetch_seq.remove(id);
        self.team_inflight.remove(id);
        self.team_used.remove(id);
    }

    pub(super) fn clear_all_teams(&mut self) {
        self.teams.clear();
        self.team_fetch_seq.clear();
        self.team_inflight.clear();
        self.team_used.clear();
    }

    fn touch_team(&mut self, id: &str) {
        self.team_tick = self.team_tick.wrapping_add(1);
        self.team_used.insert(id.to_owned(), self.team_tick);
    }

    fn evict_lru_teams(&mut self, keep_id: &str) {
        while self.teams.len() > MAX_CACHED_TEAMS {
            let victim = self
                .teams
                .keys()
                .filter(|id| id.as_str() != keep_id && self.selected.as_deref() != Some(id.as_str()))
                .min_by_key(|id| self.team_used.get(id.as_str()).copied().unwrap_or(0))
                .cloned();
            let Some(victim) = victim else { break };
            self.clear_team(&victim);
        }
    }

    /// Read the team of an external agent's chat: the board posts newer than the ones held, everything else anew.
    pub fn fetch_team(&mut self, id: String, cx: &mut Context<Self>) {
        let after = self.teams.get(&id).map(|team| team.last_post);
        // Deduplicate identical in-flight polls for the same session & cursor.
        if self.team_inflight.get(&id).is_some_and(|inflight| *inflight == after) {
            return;
        }
        let seq = self.team_fetch_seq.entry(id.clone()).and_modify(|s| *s = s.wrapping_add(1)).or_insert(1);
        let seq = *seq;
        let generation = self.host_generation;
        self.team_inflight.insert(id.clone(), after);
        let call = self.call("external.team", json!({ "sessionId": id, "after": after }));
        cx.spawn(async move |this: WeakEntity<Store>, cx: &mut AsyncApp| {
            let result = call.await;
            let _ = this.update(cx, |store, cx| {
                if store.team_fetch_seq.get(&id) != Some(&seq) || store.host_generation != generation {
                    return;
                }
                store.team_inflight.remove(&id);
                let Ok(value) = result else { return };
                let Ok(parsed) = serde_json::from_value::<Option<ExternalTeam>>(value) else { return };
                let Some(mut team) = parsed else {
                    // The external agent no longer reports a team for this session: evict any stale cache.
                    if store.teams.remove(&id).is_some() {
                        store.team_used.remove(&id);
                        cx.emit(StoreEvent::TeamChanged(id.clone()));
                        cx.notify();
                    }
                    return;
                };
                match store.teams.get_mut(&id) {
                    Some(known) => {
                        let newest = known.posts.last().map(|post| post.seq).unwrap_or(0);
                        // The board was written anew (a new swarm starts at post 1 again): read it whole once more.
                        if team.last_post < newest && after.is_some() {
                            store.teams.remove(&id);
                            store.team_used.remove(&id);
                            store.fetch_team(id.clone(), cx);
                            return;
                        }
                        let mut posts = std::mem::take(&mut known.posts);
                        posts.extend(team.posts.drain(..).filter(|post| post.seq > newest));
                        let excess = posts.len().saturating_sub(800);
                        posts.drain(..excess);
                        team.posts = posts;
                        *known = team;
                    }
                    None => {
                        let excess = team.posts.len().saturating_sub(800);
                        team.posts.drain(..excess);
                        store.teams.insert(id.clone(), team);
                    }
                }
                store.touch_team(&id);
                store.evict_lru_teams(&id);
                cx.emit(StoreEvent::TeamChanged(id.clone()));
                cx.notify();
            });
        })
        .detach();
    }
}
