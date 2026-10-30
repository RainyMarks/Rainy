# Per-model agent adaptation

Each model family is tuned by its vendor against that vendor's own agent: Claude against Claude Code, GPT against
OpenAI Codex, Gemini against Gemini CLI, and several Chinese and open-weight models against Claude Code's tool
protocol. Rainy's agent gives each family the system prompt guidance, edit tool and plan tool of that agent, so a model
works in Rainy close to how it works at home. The family comes from the model id (`native/crates/host/src/agent/profile.rs`);
nothing has to be configured. Sources were read on **2026-10-29**; re-check them before changing a family.

| Family | Model ids | Edits files with | Plan tool | Guidance after |
|---|---|---|---|---|
| Claude | `claude-*` (also Bedrock, Vertex and router spellings) | `edit`, `write` | `todo_write` | Anthropic's prompting guides, per model |
| GPT | `gpt-5*` (not `gpt-5-chat`), `gpt-6*`, o-series, `*codex*` | `apply_patch` | `update_plan` | Codex model instructions |
| Gemini | `gemini-*`, `gemma-*` | `edit`, `write` | `write_todos` | Gemini CLI core mandates |
| Claude Code–compatible | `deepseek*`, `kimi*`, `moonshot*`, `glm*`, `qwen*`, `qwq*`, `minimax*`, `doubao*` and similar | `edit`, `write` | `todo_write` | Claude guidance plus their own agents' reminders |
| Other | anything else (`gpt-4*`, Llama, Mistral…) | `edit`, `write` | `todo_write` | Claude guidance |

Every prompt opens with the same identity line (Rainy, a desktop agent for coding, research and office work, replying
in the user's language, whose user does not see tool output) and keeps the shared sections after the guidance: file
references, tool notes, MCP instructions, extra project folders, skills, the user's global prompt and the shell line.
A chat's prompt and tools stay byte-identical between its requests, so provider caches stay warm; when the user
switches to a model of another family mid-run, the next request carries that family's tools and prompt.

## Claude

From Anthropic's [prompting best practices](https://platform.claude.com/docs/en/build-with-claude/prompt-engineering/claude-prompting-best-practices)
and the per-model pages for Fable 5.1, Opus 5.5, Sonnet 5.5 and Haiku 5.5:

- Default to action for requested changes, but report and stop when the user describes a problem or asks a question.
- Read before changing; keep to the scope of the request; mention rather than fix unrelated problems; surgical edits.
- A real check (tests, type-checker, build or the changed command) before reporting a change done.
- Parallel tool calls for independent work; a line before starting and a recap that stands on its own at the end.
- Sonnet 5.5 and Haiku 5.5 add the "keep working until everything is done" paragraph their pages recommend against
  early stopping.
- Fable 5.1 adds the targeted-edit and search-before-answering paragraphs, and every round of tool results carries
  Anthropic's batching nudge ("First privately list what you need next; then request every item that doesn't depend on
  another's result in this one response.") as a text block after the `tool_result` blocks. It is sent in every request
  alike, so the history stays append-only and thinking blocks stay valid.
- Thinking is requested with `display: "summarized"`, so the progress notes Opus 5.5 and Sonnet 5.5 write between tool
  calls reach the chat.

## GPT

From the model instructions in Codex's `models.json` (`codex-rs`, Apache-2.0): a senior engineer's judgment, the
repository's own patterns, scoped edits, `apply_patch` for manual edits, never reverting the user's changes, a
code-review stance for reviews, autonomy and persistence to the end of the task, short updates while working, plans
kept with `update_plan`, and Codex's final-answer formatting rules. Codex's personality paragraphs, frontend design
rules and its own tool names (`exec_command`, `multi_tool_use.parallel`) are left out.

`apply_patch` takes Codex's patch format (`*** Begin Patch`, `*** Add File:`, `*** Update File:` with `*** Move to:`,
`*** Delete File:`, `@@` context lines, `*** End of File`, `*** End Patch`) and matches context like Codex: exactly,
then ignoring trailing whitespace, then surrounding whitespace, then typographic dashes, quotes and spaces. Rainy
checks every hunk before writing anything, so a patch that does not apply changes nothing; it keeps each file's text
encoding (GB18030 included) and line endings, records an undo snapshot per file (`<call id>#<n>`, deletions and moves
too), asks before writing outside the project like `write`, and shows a diff per file in the chat. On OpenAI's own
Responses API (`api.openai.com`) the tool is a freeform custom tool constrained by Codex's Lark grammar, as in Codex;
elsewhere it is a JSON function with one `input` string.

## Gemini

From Gemini CLI's core mandates (`packages/core/src/prompts`, Apache-2.0): follow the project's conventions, never
assume a library is available, tell directives from inquiries, persist to a verified result and update related tests,
keep type safety and secrets, explain state-changing commands, and Gemini CLI's context-efficiency rules (fewer turns,
parallel reads, grep with context). The plan tool is Gemini CLI's `write_todos`, which can also cancel an item.

## Claude Code–compatible models

DeepSeek, Moonshot (Kimi), Zhipu (GLM), Alibaba (Qwen) and MiniMax document Claude Code as a supported agent for their
coding models, so these models get Claude Code's tools and the Claude guidance. Kimi CLI's and Qwen Code's prompts add
two reminders these models need more than Claude: changes count only when made with tools, and a failed check is fixed
and run again rather than reported.

## Plan tools

`todo_write` (`todos: [{content, status, activeForm?}]`), `update_plan` (`explanation?`, `plan: [{step, status}]`) and
`write_todos` (`todos: [{description, status}]`) are one tool under three names; each call replaces the list, runs in
every permission mode and shows in the chat as a checklist card that opens by default, titled with the progress.

## Tool calls

A call whose tool name differs from a declared tool only in letter case runs that tool (Sonnet 5.5's page recommends
accepting such calls); any other unknown name is answered with the list of available tools.
