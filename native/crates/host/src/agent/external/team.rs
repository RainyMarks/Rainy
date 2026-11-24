//! The teams external agents run: Antigravity swarms (`/swarm`), teamwork (`/teamwork`) and other subagents, read
//! from the conversation folders Antigravity keeps on disk, and Codex sub-agents, gathered from the app-server's
//! notifications. Neither reaches an ACP or app-server client as a team, so a chat shows one only through this.
//!
//! Antigravity keeps, under `~/.gemini/antigravity/brain/<conversation>/.system_generated/` (the App) or
//! `~/.gemini/antigravity-cli/brain/…` (the CLI, same harness):
//! - `subagents/<child>.json`: every agent a conversation started (`typeName`, `role`, `state`, `spawnStepIndex`);
//! - `swarm/members.json`, `swarm/board.jsonl`, `swarm/plan.json`: a swarm's roster, board and plan;
//! - `logs/transcript.jsonl`: the conversation's steps, one JSON object a line.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::hash::{Hash as _, Hasher as _};
use std::io::{Read as _, Seek as _, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use rainy_protocol::chat::{ExternalTeam, TeamActivity, TeamMember, TeamPost, TeamStep, TeamThread};
use serde_json::{Map, Value};

use crate::locale::l;

/// Members read at most, depth first.
const MAX_MEMBERS: usize = 96;
/// Levels below the chat's own agent.
const MAX_DEPTH: u32 = 6;
/// Posts kept in memory and given to a caller without `after`.
const MAX_POSTS: usize = 400;
/// The end of a transcript read for a member's status.
const STATUS_TAIL: u64 = 96 * 1024;
/// The end of a transcript read for a member's recent steps.
const ACTIVITY_TAIL: u64 = 384 * 1024;
/// A member that wrote a step this recently counts as running.
const RECENT_MS: f64 = 20_000.0;
/// Characters of a post or reply kept.
const TEXT_CHARS: usize = 6000;

/// Where Antigravity's App and CLI keep their conversations.
pub fn antigravity_brains() -> Vec<PathBuf> {
    let gemini = crate::env::home_dir().join(".gemini");
    vec![gemini.join("antigravity-cli").join("brain"), gemini.join("antigravity").join("brain")]
}

/// A conversation id safe to join to a path: letters, digits, `-` and `_`.
fn safe_id(id: &str) -> bool {
    (1..=80).contains(&id.len()) && id.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

/// The conversation of the team member an Antigravity tool call over ACP belongs to: a member's call id is
/// `<conversation>:call_…`, a call of the chat's own agent is `call_…` alone.
pub fn member_of_call(call_id: &str) -> Option<&str> {
    let (conversation, call) = call_id.split_once(':')?;
    (conversation.len() >= 8 && safe_id(conversation) && !call.is_empty()).then_some(conversation)
}

fn now_ms() -> f64 {
    SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs_f64() * 1000.0).unwrap_or(0.0)
}

/// `2026-11-23T01:38:56Z` (fractions and offsets allowed) as Unix milliseconds.
pub fn parse_utc(text: &str) -> Option<f64> {
    let bytes = text.as_bytes();
    if bytes.len() < 19 {
        return None;
    }
    let num = |range: std::ops::Range<usize>| text.get(range)?.parse::<i64>().ok();
    let (year, month, day) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (hour, minute, second) = (num(11..13)?, num(14..16)?, num(17..19)?);
    let mut rest = &text[19..];
    let mut fraction = 0.0;
    if let Some(after) = rest.strip_prefix('.') {
        let digits: String = after.chars().take_while(char::is_ascii_digit).collect();
        fraction = format!("0.{digits}").parse::<f64>().unwrap_or(0.0);
        rest = &after[digits.len()..];
    }
    let offset_minutes = match rest.as_bytes().first() {
        Some(b'+' | b'-') if rest.len() >= 6 => {
            let sign = if rest.starts_with('-') { -1 } else { 1 };
            sign * (rest.get(1..3)?.parse::<i64>().ok()? * 60 + rest.get(4..6)?.parse::<i64>().ok()?)
        }
        _ => 0,
    };
    // Days from 1970-01-01 (Howard Hinnant's days_from_civil).
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let seconds = days * 86_400 + hour * 3600 + minute * 60 + second - offset_minutes * 60;
    Some((seconds as f64 + fraction) * 1000.0)
}

fn clip(text: &str, chars: usize) -> String {
    let mut out: String = text.chars().take(chars).collect();
    if text.chars().count() > chars {
        out.push('…');
    }
    out
}

fn one_line(text: &str, chars: usize) -> String {
    clip(&text.split_whitespace().collect::<Vec<_>>().join(" "), chars)
}

/// The names a post mentions (`@parser`, `@all`), in order, without repeats.
pub fn mentions(text: &str) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    let mut chars = text.char_indices().peekable();
    while let Some((index, ch)) = chars.next() {
        if ch != '@' {
            continue;
        }
        // An address inside a word (an e-mail) is no mention.
        if text[..index].chars().next_back().is_some_and(|before| before.is_alphanumeric() || before == '.') {
            continue;
        }
        let name: String = text[index + 1..].chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect();
        if !name.is_empty() && name.len() <= 40 && !found.contains(&name) {
            found.push(name);
        }
    }
    found
}

/// A file's size and change time, to tell whether it changed.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
struct Sig {
    len: u64,
    modified: u128,
}

fn sig(path: &Path) -> Option<Sig> {
    let meta = fs::metadata(path).ok()?;
    let modified = meta.modified().ok()?.duration_since(SystemTime::UNIX_EPOCH).ok()?.as_nanos();
    Some(Sig { len: meta.len(), modified })
}

/// The last `max` bytes of a file as whole lines.
fn tail_lines(path: &Path, max: u64) -> Vec<String> {
    let Ok(mut file) = fs::File::open(path) else { return Vec::new() };
    let len = file.metadata().map(|meta| meta.len()).unwrap_or(0);
    let start = len.saturating_sub(max);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return Vec::new();
    }
    let mut bytes = Vec::new();
    if file.read_to_end(&mut bytes).is_err() {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&bytes);
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    if start > 0 && !lines.is_empty() {
        // The first line was cut.
        lines.remove(0);
    }
    lines
}

fn read_json(path: &Path) -> Option<Value> {
    let bytes = fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// A tool argument as Antigravity logs it: a JSON literal inside a string (`"\"a.rs\""`, `"50"`).
fn arg(args: &Map<String, Value>, key: &str) -> Option<String> {
    let raw = args.get(key)?;
    let text = match raw {
        Value::String(text) => match serde_json::from_str::<Value>(text) {
            Ok(Value::String(inner)) => inner,
            Ok(Value::Null) => return None,
            Ok(other) => other.to_string(),
            Err(_) => text.clone(),
        },
        Value::Null => return None,
        other => other.to_string(),
    };
    let text = text.trim().to_owned();
    (!text.is_empty()).then_some(text)
}

fn file_name(path: &str) -> String {
    path.rsplit(['/', '\\']).find(|part| !part.is_empty()).unwrap_or(path).to_owned()
}

/// One line saying what a logged Antigravity tool call does.
pub fn antigravity_tool_title(name: &str, args: &Map<String, Value>) -> String {
    let path = || {
        ["Path", "AbsolutePath", "TargetFile", "File", "DirectoryPath", "path"].iter().find_map(|key| arg(args, key)).map(|path| file_name(&path)).unwrap_or_default()
    };
    let query = |keys: &[&str]| keys.iter().find_map(|key| arg(args, key)).map(|text| one_line(&text, 80)).unwrap_or_default();
    match name {
        "read_file" | "view_file" => l(&format!("读取 {}", path()), &format!("Read {}", path())),
        "edit_file" | "replace_file_content" | "multi_replace_file_content" => l(&format!("编辑 {}", path()), &format!("Edit {}", path())),
        "write_file" | "write_to_file" => l(&format!("写入 {}", path()), &format!("Write {}", path())),
        "run_command" => arg(args, "toolSummary").or_else(|| arg(args, "CommandLine").map(|command| one_line(&command, 120))).unwrap_or_else(|| l("运行命令", "Run a command")),
        "search_web" => l(&format!("搜索 {}", arg(args, "query").unwrap_or_default()), &format!("Search {}", arg(args, "query").unwrap_or_default())),
        "grep_search" | "codebase_search" => {
            let text = query(&["Query", "query"]);
            l(&format!("搜索 {text}"), &format!("Search {text}"))
        }
        "find_by_name" => {
            let text = query(&["Pattern", "pattern"]);
            l(&format!("查找文件 {text}"), &format!("Find files {text}"))
        }
        "list_dir" => l(&format!("列出 {}", path()), &format!("List {}", path())),
        "view_file_outline" | "view_code_item" => l(&format!("查看 {} 的结构", path()), &format!("Outline {}", path())),
        "read_url_content" | "view_content_chunk" => {
            let text = query(&["Url", "url", "document_id"]);
            l(&format!("读取网页 {text}"), &format!("Read {text}"))
        }
        "command_status" | "read_terminal" => l("查看命令输出", "Check a command's output"),
        "send_command_input" => l("向命令输入", "Type into a command"),
        "generate_image" => l("生成图片", "Generate an image"),
        "read_board" => l("查看看板", "Read the board"),
        "post_message" => {
            let text = one_line(&arg(args, "text").unwrap_or_default(), 80);
            l(&format!("发帖：{text}"), &format!("Post: {text}"))
        }
        "swarm_wait" => l("等待看板上的 @提及", "Wait for a mention on the board"),
        "swarm" => {
            let action = arg(args, "action").or_else(|| arg(args, "Action")).unwrap_or_default();
            let who = arg(args, "name").or_else(|| arg(args, "Name")).map(|name| format!(" @{name}")).unwrap_or_default();
            l(&format!("管理蜂群：{action}{who}"), &format!("Swarm: {action}{who}"))
        }
        "update_focus" => l(&format!("当前重点：{}", one_line(&arg(args, "focus").unwrap_or_default(), 80)), &format!("Focus: {}", one_line(&arg(args, "focus").unwrap_or_default(), 80))),
        "update_steps" => l("更新步骤清单", "Update the checklist"),
        "manage_task" => l("查看后台任务", "Check a background task"),
        "invoke_subagent" => l("派出子 Agent", "Start a subagent"),
        "manage_subagents" => l("管理子 Agent", "Manage subagents"),
        "send_message" => l("发送消息", "Send a message"),
        "webview_browser" | "browser" => l("操作浏览器", "Use the browser"),
        "ask_question" => l("向用户提问", "Ask the user"),
        "exit_plan_mode" => l("结束计划模式", "Leave plan mode"),
        "schedule" => l("安排定时任务", "Schedule a task"),
        other => other.to_owned(),
    }
}

/// The tool calls of a logged model step: an array, or a cut-off string Antigravity left (`truncated_fields`).
fn tool_calls(step: &Value) -> Vec<(String, Map<String, Value>)> {
    match step.get("tool_calls") {
        Some(Value::Array(calls)) => calls
            .iter()
            .filter_map(|call| Some((call.get("name")?.as_str()?.to_owned(), call.get("args").and_then(Value::as_object).cloned().unwrap_or_default())))
            .collect(),
        Some(Value::String(text)) => {
            let mut calls = Vec::new();
            let mut rest = text.as_str();
            while let Some(at) = rest.find("\"name\"").or_else(|| rest.find("'name'")) {
                rest = &rest[at + 6..];
                let start = rest.find(['"', '\'']).map(|i| i + 1).unwrap_or(0);
                let name: String = rest[start..].chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
                if !name.is_empty() {
                    calls.push((name, Map::new()));
                }
            }
            calls
        }
        _ => Vec::new(),
    }
}

/// What a member's transcript says about it now.
#[derive(Clone, Default, Debug)]
struct Tail {
    running: bool,
    waiting: bool,
    error: bool,
    activity: Option<String>,
    active_at: Option<f64>,
    focus: Option<String>,
    steps: Vec<TeamStep>,
    model: Option<String>,
}

fn read_tail(path: &Path, previous: Option<&Tail>) -> Tail {
    let mut tail = Tail::default();
    let lines = tail_lines(path, STATUS_TAIL);
    let steps: Vec<Value> = lines.iter().filter_map(|line| serde_json::from_str(line).ok()).collect();
    for step in &steps {
        if let Some(model) = step.get("model").and_then(Value::as_str).filter(|m| !m.is_empty()) {
            tail.model = Some(model.to_owned());
        }
        for (name, args) in tool_calls(step) {
            match name.as_str() {
                "update_focus" => tail.focus = arg(&args, "focus").map(|focus| one_line(&focus, 160)),
                "update_steps" => {
                    if let Some(list) = arg(&args, "steps").and_then(|text| serde_json::from_str::<Vec<Value>>(&text).ok()) {
                        tail.steps = list
                            .iter()
                            .filter_map(|item| {
                                Some(TeamStep {
                                    title: one_line(item.get("title")?.as_str()?, 120),
                                    status: item.get("status").and_then(Value::as_str).unwrap_or("pending").to_owned(),
                                })
                            })
                            .take(40)
                            .collect();
                    }
                }
                _ => {}
            }
        }
    }
    if let Some(previous) = previous {
        // The checklist and focus may lie before the part read.
        if tail.focus.is_none() {
            tail.focus = previous.focus.clone();
        }
        if tail.steps.is_empty() {
            tail.steps = previous.steps.clone();
        }
        if tail.model.is_none() {
            tail.model = previous.model.clone();
        }
    }
    let Some(last) = steps.last() else { return tail };
    tail.active_at = last.get("created_at").and_then(Value::as_str).and_then(parse_utc);
    let status = last.get("status").and_then(Value::as_str).unwrap_or_default();
    let kind = last.get("type").and_then(Value::as_str).unwrap_or_default();
    tail.error = status == "ERROR" || kind == "ERROR_MESSAGE";
    tail.running = status == "RUNNING" || tail.active_at.is_some_and(|at| now_ms() - at < RECENT_MS);
    // The latest call it made, and whether that leaves it asleep on the board.
    if let Some((name, args)) = steps.iter().rev().find_map(|step| tool_calls(step).into_iter().last()) {
        tail.waiting = name == "swarm_wait" && status != "RUNNING";
        tail.activity = Some(antigravity_tool_title(&name, &args));
    } else if let Some(text) = steps.iter().rev().find_map(|step| step.get("content").and_then(Value::as_str).filter(|_| step.get("type").and_then(Value::as_str) == Some("PLANNER_RESPONSE"))) {
        tail.activity = Some(one_line(text, 120));
    }
    if tail.waiting {
        tail.running = false;
    }
    tail
}

/// What a `subagents/<child>.json` entry says.
#[derive(Clone, Debug)]
struct Spawned {
    id: String,
    type_name: String,
    role: String,
    alive: bool,
    order: i64,
}

fn spawned_of(brain: &Path, conversation: &str) -> Vec<Spawned> {
    let dir = brain.join(conversation).join(".system_generated").join("subagents");
    let Ok(entries) = fs::read_dir(&dir) else { return Vec::new() };
    let mut found: Vec<Spawned> = entries
        .flatten()
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
        .filter_map(|entry| {
            let value = read_json(&entry.path())?;
            let id = value.get("conversationId")?.as_str()?.to_owned();
            if !safe_id(&id) {
                return None;
            }
            let descriptor = value.get("subagentDescriptor");
            Some(Spawned {
                id,
                type_name: descriptor.and_then(|d| d.get("typeName")).and_then(Value::as_str).unwrap_or("subagent").to_owned(),
                role: descriptor.and_then(|d| d.get("role")).and_then(Value::as_str).unwrap_or_default().to_owned(),
                alive: value.get("state").and_then(Value::as_str) != Some("SUBAGENT_STATE_KILLED"),
                order: value.get("spawnStepIndex").and_then(Value::as_i64).unwrap_or(i64::MAX),
            })
        })
        .collect();
    found.sort_by(|a, b| a.order.cmp(&b.order).then_with(|| a.id.cmp(&b.id)));
    found
}

/// A swarm member's entry in `members.json`.
#[derive(Clone, Debug, Default)]
struct Rostered {
    name: String,
    killed: bool,
    model: Option<String>,
}

/// `kind` and handle for an Antigravity subagent.
fn kind_and_name(spawned: &Spawned, rostered: Option<&Rostered>) -> (String, String) {
    let worker_name = spawned.role.strip_prefix("Swarm Worker (").and_then(|rest| rest.strip_suffix(')')).map(str::to_owned);
    let name = rostered.map(|r| r.name.clone()).or(worker_name).unwrap_or_else(|| if spawned.role.trim().is_empty() { spawned.type_name.clone() } else { spawned.role.trim().to_owned() });
    let kind = match spawned.type_name.as_str() {
        "worker" if name.contains("verifier") => "verifier",
        "worker" if name == "monitor" => "monitor",
        "worker" => "worker",
        "teamwork" => "teamwork",
        "teamwork_orchestrator" => "orchestrator",
        "teamwork_explorer" => "explorer",
        "teamwork_challenger" => "challenger",
        "teamwork_interviewer" => "interviewer",
        other if other.contains("auditor") => "auditor",
        "browser" => "browser",
        _ => "subagent",
    };
    (kind.to_owned(), name)
}

/// The Antigravity team of one chat, read again as its files change.
pub struct AntigravityTeam {
    session_id: String,
    brain: PathBuf,
    board_sig: Option<Sig>,
    board_read: u64,
    posts: Vec<TeamPost>,
    tails: HashMap<String, (Sig, Tail)>,
    team: ExternalTeam,
    fingerprint: u64,
}

impl AntigravityTeam {
    /// The team of the Antigravity conversation `session_id` (the ACP session id), when one of Antigravity's folders
    /// holds that conversation.
    pub fn locate(session_id: &str) -> Option<Self> {
        if !safe_id(session_id) {
            return None;
        }
        let brain = antigravity_brains().into_iter().find(|brain| brain.join(session_id).join(".system_generated").is_dir())?;
        Some(Self {
            session_id: session_id.to_owned(),
            brain,
            board_sig: None,
            board_read: 0,
            posts: Vec::new(),
            tails: HashMap::new(),
            team: ExternalTeam { session_id: session_id.to_owned(), source: "antigravity".into(), ..ExternalTeam::default() },
            fingerprint: 0,
        })
    }

    #[cfg(test)]
    fn at(brain: &Path, session_id: &str) -> Self {
        Self {
            session_id: session_id.to_owned(),
            brain: brain.to_owned(),
            board_sig: None,
            board_read: 0,
            posts: Vec::new(),
            tails: HashMap::new(),
            team: ExternalTeam { session_id: session_id.to_owned(), source: "antigravity".into(), ..ExternalTeam::default() },
            fingerprint: 0,
        }
    }

    fn generated(&self, conversation: &str) -> PathBuf {
        self.brain.join(conversation).join(".system_generated")
    }

    fn tail(&mut self, conversation: &str) -> Tail {
        let path = self.generated(conversation).join("logs").join("transcript.jsonl");
        let Some(now) = sig(&path) else { return Tail::default() };
        match self.tails.get(conversation) {
            Some((was, tail)) if *was == now && !tail.running => tail.clone(),
            previous => {
                let tail = read_tail(&path, previous.map(|(_, tail)| tail));
                self.tails.insert(conversation.to_owned(), (now, tail.clone()));
                tail
            }
        }
    }

    /// New posts appended to the board since the last read.
    fn read_board(&mut self, swarm: &Path) {
        let path = swarm.join("board.jsonl");
        let Some(now) = sig(&path) else { return };
        if self.board_sig == Some(now) {
            return;
        }
        if now.len < self.board_read {
            // Rewritten: read it again.
            self.board_read = 0;
            self.posts.clear();
        }
        self.board_sig = Some(now);
        let Ok(mut file) = fs::File::open(&path) else { return };
        if file.seek(SeekFrom::Start(self.board_read)).is_err() {
            return;
        }
        let mut bytes = Vec::new();
        if file.read_to_end(&mut bytes).is_err() {
            return;
        }
        // Only whole lines; a line still being written is read next time.
        let Some(end) = bytes.iter().rposition(|byte| *byte == b'\n') else { return };
        self.board_read += end as u64 + 1;
        for line in String::from_utf8_lossy(&bytes[..=end]).lines() {
            let Ok(post) = serde_json::from_str::<Value>(line) else { continue };
            let Some(seq) = post.get("seq").and_then(Value::as_u64) else { continue };
            let text = post.get("text").and_then(Value::as_str).unwrap_or_default();
            let from = post.get("agent").or_else(|| post.get("from")).and_then(Value::as_str).unwrap_or("?");
            let time = post.get("time").and_then(Value::as_str).map(str::to_owned);
            // The coordinator is the chat's own agent, as the author of a post and as the one it mentions.
            let lead = |name: &str| if name == "coordinator" { "lead".to_owned() } else { name.to_owned() };
            self.posts.push(TeamPost {
                seq,
                from: lead(from),
                to: mentions(text).iter().map(|name| lead(name)).collect(),
                text: clip(text, TEXT_CHARS),
                // `time` is local wall-clock time, no instant; Unix milliseconds only when the post carries them.
                ts: post.get("ts").and_then(Value::as_f64).unwrap_or(0.0),
                time,
            });
        }
        let excess = self.posts.len().saturating_sub(MAX_POSTS);
        self.posts.drain(..excess);
    }

    /// Read the team again; whether anything changed since the last time.
    pub fn refresh(&mut self) -> bool {
        let root = self.session_id.clone();
        let swarm = self.generated(&root).join("swarm");
        let mut roster: HashMap<String, Rostered> = HashMap::new();
        let mut title = None;
        let mut goal = None;
        let mut tasks: HashMap<String, String> = HashMap::new();
        let mut threads = Vec::new();
        let mut lead_model = None;
        if let Some(members) = read_json(&swarm.join("members.json")) {
            title = members.get("title").and_then(Value::as_str).map(str::to_owned);
            if let Some(map) = members.get("members").and_then(Value::as_object) {
                for (name, entry) in map {
                    let Some(id) = entry.get("conversation_id").and_then(Value::as_str) else { continue };
                    let model = entry.get("applied_model").or_else(|| entry.get("model")).and_then(Value::as_str).map(str::to_owned);
                    if entry.get("role").and_then(Value::as_str) == Some("coordinator") || id == root {
                        lead_model = model;
                        continue;
                    }
                    let killed = entry.get("killed").and_then(Value::as_bool) == Some(true) || entry.get("phase").and_then(Value::as_str) == Some("KILLED");
                    roster.insert(id.to_owned(), Rostered { name: name.clone(), killed, model });
                }
            }
            self.read_board(&swarm);
        }
        if let Some(plan) = read_json(&swarm.join("plan.json")) {
            if let Some(plan_goal) = plan.get("goal") {
                title = plan_goal.get("title").and_then(Value::as_str).map(str::to_owned).or(title);
                goal = plan_goal.get("line").and_then(Value::as_str).map(str::to_owned);
            }
            if let Some(map) = plan.get("tasks").and_then(Value::as_object) {
                tasks = map.iter().filter_map(|(name, text)| Some((name.clone(), text.as_str()?.to_owned()))).collect();
            }
            threads = plan
                .get("threads")
                .and_then(Value::as_array)
                .map(|list| {
                    list.iter()
                        .filter_map(|thread| {
                            Some(TeamThread {
                                id: thread.get("id")?.as_str()?.to_owned(),
                                title: thread.get("title").and_then(Value::as_str).unwrap_or_default().to_owned(),
                                goal: thread.get("goal").and_then(Value::as_str).map(str::to_owned),
                                members: thread.get("tasks").and_then(Value::as_array).map(|tasks| tasks.iter().filter_map(|task| task.as_str().map(str::to_owned)).collect()).unwrap_or_default(),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
        }

        // The chat's own agent, then everything it started, depth first.
        let lead_tail = self.tail(&root);
        let mut members = vec![TeamMember {
            id: root.clone(),
            parent: None,
            name: "lead".into(),
            kind: "lead".into(),
            status: status_of(true, false, &lead_tail).into(),
            model: lead_model.or(lead_tail.model.clone()),
            focus: lead_tail.focus.clone(),
            activity: lead_tail.activity.clone(),
            active_at: lead_tail.active_at,
            steps: lead_tail.steps.clone(),
            ..TeamMember::default()
        }];
        let mut seen: HashSet<String> = HashSet::from([root.clone()]);
        // Depth first in the order they were started: each entry with its parent and depth.
        let mut stack: Vec<(Spawned, String, u32)> = spawned_of(&self.brain, &root).into_iter().rev().map(|spawned| (spawned, root.clone(), 1)).collect();
        while let Some((spawned, parent, depth)) = stack.pop() {
            if members.len() >= MAX_MEMBERS {
                break;
            }
            let id = spawned.id.clone();
            if !seen.insert(id.clone()) {
                continue;
            }
            let rostered = roster.get(&id);
            let (kind, name) = kind_and_name(&spawned, rostered);
            let tail = self.tail(&id);
            let alive = spawned.alive && !rostered.is_some_and(|r| r.killed);
            members.push(TeamMember {
                id: id.clone(),
                parent: Some(parent),
                task: tasks.get(&name).cloned(),
                role: (!spawned.role.is_empty() && !spawned.role.starts_with("Swarm Worker (")).then(|| spawned.role.clone()),
                status: status_of(alive, false, &tail).into(),
                model: rostered.and_then(|r| r.model.clone()).or(tail.model.clone()),
                focus: tail.focus.clone(),
                activity: tail.activity.clone(),
                active_at: tail.active_at,
                steps: tail.steps.clone(),
                depth,
                kind,
                name,
            });
            if depth < MAX_DEPTH {
                stack.extend(spawned_of(&self.brain, &id).into_iter().rev().map(|child| (child, id.clone(), depth + 1)));
            }
        }

        let last_post = self.posts.last().map(|post| post.seq).unwrap_or(0);
        let mut team = ExternalTeam {
            session_id: self.session_id.clone(),
            source: "antigravity".into(),
            title,
            goal,
            members,
            posts: Vec::new(),
            last_post,
            threads,
            revision: 0,
        };
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        serde_json::to_string(&team).unwrap_or_default().hash(&mut hasher);
        let fingerprint = hasher.finish();
        let changed = fingerprint != self.fingerprint;
        team.revision = self.team.revision + u64::from(changed);
        self.fingerprint = fingerprint;
        self.team = team;
        changed
    }

    /// The name of the member whose conversation is `conversation`, the team read again when it is not known yet.
    pub fn member_name(&mut self, conversation: &str) -> Option<String> {
        let find = |team: &ExternalTeam| team.members.iter().skip(1).find(|member| member.id == conversation).map(|member| member.name.clone());
        if let Some(name) = find(&self.team) {
            return Some(name);
        }
        self.refresh();
        find(&self.team)
    }

    /// The team's revision and newest post number.
    pub fn marks(&self) -> (u64, u64) {
        (self.team.revision, self.team.last_post)
    }

    /// Whether any member other than the chat's own agent is still at work.
    pub fn busy(&self) -> bool {
        self.team.members.iter().skip(1).any(|member| member.status == "running")
    }

    /// Whether the conversation started any other agent.
    pub fn has_members(&self) -> bool {
        self.team.members.len() > 1 || !self.posts.is_empty()
    }

    /// The team with the posts newer than `after` (the newest `MAX_POSTS` without it).
    pub fn snapshot(&self, after: Option<u64>) -> ExternalTeam {
        let mut team = self.team.clone();
        team.posts = match after {
            Some(after) => self.posts.iter().filter(|post| post.seq > after).cloned().collect(),
            None => self.posts.clone(),
        };
        team
    }

    /// The latest `limit` steps of a member of this team.
    pub fn activity(&self, member: &str, limit: usize) -> Option<Vec<TeamActivity>> {
        if !self.team.members.iter().any(|known| known.id == member) || !safe_id(member) {
            return None;
        }
        let path = self.generated(member).join("logs").join("transcript.jsonl");
        Some(activity_from(&tail_lines(&path, ACTIVITY_TAIL), limit))
    }
}

fn status_of(alive: bool, done: bool, tail: &Tail) -> &'static str {
    if !alive {
        "killed"
    } else if done {
        "done"
    } else if tail.error {
        "error"
    } else if tail.running {
        "running"
    } else if tail.waiting {
        "waiting"
    } else {
        "idle"
    }
}

/// The steps of an Antigravity transcript, newest last, as activity rows.
fn activity_from(lines: &[String], limit: usize) -> Vec<TeamActivity> {
    let mut rows = Vec::new();
    for line in lines {
        let Ok(step) = serde_json::from_str::<Value>(line) else { continue };
        let ts = step.get("created_at").and_then(Value::as_str).and_then(parse_utc).unwrap_or(0.0);
        let kind = step.get("type").and_then(Value::as_str).unwrap_or_default();
        let content = step.get("content").and_then(Value::as_str).unwrap_or_default();
        match kind {
            "PLANNER_RESPONSE" => {
                if !content.trim().is_empty() {
                    rows.push(TeamActivity { ts, kind: "reply".into(), title: one_line(content, 160), detail: Some(clip(content, TEXT_CHARS)) });
                }
                for (name, args) in tool_calls(&step) {
                    let detail = match name.as_str() {
                        "run_command" => arg(&args, "CommandLine"),
                        "post_message" => arg(&args, "text"),
                        "update_steps" | "update_focus" => None,
                        _ => arg(&args, "Path").or_else(|| arg(&args, "query")),
                    };
                    rows.push(TeamActivity { ts, kind: "tool".into(), title: antigravity_tool_title(&name, &args), detail: detail.map(|text| clip(&text, TEXT_CHARS)) });
                }
            }
            "SYSTEM_MESSAGE" | "USER_INPUT" => {
                // `… content=<the message>` inside a system notice; a user's own input as is.
                let message = content.split_once("content=").map(|(_, rest)| rest.trim_end_matches("</SYSTEM_MESSAGE>").trim()).unwrap_or(content);
                if !message.is_empty() {
                    rows.push(TeamActivity { ts, kind: "message".into(), title: one_line(message, 160), detail: Some(clip(message, TEXT_CHARS)) });
                }
            }
            "ERROR_MESSAGE" => {
                let text = step.get("error").and_then(Value::as_str).unwrap_or(content);
                rows.push(TeamActivity { ts, kind: "error".into(), title: one_line(text, 160), detail: Some(clip(text, TEXT_CHARS)) });
            }
            _ if step.get("status").and_then(Value::as_str) == Some("ERROR") => {
                rows.push(TeamActivity { ts, kind: "error".into(), title: one_line(content, 160), detail: Some(clip(content, TEXT_CHARS)) });
            }
            _ => {}
        }
    }
    let skip = rows.len().saturating_sub(limit);
    rows.split_off(skip)
}

// ── Codex sub-agents ──

/// One Codex sub-agent as its notifications describe it.
#[derive(Clone, Debug, Default)]
struct CodexAgent {
    id: String,
    parent: String,
    path: String,
    model: Option<String>,
    status: String,
    activity: Option<String>,
    active_at: Option<f64>,
    /// Its answer, streamed and then whole.
    reply: String,
    log: Vec<TeamActivity>,
    order: usize,
}

/// The Codex sub-agents of one main thread, kept across its turns.
#[derive(Clone, Debug, Default)]
pub struct CodexTeam {
    main: String,
    agents: HashMap<String, CodexAgent>,
    posts: Vec<TeamPost>,
    revision: u64,
}

/// The name of a Codex sub-agent: the last part of its path (`/root/alpha` → `alpha`).
pub fn codex_agent_name(path: &str, id: &str) -> String {
    path.rsplit('/').find(|part| !part.is_empty()).map(str::to_owned).unwrap_or_else(|| id.chars().rev().take(6).collect::<Vec<_>>().into_iter().rev().collect())
}

impl CodexTeam {
    pub fn new(main: &str) -> Self {
        Self { main: main.to_owned(), ..Self::default() }
    }

    /// Whether `thread` is one of this team's sub-agents.
    pub fn knows(&self, thread: &str) -> bool {
        self.agents.contains_key(thread)
    }

    pub fn is_empty(&self) -> bool {
        self.agents.is_empty()
    }

    /// The name of a sub-agent.
    pub fn name_of(&self, thread: &str) -> Option<String> {
        self.agents.get(thread).map(|agent| codex_agent_name(&agent.path, &agent.id))
    }

    /// Whether a sub-agent is through (done, failed or stopped): `Some(failed)`.
    pub fn finished(&self, thread: &str) -> Option<bool> {
        self.agents.get(thread).and_then(|agent| match agent.status.as_str() {
            "done" => Some(false),
            "error" | "killed" => Some(true),
            _ => None,
        })
    }

    fn post(&mut self, from: String, to: Vec<String>, text: &str) {
        let seq = self.posts.last().map(|post| post.seq).unwrap_or(0) + 1;
        self.posts.push(TeamPost { seq, from, to, text: clip(text, TEXT_CHARS), ts: now_ms(), time: None });
        let excess = self.posts.len().saturating_sub(MAX_POSTS);
        self.posts.drain(..excess);
    }

    fn handle(&self, thread: &str) -> String {
        if thread == self.main { "lead".into() } else { self.name_of(thread).unwrap_or_else(|| "?".into()) }
    }

    /// A `subAgentActivity` item seen on `sender`'s thread; returns the sub-agent's id when it just started.
    pub fn sub_agent_activity(&mut self, sender: &str, item: &Value) -> Option<String> {
        let id = item.get("agentThreadId").and_then(Value::as_str)?.to_owned();
        let kind = item.get("kind").and_then(Value::as_str).unwrap_or_default();
        let path = item.get("agentPath").and_then(Value::as_str).unwrap_or_default().to_owned();
        let model = item.get("model").and_then(Value::as_str).map(str::to_owned);
        let started = !self.agents.contains_key(&id);
        let order = self.agents.len();
        let agent = self.agents.entry(id.clone()).or_insert_with(|| CodexAgent { id: id.clone(), parent: sender.to_owned(), path: path.clone(), status: "running".into(), order, ..CodexAgent::default() });
        if agent.path.is_empty() {
            agent.path = path;
        }
        if model.is_some() {
            agent.model = model;
        }
        agent.status = match kind {
            "completed" => "done",
            "interrupted" => "killed",
            _ => "running",
        }
        .into();
        agent.active_at = Some(now_ms());
        self.revision += 1;
        if kind == "completed" {
            let reply = agent.reply.trim().to_owned();
            let name = codex_agent_name(&agent.path, &agent.id);
            let to = self.handle(sender);
            if !reply.is_empty() {
                self.post(name, vec![to], &reply);
            }
        }
        started.then_some(id)
    }

    /// A `collabAgentToolCall` item of `sender`: messages to sub-agents go on the board.
    pub fn collab_call(&mut self, sender: &str, item: &Value) {
        let tool = item.get("tool").and_then(Value::as_str).unwrap_or_default();
        let receivers: Vec<String> = item.get("receiverThreadIds").and_then(Value::as_array).map(|ids| ids.iter().filter_map(|id| id.as_str().map(str::to_owned)).collect()).unwrap_or_default();
        if let Some(states) = item.get("agentsStates").and_then(Value::as_object) {
            for (id, state) in states {
                if let Some(agent) = self.agents.get_mut(id)
                    && let Some(status) = state.get("status").and_then(Value::as_str)
                {
                    agent.status = match status {
                        "completed" | "shutdown" => "done",
                        "errored" | "notFound" => "error",
                        "interrupted" => "killed",
                        "running" | "pendingInit" => "running",
                        _ => "idle",
                    }
                    .into();
                    self.revision += 1;
                }
            }
        }
        let Some(prompt) = item.get("prompt").and_then(Value::as_str).filter(|prompt| !prompt.trim().is_empty()) else { return };
        let in_progress = item.get("status").and_then(Value::as_str) == Some("inProgress");
        // Each message goes on the board once: a spawn's when it is done (only then is the new agent known), any
        // other when it starts.
        if if tool == "spawnAgent" { !in_progress && !receivers.is_empty() } else { in_progress } {
            let from = self.handle(sender);
            let to = receivers.iter().map(|id| self.handle(id)).collect();
            self.post(from, to, prompt);
            self.revision += 1;
        }
    }

    /// A notification of a sub-agent's own thread; a progress line for its card when it says something new.
    pub fn agent_event(&mut self, thread: &str, method: &str, params: &Value) -> Option<String> {
        let agent = self.agents.get_mut(thread)?;
        let now = now_ms();
        let mut changed = true;
        match method {
            "thread/status/changed" => {
                let kind = params.pointer("/status/type").and_then(Value::as_str).unwrap_or_default();
                if agent.status != "done" && agent.status != "killed" {
                    agent.status = if kind == "active" { "running" } else { "idle" }.into();
                }
            }
            "turn/started" => agent.status = "running".into(),
            "item/started" => {
                let item = params.get("item")?;
                let title = codex_item_title(item)?;
                agent.activity = Some(title.clone());
                agent.log.push(TeamActivity { ts: now, kind: "tool".into(), title, detail: None });
            }
            "item/agentMessage/delta" => {
                agent.reply.push_str(params.get("delta").and_then(Value::as_str).unwrap_or_default());
                changed = false;
            }
            "item/completed" => {
                let item = params.get("item")?;
                if item.get("type").and_then(Value::as_str) == Some("agentMessage") {
                    let text = item.get("text").and_then(Value::as_str).unwrap_or_default().to_owned();
                    if !text.trim().is_empty() {
                        agent.reply = text.clone();
                        agent.log.push(TeamActivity { ts: now, kind: "reply".into(), title: one_line(&text, 160), detail: Some(clip(&text, TEXT_CHARS)) });
                    }
                } else {
                    changed = false;
                }
            }
            _ => changed = false,
        }
        let excess = agent.log.len().saturating_sub(200);
        agent.log.drain(..excess);
        agent.active_at = Some(now);
        if changed {
            self.revision += 1;
        }
        // The card shows its last few actions and what it is writing.
        let mut lines: Vec<String> = agent.log.iter().rev().filter(|row| row.kind == "tool").take(6).map(|row| format!("• {}", row.title)).collect();
        lines.reverse();
        if !agent.reply.trim().is_empty() {
            lines.push(String::new());
            lines.push(clip(agent.reply.trim(), 4000));
        }
        Some(lines.join("\n"))
    }

    /// The answer a sub-agent gave.
    pub fn reply_of(&self, thread: &str) -> Option<String> {
        self.agents.get(thread).map(|agent| agent.reply.trim().to_owned()).filter(|reply| !reply.is_empty())
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn snapshot(&self, session_id: &str, lead_status: &str) -> ExternalTeam {
        let mut agents: Vec<&CodexAgent> = self.agents.values().collect();
        agents.sort_by_key(|agent| agent.order);
        let depth_of = |agent: &CodexAgent| agent.path.split('/').filter(|part| !part.is_empty()).count().saturating_sub(1).max(1) as u32;
        let mut members = vec![TeamMember { id: self.main.clone(), name: "lead".into(), kind: "lead".into(), status: lead_status.into(), ..TeamMember::default() }];
        // Depth first: each agent after its parent.
        let mut ordered: Vec<&CodexAgent> = Vec::new();
        fn place<'a>(parent: &str, agents: &[&'a CodexAgent], out: &mut Vec<&'a CodexAgent>) {
            for agent in agents.iter().filter(|agent| agent.parent == parent) {
                if out.iter().any(|known| known.id == agent.id) {
                    continue;
                }
                out.push(agent);
                place(&agent.id, agents, out);
            }
        }
        place(&self.main, &agents, &mut ordered);
        // Agents whose parent is not known (started before Rainy saw it) come last.
        let orphans: Vec<&CodexAgent> = agents.iter().filter(|agent| !ordered.iter().any(|known| known.id == agent.id)).copied().collect();
        ordered.extend(orphans);
        for agent in ordered {
            members.push(TeamMember {
                id: agent.id.clone(),
                parent: Some(agent.parent.clone()),
                name: codex_agent_name(&agent.path, &agent.id),
                kind: "subagent".into(),
                status: agent.status.clone(),
                model: agent.model.clone(),
                activity: agent.activity.clone(),
                active_at: agent.active_at,
                depth: depth_of(agent),
                ..TeamMember::default()
            });
        }
        ExternalTeam {
            session_id: session_id.to_owned(),
            source: "codex".into(),
            members,
            last_post: self.posts.last().map(|post| post.seq).unwrap_or(0),
            posts: self.posts.clone(),
            revision: self.revision,
            ..ExternalTeam::default()
        }
    }

    /// The latest `limit` steps of a sub-agent.
    pub fn activity(&self, thread: &str, limit: usize) -> Option<Vec<TeamActivity>> {
        let agent = self.agents.get(thread)?;
        let skip = agent.log.len().saturating_sub(limit);
        Some(agent.log[skip..].to_vec())
    }
}

/// One line for a Codex item a sub-agent started.
pub fn codex_item_title(item: &Value) -> Option<String> {
    match item.get("type").and_then(Value::as_str)? {
        "commandExecution" => Some(one_line(item.get("command").and_then(Value::as_str).unwrap_or_default(), 140)),
        "fileChange" => {
            let paths: Vec<String> = item.get("changes").and_then(Value::as_array).map(|changes| changes.iter().filter_map(|c| c.get("path").and_then(Value::as_str).map(file_name)).collect()).unwrap_or_default();
            Some(l(&format!("修改 {}", paths.join("、")), &format!("Edit {}", paths.join(", "))))
        }
        "mcpToolCall" => Some(format!("{}: {}", item.get("server").and_then(Value::as_str).unwrap_or("mcp"), item.get("tool").and_then(Value::as_str).unwrap_or("call"))),
        "webSearch" => Some(l(&format!("搜索 {}", item.get("query").and_then(Value::as_str).unwrap_or_default()), &format!("Search {}", item.get("query").and_then(Value::as_str).unwrap_or_default()))),
        "collabAgentToolCall" => Some(l("协调子 Agent", "Coordinate sub-agents")),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_utc_times_and_mentions() {
        assert_eq!(parse_utc("1970-01-01T00:00:01Z"), Some(1000.0));
        assert_eq!(parse_utc("2026-11-23T01:38:56Z"), Some(1_795_397_936_000.0));
        assert_eq!(parse_utc("2026-11-22T17:38:56-08:00"), Some(1_795_397_936_000.0));
        assert_eq!(parse_utc("2026-11-23T01:38:56.5Z"), Some(1_795_397_936_500.0));
        assert_eq!(parse_utc("soon"), None);
        assert_eq!(mentions("@parser and @api-tests, mail a@b.c, again @parser; @all"), vec!["parser", "api-tests", "all"]);
    }

    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn step(index: u32, kind: &str, status: &str, at: &str, extra: Value) -> String {
        let mut value = json!({ "step_index": index, "source": "MODEL", "type": kind, "status": status, "created_at": at });
        value.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        format!("{value}\n")
    }

    #[test]
    fn reads_an_antigravity_swarm_with_teamwork_below_it() {
        let dir = tempfile::tempdir().unwrap();
        let brain = dir.path();
        let lead = "lead-0001";
        let generated = |id: &str| brain.join(id).join(".system_generated");
        // Two swarm workers (one removed) and a teamwork run with an orchestrator that started an explorer.
        write(&generated(lead).join("subagents/w1.json"), r#"{"conversationId":"w1","subagentDescriptor":{"typeName":"worker","role":"Swarm Worker (parser)"},"state":"SUBAGENT_STATE_ALIVE","spawnStepIndex":10}"#);
        write(&generated(lead).join("subagents/w2.json"), r#"{"conversationId":"w2","subagentDescriptor":{"typeName":"worker","role":"Swarm Worker (verifier)"},"state":"SUBAGENT_STATE_KILLED","spawnStepIndex":11}"#);
        write(&generated(lead).join("subagents/t1.json"), r#"{"conversationId":"t1","subagentDescriptor":{"typeName":"teamwork","role":"Docs team"},"state":"SUBAGENT_STATE_ALIVE","spawnStepIndex":20}"#);
        write(&generated("t1").join("subagents/o1.json"), r#"{"conversationId":"o1","subagentDescriptor":{"typeName":"teamwork_orchestrator","role":"Project Orchestrator"},"state":"SUBAGENT_STATE_ALIVE","spawnStepIndex":3}"#);
        write(&generated("o1").join("subagents/e1.json"), r#"{"conversationId":"e1","subagentDescriptor":{"typeName":"teamwork_explorer","role":"Host Explorer"},"state":"SUBAGENT_STATE_KILLED","spawnStepIndex":4}"#);
        write(
            &generated(lead).join("swarm/members.json"),
            r#"{"title":"Port the parser","members":{"coordinator":{"conversation_id":"lead-0001","role":"coordinator","applied_model":"gemini-x"},"parser":{"conversation_id":"w1","role":"worker","phase":"RUNNING","applied_model":"gemini-x"},"verifier":{"conversation_id":"w2","role":"worker","phase":"KILLED","killed":true}}}"#,
        );
        write(&generated(lead).join("swarm/plan.json"), r#"{"goal":{"title":"Parser","line":"Port it."},"tasks":{"parser":"Write the parser","verifier":"Check it"},"threads":[{"id":"t-a","title":"Core","goal":"Parse","tasks":["parser","verifier"]}]}"#);
        write(&generated(lead).join("swarm/board.jsonl"), "{\"seq\":1,\"agent\":\"coordinator\",\"text\":\"@parser start\",\"time\":\"2026-11-23 10:00:00\"}\n{\"seq\":2,\"agent\":\"parser\",\"text\":\"done @coordinator\",\"time\":\"2026-11-23 10:05:00\"}\n");
        let calls = json!({ "tool_calls": [
            { "name": "update_focus", "args": { "focus": "\"Lexer first\"" } },
            { "name": "update_steps", "args": { "steps": "\"[{\\\"status\\\":\\\"completed\\\",\\\"title\\\":\\\"Read\\\"},{\\\"status\\\":\\\"in_progress\\\",\\\"title\\\":\\\"Write\\\"}]\"" } },
            { "name": "read_file", "args": { "Path": "\"C:\\\\repo\\\\src\\\\lex.rs\"" } }
        ], "model": "gemini-x" });
        write(&generated("w1").join("logs/transcript.jsonl"), &(step(0, "SYSTEM_MESSAGE", "DONE", "2026-11-23T01:00:00Z", json!({ "content": "x content=You have just started." })) + &step(1, "PLANNER_RESPONSE", "DONE", "2026-11-23T01:00:05Z", calls)));
        let wait = json!({ "tool_calls": [{ "name": "swarm_wait", "args": { "since": "2" } }] });
        write(&generated("t1").join("logs/transcript.jsonl"), &step(0, "PLANNER_RESPONSE", "DONE", "2026-11-23T01:00:05Z", wait));

        let mut team = AntigravityTeam::at(brain, lead);
        assert!(team.refresh());
        assert!(!team.refresh(), "nothing changed");
        let snapshot = team.snapshot(None);
        assert_eq!(snapshot.title.as_deref(), Some("Parser"));
        assert_eq!(snapshot.goal.as_deref(), Some("Port it."));
        assert_eq!(snapshot.threads[0].members, vec!["parser", "verifier"]);
        let names: Vec<(&str, &str, u32, &str)> = snapshot.members.iter().map(|m| (m.name.as_str(), m.kind.as_str(), m.depth, m.status.as_str())).collect();
        assert_eq!(
            names,
            vec![
                ("lead", "lead", 0, "idle"),
                ("parser", "worker", 1, "idle"),
                ("verifier", "verifier", 1, "killed"),
                ("Docs team", "teamwork", 1, "waiting"),
                ("Project Orchestrator", "orchestrator", 2, "idle"),
                ("Host Explorer", "explorer", 3, "killed"),
            ]
        );
        let parser = &snapshot.members[1];
        assert_eq!(parser.task.as_deref(), Some("Write the parser"));
        assert_eq!(parser.model.as_deref(), Some("gemini-x"));
        assert_eq!(parser.focus.as_deref(), Some("Lexer first"));
        assert_eq!(parser.activity.as_deref(), Some(l("读取 lex.rs", "Read lex.rs").as_str()));
        assert_eq!(parser.steps.len(), 2);
        assert_eq!(snapshot.members[4].parent.as_deref(), Some("t1"));
        assert_eq!(snapshot.posts.len(), 2);
        assert_eq!(snapshot.posts[0].from, "lead");
        assert_eq!(snapshot.posts[0].to, vec!["parser"]);
        assert_eq!(snapshot.posts[1].time.as_deref(), Some("2026-11-23 10:05:00"));
        assert_eq!(snapshot.posts[1].to, vec!["lead"], "the coordinator mentioned is the lead");
        assert_eq!(snapshot.posts[1].ts, 0.0, "a local time is no instant");

        // A new post is read on its own; only newer posts go to a caller that has the rest.
        fs::OpenOptions::new().append(true).open(generated(lead).join("swarm/board.jsonl")).unwrap().write_all(b"{\"seq\":3,\"agent\":\"parser\",\"text\":\"@all more\"}\n").unwrap();
        assert!(team.refresh());
        let newer = team.snapshot(Some(2));
        assert_eq!(newer.posts.iter().map(|post| post.seq).collect::<Vec<_>>(), vec![3]);
        assert_eq!(newer.last_post, 3);
        assert!(newer.revision > snapshot.revision);

        // A member's recent steps, and nothing for a conversation outside the team.
        let steps = team.activity("w1", 10).unwrap();
        assert_eq!(steps.iter().map(|row| row.kind.as_str()).collect::<Vec<_>>(), vec!["message", "tool", "tool", "tool"]);
        assert_eq!(steps[0].title, "You have just started.");
        assert!(team.activity("other", 10).is_none());
        assert!(team.activity("../w1", 10).is_none());

        // Whose a tool call over ACP is: a member's name, never the chat's own agent's.
        assert_eq!(team.member_name("w1").as_deref(), Some("parser"));
        assert_eq!(team.member_name("e1").as_deref(), Some("Host Explorer"));
        assert_eq!(team.member_name(lead), None);
        assert_eq!(team.member_name("nobody"), None);
    }

    /// Tool call ids as `agy acp` 1.5.16 sends them: a member's carries its conversation, the chat's own agent's not.
    #[test]
    fn tells_a_members_call_from_the_agents_own() {
        assert_eq!(member_of_call("d5779e44-6979-4cd5-8b06-37256ccbdd33:call_5036756017209403336"), Some("d5779e44-6979-4cd5-8b06-37256ccbdd33"));
        assert_eq!(member_of_call("call_6896172746418138150"), None);
        assert_eq!(member_of_call("acp-perm-7"), None);
        assert_eq!(member_of_call("../../x:call_1"), None);
        assert_eq!(member_of_call("d5779e44-6979-4cd5-8b06-37256ccbdd33:"), None);
    }

    use std::io::Write as _;

    #[test]
    fn gathers_codex_sub_agents_from_notifications() {
        let mut team = CodexTeam::new("main");
        let started = json!({ "type": "subAgentActivity", "kind": "started", "agentThreadId": "c1", "agentPath": "/root/alpha", "model": "gpt-x" });
        assert_eq!(team.sub_agent_activity("main", &started).as_deref(), Some("c1"));
        assert_eq!(team.sub_agent_activity("main", &started), None, "known already");
        assert!(team.knows("c1") && !team.knows("other"));
        let line = team.agent_event("c1", "item/started", &json!({ "item": { "type": "commandExecution", "command": "rg alpha notes.txt" } })).unwrap();
        assert!(line.contains("rg alpha notes.txt"));
        team.agent_event("c1", "item/completed", &json!({ "item": { "type": "agentMessage", "text": "alpha: blue" } }));
        team.sub_agent_activity("main", &json!({ "type": "subAgentActivity", "kind": "completed", "agentThreadId": "c1", "agentPath": "/root/alpha" }));
        let snapshot = team.snapshot("s1", "running");
        assert_eq!(snapshot.source, "codex");
        assert_eq!(snapshot.members.len(), 2);
        assert_eq!((snapshot.members[1].name.as_str(), snapshot.members[1].status.as_str(), snapshot.members[1].model.as_deref()), ("alpha", "done", Some("gpt-x")));
        assert_eq!(snapshot.posts.len(), 1);
        assert_eq!((snapshot.posts[0].from.as_str(), snapshot.posts[0].to.clone(), snapshot.posts[0].text.as_str()), ("alpha", vec!["lead".to_owned()], "alpha: blue"));
        assert_eq!(team.reply_of("c1").as_deref(), Some("alpha: blue"));
        assert_eq!(team.activity("c1", 10).unwrap().len(), 2);
        // A message the lead sends to it goes on the board once.
        team.collab_call("main", &json!({ "type": "collabAgentToolCall", "tool": "sendInput", "status": "inProgress", "receiverThreadIds": ["c1"], "prompt": "and gamma?" }));
        team.collab_call("main", &json!({ "type": "collabAgentToolCall", "tool": "sendInput", "status": "completed", "receiverThreadIds": ["c1"], "prompt": "and gamma?" }));
        let posts = team.snapshot("s1", "idle").posts;
        assert_eq!(posts.len(), 2);
        assert_eq!((posts[1].from.as_str(), posts[1].to.clone()), ("lead", vec!["alpha".to_owned()]));
    }
}
