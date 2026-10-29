//! What each model offers for reasoning: the levels a request can name, the level its provider uses when a request
//! names none, and what each level sends. The Host builds requests from this table and the interface draws its levels
//! from it, so the two always agree. It follows the providers' own documentation as of 2026-10-28
//! (`docs/reasoning-models.md` lists the sources); a model it does not know gets a generic set for its protocol, its
//! service and the reasoning format its profile names.

use std::cmp::Reverse;

/// A wire protocol of model services.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Api {
    /// OpenAI Chat Completions.
    Completions,
    /// OpenAI Responses.
    Responses,
    /// Anthropic Messages.
    Anthropic,
}

impl Api {
    pub fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "openai-completions" => Self::Completions,
            "openai-responses" => Self::Responses,
            "anthropic-messages" => Self::Anthropic,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Completions => "openai-completions",
            Self::Responses => "openai-responses",
            Self::Anthropic => "anthropic-messages",
        }
    }

    /// A profile's protocol: the one it names, else Chat Completions for a local model and Responses for an API one.
    pub fn of(api: Option<&str>, local: bool) -> Self {
        api.and_then(Self::parse).unwrap_or(if local { Self::Completions } else { Self::Responses })
    }
}

/// Rainy's reasoning levels, weakest first. They are the names the providers share — OpenAI's none to max, Claude's
/// low to max, Gemini's minimal to high, DeepSeek's low, high and max — so a level means what its provider calls it.
/// Each model offers some of them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Level {
    Off,
    Minimal,
    Low,
    Medium,
    High,
    XHigh,
    Max,
}

impl Level {
    pub const ALL: [Level; 7] = [Level::Off, Level::Minimal, Level::Low, Level::Medium, Level::High, Level::XHigh, Level::Max];

    pub fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "off" => Self::Off,
            "minimal" => Self::Minimal,
            "low" => Self::Low,
            "medium" => Self::Medium,
            "high" => Self::High,
            "xhigh" => Self::XHigh,
            "max" => Self::Max,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "xhigh",
            Self::Max => "max",
        }
    }

    /// The level as 3.x and 4.0–4.2 store it: the nearest of off, low, high and max, the only names they read. Minimal
    /// still reasons, so it is low; medium and xhigh lie as near to high as to low or max and become high, which every
    /// reasoning model those versions knew accepts, where many refuse max.
    pub fn legacy(self) -> Self {
        match self {
            Self::Off => Self::Off,
            Self::Minimal | Self::Low => Self::Low,
            Self::Medium | Self::High | Self::XHigh => Self::High,
            Self::Max => Self::Max,
        }
    }
}

/// A level as files and messages store it: `thinking` with the name 3.x and 4.0–4.2 read, and `thinkingLevel` with the
/// level itself when it is not one of those names.
pub fn store_level(level: Option<&str>) -> (Option<String>, Option<String>) {
    match level.map(|text| (text, Level::parse(text))) {
        None => (None, None),
        Some((text, Some(level))) if level.legacy() != level => (Some(level.legacy().as_str().to_owned()), Some(text.to_owned())),
        Some((text, _)) => (Some(text.to_owned()), None),
    }
}

/// The level a stored `thinking` and `thinkingLevel` name: `thinkingLevel` while `thinking` still holds its legacy
/// name, else `thinking`. An older version that changed the level wrote `thinking` alone and dropped the other field;
/// a file edited by hand may disagree the same way.
pub fn read_level(thinking: Option<String>, level: Option<String>) -> Option<String> {
    match (&thinking, level.as_deref().and_then(Level::parse)) {
        (Some(stored), Some(fine)) if stored == fine.legacy().as_str() => level,
        _ => thinking,
    }
}

/// What a request sends for one level.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wire {
    /// Reasoning switched off.
    Off,
    /// Reasoning on, at the model's own amount: no grade is named.
    On,
    /// A named effort.
    Effort(&'static str),
    /// A thinking budget, in tokens.
    Budget(u32),
}

/// Where a request carries the level.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Control {
    /// Nowhere: the model takes no reasoning parameter.
    None,
    /// Chat Completions `reasoning_effort`, Responses `reasoning.effort` and, for Kimi K3, Anthropic
    /// `output_config.effort`; `none` turns reasoning off.
    Effort,
    /// `thinking: {type: enabled | disabled}` and, while enabled, Chat Completions `reasoning_effort` or Anthropic
    /// `output_config.effort` (DeepSeek, Zhipu GLM, Moonshot Kimi K2.6, Ant Ling and other Anthropic-compatible
    /// servers). `keep` also sends `clear_thinking: false`, which keeps earlier turns' reasoning (GLM).
    Thinking { keep: bool },
    /// DashScope: `enable_thinking`, with `reasoning_effort` or `thinking_budget`.
    DashScope,
    /// vLLM, llama.cpp, SGLang and NVIDIA NIM: `chat_template_kwargs.enable_thinking`, with `reasoning_effort`.
    Template,
    /// OpenRouter: `reasoning: {effort | enabled | max_tokens}`.
    OpenRouter,
    /// Together AI: `reasoning: {enabled}`, and `reasoning_effort` for graded levels.
    Together,
    /// Claude over Anthropic Messages: adaptive thinking with `output_config.effort`, each reply's effort replayed
    /// before it when `effort_messages` (the per-message effort beta), or `thinking.budget_tokens` when not `adaptive`.
    Claude { adaptive: bool, effort_messages: bool },
}

/// One model's reasoning: the levels a request can name, weakest first, with what each sends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reasoning {
    pub levels: &'static [(Level, Wire)],
    /// The level the provider uses when a request names none.
    pub default: Option<Level>,
    pub control: Control,
    /// The model reasons by itself and takes no level.
    pub always: bool,
}

impl Reasoning {
    /// A model that does not reason.
    pub const NONE: Reasoning = Reasoning { levels: &[], default: None, control: Control::None, always: false };

    pub fn levels(&self) -> impl Iterator<Item = Level> + '_ {
        self.levels.iter().map(|(level, _)| *level)
    }

    pub fn wire(&self, level: Level) -> Option<Wire> {
        self.levels.iter().find(|(offered, _)| *offered == level).map(|(_, wire)| *wire)
    }

    /// Whether the user has a choice: two levels or more.
    pub fn adjustable(&self) -> bool {
        self.levels.len() > 1
    }

    /// Whether the model reasons at all.
    pub fn reasons(&self) -> bool {
        self.always || self.levels.iter().any(|(level, _)| *level != Level::Off)
    }

    /// Whether the levels only switch reasoning off and on.
    pub fn toggle(&self) -> bool {
        matches!(self.levels, [(Level::Off, Wire::Off), (_, Wire::On)])
    }

    /// The level a request for `requested` uses: the level itself when the model offers it; off, on a model that cannot
    /// turn reasoning off, its weakest level; any other level the nearest one offered, the stronger on a tie, never off.
    pub fn resolve(&self, requested: Level) -> Option<Level> {
        if self.levels().any(|level| level == requested) {
            return Some(requested);
        }
        if requested == Level::Off {
            return self.levels().next();
        }
        let distance = |level: Level| (level as i32 - requested as i32).abs();
        self.levels().filter(|level| *level != Level::Off).min_by_key(|level| (distance(*level), Reverse(*level))).or_else(|| self.levels().next())
    }

    /// The level a request uses for a level name, or for the provider's default when it names none or one Rainy does
    /// not know.
    pub fn choose(&self, requested: Option<&str>) -> Option<Level> {
        requested.and_then(Level::parse).or(self.default).and_then(|level| self.resolve(level))
    }
}

/// The parts of a model profile the table reads.
#[derive(Clone, Copy, Debug)]
pub struct Profile<'a> {
    pub api: Api,
    pub base_url: &'a str,
    pub provider: &'a str,
    pub model: &'a str,
    /// `openai`, `deepseek` or `qwen`: the reasoning format a Chat Completions profile names.
    pub thinking_format: Option<&'a str>,
    pub local: bool,
    /// The profile has a reasoning level; a model the table does not know reasons only then.
    pub reasoning: bool,
}

use Level::{High, Low, Max, Medium, Minimal, Off, XHigh};

const fn effort(level: Level, name: &'static str) -> (Level, Wire) {
    (level, Wire::Effort(name))
}

const OFF: (Level, Wire) = (Off, Wire::Off);
const ON: (Level, Wire) = (High, Wire::On);
const MINIMAL: (Level, Wire) = effort(Minimal, "minimal");
const LOW: (Level, Wire) = effort(Low, "low");
const MEDIUM: (Level, Wire) = effort(Medium, "medium");
const HIGH: (Level, Wire) = effort(High, "high");
const XHIGH: (Level, Wire) = effort(XHigh, "xhigh");
const MAX: (Level, Wire) = effort(Max, "max");

const NONE_TO_MAX: &[(Level, Wire)] = &[OFF, LOW, MEDIUM, HIGH, XHIGH, MAX];
const LOW_TO_MAX: &[(Level, Wire)] = &[LOW, MEDIUM, HIGH, XHIGH, MAX];
const NONE_TO_XHIGH: &[(Level, Wire)] = &[OFF, LOW, MEDIUM, HIGH, XHIGH];
const LOW_TO_XHIGH: &[(Level, Wire)] = &[LOW, MEDIUM, HIGH, XHIGH];
const MEDIUM_TO_XHIGH: &[(Level, Wire)] = &[MEDIUM, HIGH, XHIGH];
const NONE_TO_HIGH: &[(Level, Wire)] = &[OFF, LOW, MEDIUM, HIGH];
const MINIMAL_TO_HIGH: &[(Level, Wire)] = &[MINIMAL, LOW, MEDIUM, HIGH];
const LOW_TO_HIGH: &[(Level, Wire)] = &[LOW, MEDIUM, HIGH];
const HIGH_ONLY: &[(Level, Wire)] = &[HIGH];
/// Claude Opus 4.6 and Sonnet 4.6: xhigh came with Opus 4.7.
const CLAUDE_46: &[(Level, Wire)] = &[OFF, LOW, MEDIUM, HIGH, MAX];
/// Claude before 4.6, and Anthropic-compatible servers Rainy does not know: a thinking budget.
const BUDGETS: &[(Level, Wire)] = &[OFF, (Low, Wire::Budget(2048)), (Medium, Wire::Budget(8192)), (High, Wire::Budget(16_384)), (Max, Wire::Budget(32_768))];
/// DeepSeek: low, high and max (it maps minimal to low, medium and xhigh to high).
const NONE_LOW_HIGH_MAX: &[(Level, Wire)] = &[OFF, LOW, HIGH, MAX];
/// Kimi K3 and GLM-5.3, which always reason.
const LOW_HIGH_MAX: &[(Level, Wire)] = &[LOW, HIGH, MAX];
/// GLM-5.2 (it maps low and medium to high).
const NONE_HIGH_MAX: &[(Level, Wire)] = &[OFF, HIGH, MAX];
const NONE_HIGH: &[(Level, Wire)] = &[OFF, HIGH];
/// Qwen3.8: low, medium and xhigh (DashScope maps high and max to xhigh; the open model's template refuses them).
const QWEN_EFFORTS: &[(Level, Wire)] = &[OFF, LOW, MEDIUM, XHIGH];
const QWEN_EFFORTS_ON: &[(Level, Wire)] = &[LOW, MEDIUM, XHIGH];
/// Qwen3.5 to 3.7 grade reasoning only by `thinking_budget`; Rainy uses the budgets DashScope gives Qwen3.8's low and
/// medium, and the model's own (its whole reasoning length) above them.
const QWEN_BUDGETS: &[(Level, Wire)] = &[OFF, (Low, Wire::Budget(4096)), (Medium, Wire::Budget(16_384)), ON];
const QWEN_BUDGETS_ON: &[(Level, Wire)] = &[(Low, Wire::Budget(4096)), (Medium, Wire::Budget(16_384)), ON];
const TOGGLE: &[(Level, Wire)] = &[OFF, ON];
const ON_ONLY: &[(Level, Wire)] = &[ON];

/// A model family: its levels and default, and how its own vendor takes them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Family {
    levels: &'static [(Level, Wire)],
    default: Option<Level>,
    native: Control,
    always: bool,
}

const fn graded(levels: &'static [(Level, Wire)], default: Level, native: Control) -> Family {
    Family { levels, default: Some(default), native, always: false }
}

/// Reasons by itself; no parameter changes how much.
const FIXED: Family = Family { levels: &[], default: None, native: Control::None, always: true };
/// Does not reason.
const PLAIN: Family = Family { levels: &[], default: None, native: Control::None, always: false };

/// A model id as the table compares it: lower case, without a `vendor/` prefix (OpenRouter, Together, NVIDIA) or a
/// `:variant` suffix (OpenRouter).
fn normalize(model: &str) -> String {
    let lower = model.trim().to_ascii_lowercase();
    let id = lower.rsplit('/').next().unwrap_or(&lower);
    id.split(':').next().unwrap_or(id).to_owned()
}

fn claude(id: &str) -> Family {
    // Bedrock prefixes the vendor (`anthropic.claude-…`), Vertex appends `@date`, OpenRouter writes versions with dots.
    let id = id.split('@').next().unwrap_or(id).replace('.', "-");
    let starts = |prefixes: &[&str]| prefixes.iter().any(|prefix| id.starts_with(prefix));
    let managed = Control::Claude { adaptive: true, effort_messages: true };
    let adaptive = Control::Claude { adaptive: true, effort_messages: false };
    let budget = Control::Claude { adaptive: false, effort_messages: false };
    if starts(&["claude-fable-5-1", "claude-mythos-5-1"]) {
        graded(LOW_TO_MAX, High, managed)
    } else if starts(&["claude-fable-5", "claude-mythos-5"]) {
        graded(LOW_TO_MAX, High, adaptive)
    } else if starts(&["claude-opus-5-5", "claude-haiku-5-5"]) {
        // Thinking cannot be disabled on Opus 5.5; Haiku 5.5 accepts `disabled` only without per-message effort.
        graded(LOW_TO_MAX, Medium, managed)
    } else if starts(&["claude-sonnet-5-5"]) {
        // Its thinking-off form (`between_tools`) refuses per-message effort.
        graded(LOW_TO_MAX, High, managed)
    } else if starts(&["claude-opus-5"]) {
        // `disabled` only up to high effort, and Anthropic advises lower effort instead.
        graded(LOW_TO_MAX, High, managed)
    } else if starts(&["claude-sonnet-5"]) {
        graded(NONE_TO_MAX, High, adaptive)
    } else if starts(&["claude-opus-4-8", "claude-opus-4-7"]) {
        // A request without `thinking` runs without it.
        graded(NONE_TO_MAX, Off, adaptive)
    } else if starts(&["claude-opus-4-6", "claude-sonnet-4-6"]) {
        graded(CLAUDE_46, Off, adaptive)
    } else if starts(&["claude-opus-4", "claude-sonnet-4", "claude-haiku-4", "claude-3-7"]) {
        graded(BUDGETS, Off, budget)
    } else {
        PLAIN
    }
}

/// The family of a normalized model id; `None` for a model the table does not know.
fn family(id: &str) -> Option<Family> {
    use Control::{DashScope, Effort, Template, Thinking};
    let starts = |prefixes: &[&str]| prefixes.iter().any(|prefix| id.starts_with(prefix));
    if let Some(at) = id.find("claude-") {
        return Some(claude(&id[at..]));
    }
    let o_series = id.starts_with('o') && id.chars().nth(1).is_some_and(|c| c.is_ascii_digit());
    Some(if starts(&["gpt-6-astra", "gpt-6.1-sol"]) {
        graded(LOW_TO_MAX, Medium, Effort)
    } else if starts(&["gpt-6", "gpt-5.6"]) {
        graded(NONE_TO_MAX, Medium, Effort)
    } else if starts(&["gpt-5.5-pro"]) {
        graded(MEDIUM_TO_XHIGH, High, Effort)
    } else if starts(&["gpt-5.4-pro", "gpt-5.2-pro"]) {
        graded(MEDIUM_TO_XHIGH, Medium, Effort)
    } else if starts(&["gpt-5.5"]) {
        graded(NONE_TO_XHIGH, Medium, Effort)
    } else if starts(&["gpt-5.4", "gpt-5.2"]) {
        graded(NONE_TO_XHIGH, Off, Effort)
    } else if starts(&["gpt-5.3"]) {
        graded(LOW_TO_XHIGH, Medium, Effort)
    } else if starts(&["gpt-5.1"]) {
        graded(NONE_TO_HIGH, Off, Effort)
    } else if starts(&["gpt-5-pro"]) {
        graded(HIGH_ONLY, High, Effort)
    } else if starts(&["gpt-5-chat"]) {
        PLAIN
    } else if starts(&["gpt-5"]) {
        graded(MINIMAL_TO_HIGH, Medium, Effort)
    } else if o_series || starts(&["gpt-oss"]) {
        graded(LOW_TO_HIGH, Medium, Effort)
    } else if starts(&["gpt-4", "gpt-3", "chatgpt-", "chat-latest"]) {
        PLAIN
    } else if starts(&["deepseek-r1"]) {
        FIXED
    } else if starts(&["deepseek"]) {
        graded(NONE_LOW_HIGH_MAX, High, Thinking { keep: false })
    } else if starts(&["qwen3.8", "qwen-3.8"]) {
        graded(if id.contains("2.4t") { QWEN_EFFORTS_ON } else { QWEN_EFFORTS }, XHigh, DashScope)
    } else if starts(&["qwen3.7-max-preview", "qwen3.7-max-20"]) {
        graded(QWEN_BUDGETS_ON, High, DashScope)
    } else if starts(&["qwen3-coder", "qwen-coder", "qwen-max", "qwen-long", "qwen-vl", "qwen2"]) {
        PLAIN
    } else if starts(&["qwen3.5", "qwen3.6", "qwen3.7", "qwen3-"]) {
        graded(QWEN_BUDGETS, High, DashScope)
    } else if starts(&["qwen-plus", "qwen-flash", "qwen-turbo"]) {
        graded(QWEN_BUDGETS, Off, DashScope)
    } else if starts(&["qwq", "qvq"]) {
        FIXED
    } else if starts(&["kimi-k3"]) {
        graded(LOW_HIGH_MAX, Max, Effort)
    } else if starts(&["kimi-k2.6"]) {
        graded(TOGGLE, High, Thinking { keep: false })
    } else if starts(&["kimi-k2.7", "kimi-k2-thinking"]) {
        FIXED
    } else if starts(&["kimi-k2", "kimi-latest", "moonshot-v1"]) {
        PLAIN
    } else if starts(&["glm-5.3"]) {
        graded(LOW_HIGH_MAX, Max, Thinking { keep: true })
    } else if starts(&["glm-5.2"]) {
        graded(NONE_HIGH_MAX, Max, Thinking { keep: true })
    } else if starts(&["glm-5", "glm-4.5", "glm-4.6", "glm-4.7"]) {
        graded(TOGGLE, High, Thinking { keep: true })
    } else if starts(&["gemini-4", "gemini-3.8"]) {
        graded(LOW_TO_HIGH, Medium, Effort)
    } else if starts(&["gemini-3.6"]) {
        graded(MINIMAL_TO_HIGH, Medium, Effort)
    } else if starts(&["gemini-3"]) && id.contains("flash-lite") {
        graded(MINIMAL_TO_HIGH, Minimal, Effort)
    } else if starts(&["gemini-3"]) && id.contains("flash") {
        graded(MINIMAL_TO_HIGH, High, Effort)
    } else if starts(&["gemini-3"]) {
        graded(LOW_TO_HIGH, High, Effort)
    } else if starts(&["gemini-2.5-flash-lite"]) {
        graded(NONE_TO_HIGH, Off, Effort)
    } else if starts(&["gemini-2.5-flash"]) {
        // Gemini 2.5 thinks dynamically when nothing is sent; medium is the nearest named level.
        graded(NONE_TO_HIGH, Medium, Effort)
    } else if starts(&["gemini-2.5"]) {
        graded(LOW_TO_HIGH, Medium, Effort)
    } else if starts(&["grok-4.7", "grok-4.6"]) {
        graded(LOW_TO_XHIGH, High, Effort)
    } else if starts(&["grok-4.5"]) {
        graded(LOW_TO_HIGH, High, Effort)
    } else if starts(&["grok-4.3"]) {
        graded(NONE_TO_XHIGH, Low, Effort)
    } else if starts(&["grok-"]) && id.contains("non-reasoning") {
        PLAIN
    } else if starts(&["grok-"]) {
        FIXED
    } else if starts(&["mistral-small-latest", "mistral-medium-3", "mistral-large-4"]) {
        graded(NONE_HIGH, High, Effort)
    } else if starts(&["magistral", "ring-"]) {
        FIXED
    } else if starts(&["ling-"]) {
        graded(TOGGLE, High, Thinking { keep: false })
    } else if id.contains("nemotron") {
        graded(TOGGLE, High, Template)
    } else {
        return None;
    })
}

/// The service behind a profile, by its address (and the provider ids pi-ai named them with).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Service {
    OpenAi,
    DeepSeek,
    DashScope,
    Zhipu,
    Moonshot,
    Google,
    Xai,
    OpenRouter,
    Together,
    Nvidia,
    Cerebras,
    Cloudflare,
    Ling,
    Mistral,
    Local,
    Other,
}

fn service(profile: &Profile<'_>) -> Service {
    let url = profile.base_url.trim().to_ascii_lowercase();
    let authority = url.split_once("://").map_or(url.as_str(), |(_, rest)| rest).split(['/', '?', '#']).next().unwrap_or_default();
    let authority = authority.rsplit('@').next().unwrap_or(authority);
    let host = match authority.strip_prefix('[') {
        Some(rest) => rest.split(']').next().unwrap_or(rest),
        None => authority.split(':').next().unwrap_or(authority),
    };
    let is = |domain: &str| host == domain || host.strip_suffix(domain).is_some_and(|rest| rest.ends_with('.'));
    let provider = profile.provider;
    if is("deepseek.com") || provider == "deepseek" {
        Service::DeepSeek
    } else if is("aliyuncs.com") {
        Service::DashScope
    } else if is("bigmodel.cn") || is("z.ai") || provider == "zai" {
        Service::Zhipu
    } else if is("moonshot.cn") || is("moonshot.ai") {
        Service::Moonshot
    } else if is("googleapis.com") {
        Service::Google
    } else if is("x.ai") || provider == "xai" {
        Service::Xai
    } else if is("openrouter.ai") || provider == "openrouter" {
        Service::OpenRouter
    } else if is("together.ai") || is("together.xyz") {
        Service::Together
    } else if is("nvidia.com") {
        Service::Nvidia
    } else if is("cerebras.ai") {
        Service::Cerebras
    } else if host == "api.cloudflare.com" {
        Service::Cloudflare
    } else if is("ant-ling.com") {
        Service::Ling
    } else if is("mistral.ai") {
        Service::Mistral
    } else if is("openai.com") || is("openai.azure.com") {
        Service::OpenAi
    } else if profile.local || host == "localhost" || host == "::1" || host.starts_with("127.") || host.ends_with(".localhost") {
        Service::Local
    } else {
        Service::Other
    }
}

/// Where the request carries the level, by protocol and service. The format a profile names decides only for a local
/// server or a service the table does not know; known services take their own parameters.
fn control(profile: &Profile<'_>, service: Service, native: Option<Control>) -> Control {
    match profile.api {
        Api::Anthropic => match native {
            Some(claude @ Control::Claude { .. }) => claude,
            Some(Control::Effort) => Control::Effort,
            _ => Control::Thinking { keep: false },
        },
        Api::Responses => Control::Effort,
        Api::Completions => match service {
            Service::DeepSeek | Service::Ling => Control::Thinking { keep: false },
            Service::Zhipu => Control::Thinking { keep: true },
            Service::DashScope => Control::DashScope,
            Service::OpenRouter => Control::OpenRouter,
            Service::Together => Control::Together,
            Service::Nvidia => Control::Template,
            Service::Moonshot => match native {
                Some(thinking @ Control::Thinking { .. }) => thinking,
                _ => Control::Effort,
            },
            Service::Local => match profile.thinking_format {
                Some("qwen") => Control::Template,
                Some("deepseek") => Control::Thinking { keep: false },
                _ => Control::Effort,
            },
            Service::Other => match (profile.thinking_format, native) {
                (Some("qwen"), _) => Control::DashScope,
                (Some("deepseek"), _) => Control::Thinking { keep: false },
                (_, Some(native @ (Control::Thinking { .. } | Control::DashScope | Control::Template))) => native,
                _ => Control::Effort,
            },
            Service::OpenAi | Service::Google | Service::Xai | Service::Cerebras | Service::Cloudflare | Service::Mistral => Control::Effort,
        },
    }
}

/// Whether a request can carry a thinking budget.
fn takes_budgets(api: Api, control: Control) -> bool {
    match api {
        Api::Anthropic => true,
        Api::Responses => false,
        Api::Completions => matches!(control, Control::DashScope | Control::OpenRouter),
    }
}

/// The same family switching reasoning only off and on, where its grades cannot be sent.
fn switch_only(family: Family) -> Family {
    let off = family.levels.first().is_some_and(|(level, _)| *level == Off);
    let default = if off && family.default == Some(Off) { Off } else { High };
    Family { levels: if off { TOGGLE } else { ON_ONLY }, default: Some(default), ..family }
}

/// A model the table does not know: what its protocol and service generally take, when its profile reasons.
fn generic(profile: &Profile<'_>, service: Service, control: Control) -> Reasoning {
    if !profile.reasoning {
        return Reasoning::NONE;
    }
    let (levels, default) = match (profile.api, control) {
        (Api::Anthropic, _) => (BUDGETS, Off),
        (Api::Completions, Control::Thinking { .. }) if service == Service::DeepSeek || profile.thinking_format == Some("deepseek") => (NONE_LOW_HIGH_MAX, High),
        (Api::Completions, Control::Thinking { .. } | Control::DashScope | Control::Template | Control::Together) => (TOGGLE, High),
        _ => (NONE_TO_HIGH, Medium),
    };
    Reasoning { levels, default: Some(default), control, always: false }
}

/// What a profile's model offers for reasoning.
pub fn reasoning(profile: &Profile<'_>) -> Reasoning {
    let id = normalize(profile.model);
    let service = service(profile);
    let family = family(&id);
    let control = control(profile, service, family.map(|family| family.native));
    let Some(family) = family else { return generic(profile, service, control) };
    if family.levels.is_empty() {
        return Reasoning { always: family.always, ..Reasoning::NONE };
    }
    let family = match service {
        // Together switches hybrid models with `reasoning.enabled`; NVIDIA's chat templates only switch thinking.
        Service::Together if !id.starts_with("gpt-oss") => switch_only(family),
        Service::Nvidia => switch_only(family),
        // Cerebras serves Qwen3.8 with OpenAI's names up to high.
        Service::Cerebras if family.levels == QWEN_EFFORTS => graded(NONE_TO_HIGH, High, family.native),
        _ => family,
    };
    let budgets = family.levels.iter().any(|(_, wire)| matches!(wire, Wire::Budget(_)));
    let family = if budgets && !takes_budgets(profile.api, control) { switch_only(family) } else { family };
    Reasoning { levels: family.levels, default: family.default, control, always: false }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(api: Api, base_url: &'static str, model: &'static str) -> Profile<'static> {
        Profile { api, base_url, provider: "p", model, thinking_format: None, local: false, reasoning: true }
    }

    fn names(reasoning: &Reasoning) -> Vec<&'static str> {
        reasoning.levels().map(Level::as_str).collect()
    }

    #[test]
    fn stores_levels_for_older_versions() {
        assert_eq!(store_level(Some("xhigh")), (Some("high".into()), Some("xhigh".into())));
        assert_eq!(store_level(Some("minimal")), (Some("low".into()), Some("minimal".into())));
        assert_eq!(store_level(Some("max")), (Some("max".into()), None));
        assert_eq!(store_level(None), (None, None));
        // The fine level is read while the legacy field still names it.
        assert_eq!(read_level(Some("high".into()), Some("medium".into())).as_deref(), Some("medium"));
        // An older version changed the level and kept only `thinking`.
        assert_eq!(read_level(Some("low".into()), Some("xhigh".into())).as_deref(), Some("low"));
        assert_eq!(read_level(None, Some("xhigh".into())), None);
        assert_eq!(read_level(Some("high".into()), Some("bogus".into())).as_deref(), Some("high"));
    }

    #[test]
    fn resolves_to_the_nearest_level() {
        let deepseek = reasoning(&profile(Api::Completions, "https://api.deepseek.com", "deepseek-flash"));
        assert_eq!(names(&deepseek), ["off", "low", "high", "max"]);
        assert_eq!(deepseek.resolve(Medium), Some(High));
        assert_eq!(deepseek.resolve(Minimal), Some(Low));
        assert_eq!(deepseek.resolve(XHigh), Some(Max));
        let kimi = reasoning(&profile(Api::Completions, "https://api.moonshot.cn/v1", "kimi-k3.1-pro"));
        // A model that always reasons thinks a little instead of not at all.
        assert_eq!(kimi.resolve(Off), Some(Low));
        assert_eq!(kimi.choose(None), Some(Max));
        assert_eq!(kimi.choose(Some("bogus")), Some(Max));
        let qwen = reasoning(&profile(Api::Completions, "https://dashscope.aliyuncs.com/compatible-mode/v1", "qwen3.8-max"));
        assert_eq!(qwen.resolve(High), Some(XHigh));
        assert_eq!(qwen.resolve(Max), Some(XHigh));
        assert_eq!(Reasoning::NONE.resolve(High), None);
    }

    /// A protocol, an address and a model id, with the levels and the default the table gives them.
    type Case = (Api, &'static str, &'static str, &'static [&'static str], Option<&'static str>);

    #[test]
    fn knows_each_family() {
        let cases: &[Case] = &[
            (Api::Anthropic, "https://api.anthropic.com", "claude-opus-5-5", &["low", "medium", "high", "xhigh", "max"], Some("medium")),
            (Api::Anthropic, "https://api.anthropic.com", "claude-sonnet-5", &["off", "low", "medium", "high", "xhigh", "max"], Some("high")),
            (Api::Anthropic, "https://api.anthropic.com", "claude-opus-4-6", &["off", "low", "medium", "high", "max"], Some("off")),
            (Api::Anthropic, "https://api.anthropic.com", "claude-haiku-4-5-20251001", &["off", "low", "medium", "high", "max"], Some("off")),
            (Api::Responses, "https://api.openai.com/v1", "gpt-6.1-sol", &["low", "medium", "high", "xhigh", "max"], Some("medium")),
            (Api::Responses, "https://api.openai.com/v1", "gpt-6.1-terra", &["off", "low", "medium", "high", "xhigh", "max"], Some("medium")),
            (Api::Responses, "https://api.openai.com/v1", "gpt-5.5", &["off", "low", "medium", "high", "xhigh"], Some("medium")),
            (Api::Responses, "https://api.openai.com/v1", "gpt-5.5-pro", &["medium", "high", "xhigh"], Some("high")),
            (Api::Responses, "https://api.openai.com/v1", "gpt-5.4-mini", &["off", "low", "medium", "high", "xhigh"], Some("off")),
            (Api::Responses, "https://api.openai.com/v1", "gpt-5-mini", &["minimal", "low", "medium", "high"], Some("medium")),
            (Api::Responses, "https://api.openai.com/v1", "o3", &["low", "medium", "high"], Some("medium")),
            (Api::Responses, "https://api.openai.com/v1", "gpt-4.1", &[], None),
            (Api::Completions, "https://generativelanguage.googleapis.com/v1beta/openai", "gemini-3.6-flash", &["minimal", "low", "medium", "high"], Some("medium")),
            (Api::Completions, "https://generativelanguage.googleapis.com/v1beta/openai", "gemini-2.5-flash", &["off", "low", "medium", "high"], Some("medium")),
            (Api::Completions, "https://api.x.ai/v1", "grok-4.7", &["low", "medium", "high", "xhigh"], Some("high")),
            (Api::Completions, "https://api.x.ai/v1", "grok-4.3", &["off", "low", "medium", "high", "xhigh"], Some("low")),
            (Api::Completions, "https://open.bigmodel.cn/api/paas/v4", "glm-5.3", &["low", "high", "max"], Some("max")),
            (Api::Completions, "https://api.z.ai/api/paas/v4", "glm-4.7", &["off", "high"], Some("high")),
            (Api::Completions, "https://api.moonshot.ai/v1", "kimi-k2.6", &["off", "high"], Some("high")),
            (Api::Completions, "https://dashscope.aliyuncs.com/compatible-mode/v1", "qwen3.6-plus", &["off", "low", "medium", "high"], Some("high")),
            (Api::Completions, "https://dashscope.aliyuncs.com/compatible-mode/v1", "qwen-plus", &["off", "low", "medium", "high"], Some("off")),
            (Api::Responses, "https://api.deepseek.com", "deepseek-flash", &["off", "low", "high", "max"], Some("high")),
        ];
        for (api, url, model, levels, default) in cases {
            let found = reasoning(&profile(*api, url, model));
            assert_eq!(names(&found), *levels, "{model}");
            assert_eq!(found.default.map(Level::as_str), *default, "{model}");
        }
    }

    #[test]
    fn picks_where_the_level_goes() {
        let control = |api, url, model| reasoning(&profile(api, url, model)).control;
        assert_eq!(control(Api::Anthropic, "https://api.anthropic.com", "claude-opus-5-5"), Control::Claude { adaptive: true, effort_messages: true });
        assert_eq!(control(Api::Anthropic, "https://api.anthropic.com", "claude-fable-5"), Control::Claude { adaptive: true, effort_messages: false });
        assert_eq!(control(Api::Anthropic, "https://api.deepseek.com/anthropic", "deepseek-v4-pro"), Control::Thinking { keep: false });
        assert_eq!(control(Api::Completions, "https://api.deepseek.com", "deepseek-flash"), Control::Thinking { keep: false });
        assert_eq!(control(Api::Completions, "https://open.bigmodel.cn/api/paas/v4", "glm-5.2"), Control::Thinking { keep: true });
        assert_eq!(control(Api::Completions, "https://api.moonshot.cn/v1", "kimi-k3"), Control::Effort);
        assert_eq!(control(Api::Completions, "https://api.moonshot.cn/v1", "kimi-k2.6"), Control::Thinking { keep: false });
        assert_eq!(control(Api::Completions, "https://openrouter.ai/api/v1", "openai/gpt-5.5"), Control::OpenRouter);
        // A proxy Rainy does not know gets the family's own parameters.
        assert_eq!(control(Api::Completions, "https://llm.example.com/v1", "deepseek-v4-pro"), Control::Thinking { keep: false });
        assert_eq!(control(Api::Completions, "https://llm.example.com/v1", "gpt-5.5"), Control::Effort);
    }

    #[test]
    fn adapts_levels_to_the_service() {
        // Budgets need a field for them: a local server only switches Qwen3.6's thinking.
        let local = Profile { local: true, ..profile(Api::Completions, "http://127.0.0.1:8080/v1", "Qwen3.6-35B-A3B") };
        let found = reasoning(&local);
        assert_eq!((names(&found), found.control, found.toggle()), (vec!["off", "high"], Control::Effort, true));
        let template = reasoning(&Profile { thinking_format: Some("qwen"), ..local });
        assert_eq!(template.control, Control::Template);
        // Strata and llama.cpp take Qwen3.8's own names.
        let strata = reasoning(&Profile { model: "Qwen3.8-27B", ..local });
        assert_eq!(names(&strata), ["off", "low", "medium", "xhigh"]);
        let together = reasoning(&profile(Api::Completions, "https://api.together.xyz/v1", "deepseek-ai/DeepSeek-V4-Pro-0813"));
        assert_eq!((names(&together), together.control), (vec!["off", "high"], Control::Together));
        let oss = reasoning(&profile(Api::Completions, "https://api.together.xyz/v1", "openai/gpt-oss-120b"));
        assert_eq!(names(&oss), ["low", "medium", "high"]);
        let cerebras = reasoning(&profile(Api::Completions, "https://api.cerebras.ai/v1", "qwen-3.8-27b"));
        assert_eq!((names(&cerebras), cerebras.default), (vec!["off", "low", "medium", "high"], Some(High)));
        // The Responses API has no budget field either.
        let responses = reasoning(&profile(Api::Responses, "https://dashscope.aliyuncs.com/compatible-mode/v1", "qwen3.5-plus"));
        assert_eq!(names(&responses), ["off", "high"]);
    }

    #[test]
    fn falls_back_to_the_profile_for_unknown_models() {
        let unknown = profile(Api::Completions, "https://llm.example.com/v1", "house-model");
        assert_eq!(names(&reasoning(&unknown)), ["off", "low", "medium", "high"]);
        assert_eq!(reasoning(&Profile { reasoning: false, ..unknown }), Reasoning::NONE);
        let deepseek_format = reasoning(&Profile { thinking_format: Some("deepseek"), ..unknown });
        assert_eq!((names(&deepseek_format), deepseek_format.default), (vec!["off", "low", "high", "max"], Some(High)));
        let qwen_format = reasoning(&Profile { thinking_format: Some("qwen"), ..unknown });
        assert_eq!((names(&qwen_format), qwen_format.control), (vec!["off", "high"], Control::DashScope));
        let anthropic = reasoning(&profile(Api::Anthropic, "https://llm.example.com", "house-model"));
        assert_eq!((anthropic.wire(High), anthropic.control), (Some(Wire::Budget(16_384)), Control::Thinking { keep: false }));
        // Known models ignore the profile's switch: their table entry decides.
        assert!(reasoning(&Profile { reasoning: false, ..profile(Api::Responses, "https://api.openai.com/v1", "gpt-5.5") }).adjustable());
        assert!(reasoning(&profile(Api::Responses, "https://api.x.ai/v1", "grok-4.20-0309-reasoning")).always);
    }
}
