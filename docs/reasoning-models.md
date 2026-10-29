# Reasoning levels per model

Rainy asks each model for reasoning the way its provider documents it: the levels the model really offers, under the
provider's own names, in the request field that model takes, starting from the provider's default. This page is the
source of the table in `native/crates/protocol/src/models/reasoning.rs`, which the Host builds requests from
(`host/src/agent/llm/`) and the interface draws the composer's control and the Settings form from. Everything here was
checked against the providers' own documentation on **2026-10-28**; re-check before adding or changing a family.

## Levels

Rainy has seven levels, weakest first. They are the names the providers share, so a level means what its provider
calls it; each model offers some of them.

| Level | Wire name (most providers) | 中文 | English |
|---|---|---|---|
| `off` | `none`, or a thinking switch set off | 关闭 | Off |
| `minimal` | `minimal` | 极低 | Minimal |
| `low` | `low` | 低 | Low |
| `medium` | `medium` | 中 | Medium |
| `high` | `high` | 高 | High |
| `xhigh` | `xhigh` | 超高 | Extra high |
| `max` | `max` | 最高 | Max |

A model that only switches reasoning off and on offers `off` and `high`; the interface names the second 开启 (On).

- **Resolution.** A requested level the model lacks moves to the nearest one it offers, the stronger one on a tie, and
  never to `off`; `off` on a model that cannot turn reasoning off becomes its weakest level. No level in the chat or the
  profile means the provider's default. A request outside a chat that asks for no reasoning (commit messages, the
  project map, inline completions) gets `off` or the weakest level.
- **Storage.** 3.x and 4.0–4.2 accept only `off`, `low`, `high` and `max` in `settings.json` (`models[].thinking`,
  `selected.thinking`) and in chat files (`model.thinking` of header and `meta` lines). 4.3 writes the nearest of those
  names there (`minimal` → `low`; `medium`, `xhigh` → `high`) and the level itself in a new optional `thinkingLevel`
  when it differs. It reads `thinkingLevel` only while `thinking` still holds its nearest legacy name: an older version
  that changes the level rewrites the object without the field it does not know (serde and 3.x's zod objects drop
  unknown keys), so its choice wins. `xhigh` maps to `high`, not `max`, because older versions send `max` as is and
  most providers refuse it.

## Where the level goes

The protocol and the service decide the request field; the family decides the levels. A profile's reasoning format
(`openai`, `deepseek`, `qwen`) decides only for a local server or a service Rainy does not know, because the form sends
`openai` unless it is changed.

| Service | Chat Completions | Responses | Anthropic Messages |
|---|---|---|---|
| OpenAI, Azure OpenAI, Gemini, xAI, Cerebras, Cloudflare Workers AI, Mistral, local servers (`openai` format) | `reasoning_effort`; `none` is off | `reasoning: {effort, summary: auto}` and `include: [reasoning.encrypted_content]`; off is `{effort: none}` | — |
| DeepSeek, Ant Ling, Kimi K2.6 | `thinking: {type: enabled \| disabled}` and, enabled, `reasoning_effort` | as above | `thinking` and `output_config.effort` (DeepSeek) |
| Zhipu GLM | as DeepSeek, with `clear_thinking: false` (earlier turns' reasoning is sent back) | — | — |
| Kimi K3 | `reasoning_effort` (always thinks) | as above | `output_config.effort` |
| DashScope (Qwen, and the DeepSeek, GLM and Kimi models it hosts) | `enable_thinking` (left out for models that always think), with `reasoning_effort` or `thinking_budget` | as above (no budget field) | — |
| Local servers with the `qwen` format (vLLM, SGLang), NVIDIA NIM | `chat_template_kwargs: {enable_thinking}`, with `reasoning_effort` | as above | — |
| OpenRouter | `reasoning: {effort}`, `{enabled: true}` or `{max_tokens}` | as above | — |
| Together AI | `reasoning: {enabled}`; `reasoning_effort` for gpt-oss | as above | — |
| Anthropic (Claude) | — | — | adaptive `thinking` with `output_config.effort` (per-message effort where offered), or `budget_tokens` |

Where a field cannot carry a model's grades (a thinking budget over Chat Completions outside DashScope and OpenRouter,
or over the Responses API), the model only switches reasoning off and on. Under `reasoning_effort` an ungraded "on"
sends `high`, which llama.cpp, Ollama and Strata read as on.

## Families

Levels are listed with the wire value each sends when it differs from the level's name. "No off" means the model
always reasons and Rainy never sends an off value to it.

### Anthropic Claude (Anthropic Messages)

| Models | Levels | Default | Thinking |
|---|---|---|---|
| Claude Fable 5.1, Mythos 5.1, Opus 5, Sonnet 5.5 | low, medium, high, xhigh, max (no off) | high | adaptive; per-message effort (beta `mid-conversation-output-config-2026-07-01`, with `thinking-binding-controls-2026-08-01`) |
| Claude Opus 5.5, Haiku 5.5 | low … max (no off) | **medium** | as above |
| Claude Fable 5, Mythos 5 | low … max (no off) | high | adaptive, effort in `output_config` (no per-message effort: 400) |
| Claude Sonnet 5 | off, low … max | high | adaptive; off is `thinking: disabled` |
| Claude Opus 4.8, 4.7 | off, low … max | off (no `thinking` means none) | adaptive; off is `disabled` |
| Claude Opus 4.6, Sonnet 4.6 | off, low, medium, high, max (no xhigh) | off | adaptive |
| Claude Opus 4.5, Sonnet 4.5, Haiku 4.5 and older 4.x | off, low 2048, medium 8192, high 16384, max 32768 tokens | off | `thinking: {type: enabled, budget_tokens}` with interleaved thinking |

Decisions: Opus 5.5 rejects `disabled` at every effort. Haiku 5.5 accepts `disabled` up to `high` but then refuses
per-message effort and `block_binding`, and Sonnet 5.5's off form (`between_tools`) refuses per-message effort too;
Opus 5 accepts `disabled` up to `high`, but Anthropic advises lowering effort instead. Rainy keeps per-message effort,
which holds the prompt cache when the level changes, and offers no off on these models. `display: summarized` is sent
because the default (`omitted`) streams empty thinking.

### OpenAI (Chat Completions `reasoning_effort`, Responses `reasoning.effort`)

| Models | Levels | Default |
|---|---|---|
| gpt-6-astra, gpt-6.1-sol | low, medium, high, xhigh, max (no off) | medium |
| gpt-6.1-terra, gpt-6-sol, gpt-6-luna, gpt-5.6 (sol, terra, luna, cyber) | off (`none`), low … max | medium |
| gpt-5.5 | off, low, medium, high, xhigh | medium |
| gpt-5.5-pro | medium, high, xhigh | high |
| gpt-5.4-pro, gpt-5.2-pro | medium, high, xhigh | medium |
| gpt-5.4 (mini, nano), gpt-5.2 | off, low, medium, high, xhigh | off |
| gpt-5.3-codex | low, medium, high, xhigh | medium |
| gpt-5.1 | off, low, medium, high | off |
| gpt-5, gpt-5-mini, gpt-5-nano | minimal, low, medium, high (no off) | medium |
| gpt-5-pro | high only | high |
| o-series, gpt-oss | low, medium, high | medium |
| gpt-4.1, gpt-4o, chat-latest | no reasoning control | — |

Not handled: OpenAI requires the Responses API for tool calls on GPT-6 Astra and 6.1 Sol and allows tools on Chat
Completions only with `none` for GPT-5.6 and GPT-6 Sol and Luna (Azure states this as an error). Rainy's API models use
Responses by default; a profile set to Chat Completions for these models fails on tools.

### DeepSeek

| Models | Levels | Default |
|---|---|---|
| deepseek-flash (V4.1 Flash), deepseek-v4-pro, deepseek-v4-flash, other deepseek ids | off, low, high, max | high |
| deepseek-r1 (other hosts) | always reasons, no control | — |

Chat Completions: `thinking: {type}` and `reasoning_effort` (DeepSeek maps minimal to low, medium and xhigh to high);
Responses: `reasoning.effort` none/low/high/max; Anthropic format: `thinking` and `output_config.effort`. With tools,
every earlier `reasoning_content` must be sent back (Rainy does). The DeepSeek preset keeps Rainy's own default, `max`.

### Qwen

| Models | Levels | Default |
|---|---|---|
| qwen3.8-max, qwen3.8-flash, qwen3.8-27b, qwen3.8-omni-flash | off, low, medium, xhigh | xhigh |
| qwen3.8-2.4t-a95b | low, medium, xhigh (no off) | xhigh |
| qwen3.5 – 3.7 (plus, flash, max, open models), qwen3-* | off, low 4096, medium 16384 tokens (`thinking_budget`), high = the model's own amount | high |
| qwen3.7-max-preview, qwen3.7-max-2026-05-17 | low, medium, high as above (no off) | high |
| qwen-plus, qwen-flash, qwen-turbo | as qwen3.5 | off |
| qwq, qvq | always reason | — |
| qwen3-coder, qwen-max, qwen2.x | no reasoning control | — |

Qwen3.5–3.7 have no named grades; Rainy uses the budgets DashScope gives Qwen3.8's low and medium. DashScope refuses an
effort together with a budget, so one of them is sent. Local servers: llama.cpp reads `reasoning_effort` (`none` turns
thinking off, other values go to the chat template, and Qwen3.8's template refuses `high` and `max`) and
`chat_template_kwargs.enable_thinking`; vLLM and SGLang read `chat_template_kwargs.enable_thinking` (the `qwen`
format); Ollama reads `reasoning_effort` as on or off. Strata (the local engine) reads `reasoning_effort`
(none/off/minimal off, low, medium, high/xhigh/max as xhigh) and not `enable_thinking`, which is why its profile uses
the `openai` format. Over a local server or the Responses API, budget-only models only switch.

### Google Gemini (OpenAI-compatible endpoint, `reasoning_effort`)

| Models | Levels | Default |
|---|---|---|
| gemini-4-argon, gemini-3.8-flash | low, medium, high (no off) | medium |
| gemini-3.6-flash | minimal, low, medium, high (no off) | medium |
| gemini-3.5-flash-lite, gemini-3.1-flash-lite | minimal … high (no off) | minimal |
| gemini-3-flash | minimal … high (no off) | high |
| gemini-3.1-pro and other Gemini 3 | low, medium, high (no off) | high |
| gemini-2.5-pro | low, medium, high (no off) | dynamic; Rainy shows medium |
| gemini-2.5-flash | off (`none`), low, medium, high | dynamic; medium |
| gemini-2.5-flash-lite | off, low, medium, high | off |

Not handled: Gemini 3 and later need the thought signature of each tool call
(`tool_calls[].extra_content.google.thought_signature`) sent back on the next request; Rainy's Chat Completions
reader does not keep it yet.

### Moonshot Kimi

| Models | Levels | Default | Field |
|---|---|---|---|
| kimi-k3, kimi-k3.1-pro | low, high, max (no off) | max | `reasoning_effort`; Responses `reasoning.effort`; Messages `output_config.effort` |
| kimi-k2.6 | off, on | on | `thinking: {type}` |
| kimi-k2.7-code | always reasons | — | — |

### Zhipu GLM (`thinking: {type}`, `clear_thinking: false`)

| Models | Levels | Default |
|---|---|---|
| glm-5.3 (flash, flashx) | low, high, max (no off) | max |
| glm-5.2 | off, high, max | max |
| glm-5.1, glm-5, glm-5-turbo, glm-4.5 – 4.7 | off, on | on |

### xAI Grok (`reasoning_effort`, Responses `reasoning.effort`)

| Models | Levels | Default |
|---|---|---|
| grok-4.7, grok-4.6 | low, medium, high, xhigh (no off) | high |
| grok-4.5 | low, medium, high | high |
| grok-4.3 | off (`none`), low, medium, high, xhigh | low |
| grok-4.20-*-reasoning, grok-build, grok-code-fast | always reason, nothing sent | — |
| *-non-reasoning | no reasoning | — |

Before 4.3 Rainy sent no `reasoning_effort` to xAI, because Grok 4 refused it.

### Other services

| Service | What Rainy sends |
|---|---|
| OpenRouter | The underlying model's levels (by the id after `vendor/`) as `reasoning.effort`; off is `none`, budgets are `reasoning.max_tokens`. Unknown models: off, low, medium, high |
| Together AI | Hybrid models only switch (`reasoning.enabled`); gpt-oss takes `reasoning_effort` low/medium/high |
| Cerebras | `reasoning_effort`; qwen-3.8-27b takes off, low, medium, high (default high) |
| NVIDIA NIM | Nemotron and other models only switch, through `chat_template_kwargs.enable_thinking` |
| Cloudflare Workers AI | `reasoning_effort` with the family's levels |
| Ant Ling | Ling only switches (`thinking: {type}`); Ring always reasons |
| Mistral | mistral-small-latest, mistral-medium-3.5, mistral-large-4: off (`none`) or high; Magistral always reasons |

### Models Rainy does not know

A profile with a reasoning level gets: over Chat Completions `reasoning_effort` off/low/medium/high (default medium), or
off/low/high/max with the `deepseek` format, or an off/on switch with the `qwen` format or on a switch-only service;
over Responses `reasoning.effort` off/low/medium/high; over Anthropic Messages a thinking budget (off, 2048, 8192,
16384, 32768 tokens; default off). A profile without a level gets no reasoning parameter.

## Ambiguities

- **GPT-6 Astra's default** (medium) is stated only by OpenAI staff on the community forum, not in the docs. The
  defaults of gpt-5.2-pro and gpt-oss, and the levels of o3-pro and gpt-5.6-cyber, are not documented; Rainy uses the
  neighbouring models'.
- **DeepSeek** lists `none` as an effort on its API reference but not in its thinking guide, which mentions an `ultra`
  alias of `max` instead; Rainy turns thinking off with `thinking.type` on Chat Completions and `none` on Responses.
  Third-party pages say xhigh maps to max; DeepSeek's own pages say high (Rainy resolves xhigh to max, as for any tie).
- **Qwen3.8**: DashScope's pages disagree on whether `preserve_thinking` defaults on for 3.8 max and flash, and an
  explicit `xhigh` budget (262144) differs from the unset default (131072).
- **GLM**: the thinking guide says GLM-5.x and 4.7 think by default, the API reference that they decide per request;
  z.ai and bigmodel.cn disagree on whether the switch starts at 4.5 or 4.6.
- **Gemini**: the compatibility table has no columns for 3.8 Flash, 3.6 Flash and 3.1 Pro; a Google engineer wrote on
  the forum that `none` gives minimal thinking on 3.6 Flash, while the docs say Gemini 3 cannot turn thinking off.
- **xAI**: grok-4.5's model page lists xhigh, its reasoning guide treats xhigh as high; grok-4.3's page lists five
  levels and the retirement guide four.
- **Mistral**'s default effort is not documented; Rainy assumes high.
- **Claude** facts come from Anthropic's API reference as bundled with Claude Code (cached 2026-10-06), which mirrors
  the effort, adaptive-thinking and migration pages below.

## Sources (read 2026-10-28)

- Anthropic: [Effort](https://platform.claude.com/docs/en/build-with-claude/effort),
  [Adaptive thinking](https://platform.claude.com/docs/en/build-with-claude/adaptive-thinking),
  [Extended thinking](https://platform.claude.com/docs/en/build-with-claude/extended-thinking),
  [Migration guide](https://platform.claude.com/docs/en/about-claude/models/migration-guide),
  [Models overview](https://platform.claude.com/docs/en/about-claude/models/overview).
- OpenAI: [Reasoning models](https://developers.openai.com/api/docs/guides/reasoning),
  [Using GPT-6](https://developers.openai.com/api/docs/guides/latest-model),
  [Create chat completion](https://developers.openai.com/api/reference/resources/chat/subresources/completions/methods/create),
  [Function calling](https://developers.openai.com/api/docs/guides/function-calling),
  [Models](https://developers.openai.com/api/docs/models) and each model's page under it,
  [Deprecations](https://developers.openai.com/api/docs/deprecations), `openai-python`
  `src/openai/types/shared/reasoning_effort.py` (v3.40.0: none, minimal, low, medium, high, xhigh, max),
  [forum: GPT-6 Astra's default](https://community.openai.com/t/what-is-the-default-level-of-reasoning-for-gpt-6-astra/1408343),
  [Azure OpenAI reasoning models](https://learn.microsoft.com/en-us/azure/foundry/openai/how-to/reasoning).
- DeepSeek: [Thinking mode](https://api-docs.deepseek.com/guides/thinking_mode),
  [Create chat completion](https://api-docs.deepseek.com/api/create-chat-completion),
  [Models & pricing](https://api-docs.deepseek.com/quick_start/pricing), [Change log](https://api-docs.deepseek.com/updates),
  [Responses API](https://api-docs.deepseek.com/guides/responses_api),
  [Anthropic API](https://api-docs.deepseek.com/guides/anthropic_api) (English and Chinese).
- Qwen / Alibaba Cloud Model Studio: [深度思考](https://help.aliyun.com/zh/model-studio/deep-thinking),
  [Deep thinking](https://www.alibabacloud.com/help/en/model-studio/deep-thinking),
  [OpenAI 兼容 Chat](https://help.aliyun.com/zh/model-studio/qwen-api-via-openai-chat-completions),
  [OpenAI 兼容 Responses](https://help.aliyun.com/zh/model-studio/compatibility-with-openai-responses-api),
  [文本生成模型](https://help.aliyun.com/zh/model-studio/text-generation-model/),
  [模型价格](https://help.aliyun.com/zh/model-studio/model-pricing); model cards and chat templates of Qwen3-32B,
  Qwen3.5-27B, Qwen3.6-27B and Qwen3.8-27B on Hugging Face.
- Local servers: "Reasoning Outputs" on [docs.vllm.ai](https://docs.vllm.ai) (latest and stable), llama.cpp
  (`github.com/ggml-org/llama.cpp`) `tools/server/README.md` and `common/chat.cpp`, SGLang's "Reasoning Parser" and
  "Thinking Budget" pages, Ollama `docs/api/openai-compatibility.mdx`, Strata (`github.com/Niko1221/Strata`)
  `README.md`, `serve/frontend.py` and `serve/server.py`.
- Google: [OpenAI compatibility](https://ai.google.dev/gemini-api/docs/openai),
  [Gemini thinking](https://ai.google.dev/gemini-api/docs/thinking),
  [Thought signatures](https://ai.google.dev/gemini-api/docs/generate-content/thought-signatures),
  [Gemini 3](https://ai.google.dev/gemini-api/docs/gemini-3), [Models](https://ai.google.dev/gemini-api/docs/models).
- Moonshot: [思考模型](https://platform.kimi.com/docs/guide/use-thinking-models),
  [推理强度](https://platform.kimi.com/docs/guide/use-reasoning-effort.md),
  [Chat Completions](https://platform.kimi.com/docs/api/chat.md), [模型列表](https://platform.kimi.com/docs/models.md),
  [Responses](https://platform.kimi.com/docs/api/responses.md), [Messages](https://platform.kimi.com/docs/api/messages.md).
- Zhipu: [Thinking mode](https://docs.z.ai/guides/capabilities/thinking-mode.md),
  [Chat completion](https://docs.z.ai/api-reference/llm/chat-completion.md), and the same pages on
  [docs.bigmodel.cn](https://docs.bigmodel.cn).
- xAI: [Reasoning](https://docs.x.ai/developers/model-capabilities/text/reasoning),
  [Models](https://docs.x.ai/developers/models) and the grok-4.7, 4.6, 4.5, 4.3 and 4.20 model pages,
  [May 15 retirement](https://docs.x.ai/developers/migration/may-15-retirement).
- OpenRouter: [Reasoning tokens](https://openrouter.ai/docs/guides/best-practices/reasoning-tokens),
  [API parameters](https://openrouter.ai/docs/api_reference/parameters).
- Others: [Together reasoning](https://docs.together.ai/docs/inference/chat/reasoning),
  [Cerebras reasoning](https://inference-docs.cerebras.ai/capabilities/reasoning),
  [NVIDIA NIM reasoning models](https://docs.nvidia.com/nim/large-language-models/1.12.0/reasoning-model.html),
  [Cloudflare OpenAI compatibility](https://developers.cloudflare.com/workers-ai/configuration/open-ai-compatibility/),
  [Ant Ling OpenAI API](https://developer.ant-ling.com/en/docs/api-reference/chat/openai/),
  [Mistral reasoning](https://docs.mistral.ai/studio/conversations/reasoning).
