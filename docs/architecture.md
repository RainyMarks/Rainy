# RainyAgent architecture

RainyAgent is one Windows desktop application built from one package. There is no plugin framework: every part is a plain TypeScript module that `src/host/index.ts`, `src/main/main.ts` or `src/renderer/main.tsx` constructs explicitly. The model layer is [pi-ai](https://www.npmjs.com/package/@earendil-works/pi-ai) and the agent loop is [pi-agent-core](https://www.npmjs.com/package/@earendil-works/pi-agent-core).

## Processes

```text
Electron main (src/main)          window, updates, CTF tools, optional modules, Strata, WSL setup
  │  spawns over stdio (RAINY_CONTROL lines)
  ▼
Host (src/host), one per execution target: Windows Node or Node inside a WSL distro
  │  HTTP + WebSocket on 127.0.0.1:<random port>, launch-token cookie
  ▼
Renderer (src/renderer), loaded by the main window from the Host origin
```

- Electron main spawns the Host with `node host.js` (Windows) or `wsl.exe --distribution <d> --exec env … node host.js` (WSL). The Host prints `RAINY_CONTROL {"type":"ready","protocol":1,"url":…}` on stdout and accepts `stop`, `inspect-activity` and `inspect-project` lines on stdin (`src/main/transport.ts`, `src/host/control.ts`).
- The main window loads `ready.url` (`http://127.0.0.1:<port>/?token=…`). The Host answers with an HttpOnly cookie and redirects to `/`. Every later request and WebSocket upgrade must carry that cookie.
- Main and preload are plain Electron code. Preload exposes the `__RAINY_*` bridges listed in `src/preload/preload.ts`.

## Host

`src/host/index.ts` reads the environment, creates the data directories and constructs these modules in order:

| Module | Owns |
|---|---|
| `server.ts` | HTTP routes, launch-token auth, static files, the `/rpc` WebSocket, the `/rainy/ide/lsp` WebSocket |
| `rpc.ts` | Method table and event broadcast; the method and event types are `src/shared/rpc.ts` |
| `settings.ts` | `settings.json` and `.credentials.json` under the Host home, plus the one-time import of 1.x settings |
| `agent/` | Models, tools, system prompt, sessions, compaction, project memory, skills, MCP |
| `ide/` | File tree, editor state, run/debug, terminals, language servers, formatting |
| `runtime/` | Interpreter discovery and selection, project registry shared with main |
| `icesky/` | The IceSky iframe: static files, model relay, drafts |

### Agent

- `agent/models.ts` stores model profiles. `agent/llm.ts` turns a profile into a pi-ai `Model` and streams it through the matching pi-ai protocol (`openai-completions`, `openai-responses`, `anthropic-messages`).
- `agent/session.ts` wraps one pi-agent-core `Agent` per open chat. Before each run it sets the transcript to a fresh system message (prompt and tool declarations) followed by the compacted conversation from the log. Tools run sequentially.
- `agent/tools/` implements `read`, `write`, `edit` and `bash` (WSL) or `pwsh` (Windows). Names, parameters and result text match RainyAgent 1.x. Tools run without approval prompts. Write and edit require a prior read of an existing file. Large results are cut to a head and tail window, and the full text is saved under the Host temp directory.
- `agent/prompt.ts` assembles the system prompt: persona, @-reference note, tool notes, MCP server instructions, extra project roots, skill descriptors, the user's global prompt and the working-directory line.
- `agent/instructions.ts` injects `AGENTS.md`/`CLAUDE.md` files as a user message before the first request of a session.
- `agent/compaction.ts` replaces an older span of the conversation with a summary when the request estimate reaches the budget threshold, after a context-overflow error, or on `/compact`.
- `agent/memory/` keeps per-project notes, generates them while the app is idle and recalls them once at the start of a session.
- `agent/skills.ts` lists `SKILL.md` files from `<cwd>/.rainy/skills` and `~/.rainy-agent/skills`. `agent/mcp.ts` connects the MCP servers configured in Settings.

### Data under the Host home

The Host home is `RAINY_HOME`: `%APPDATA%\RainyAgent\native-home` for the Windows Host and `~/.rainy-agent` inside WSL.

| Path | Content |
|---|---|
| `settings.json` | Models (without keys), selected model, global prompt, MCP servers, UI preferences |
| `.credentials.json` | API keys, mode 0600 |
| `chats/<sessionId>.jsonl` | One chat: a header line, then transcript entries in order |
| `chats/index.json` | Chat summaries for the history list; rebuilt from the files when missing |
| `projects.json` | Projects (workspaces) and their roots |
| `ide/<workspaceId>.json` | Editor tabs, dirty buffers, layout and run configurations |
| `runtime-environments.json` | Interpreter selections per project |
| `<carrier-state>/project-memory/<projectId>/memory.v1.json` | Project memory, shared by the Windows and WSL Hosts |

Chats from RainyAgent 1.x use the DeepSeek Harness session format and are not imported. Models, API keys and the global prompt are imported once from the 1.x profile.

### Transcript entries

A chat file is JSON Lines. The first line is `{"type":"header","version":1,…}`. Each later line is either `{"type":"entry","entry":…}` with one `TranscriptEntry` from `src/shared/rpc.ts` (`user`, `assistant`, `toolResult`, `context`, `compaction`, `notice` or `turn`) or `{"type":"meta",…}` with a title, model, archive or pin change. The model context is rebuilt from the entries: a `compaction` entry replaces the entries it covers with its summary, and `notice` and `turn` entries never reach the model. The renderer shows every entry.

## Renderer

`src/renderer` is one React application. `rpc.ts` connects to `/rpc` and reconnects after the Host restarts.

| Folder | Content |
|---|---|
| `app/` | Window layout: top bar, file pane, editor area, AI pane, bottom panel, status bar |
| `chat/` | Transcript, markdown, tool cards, composer, model picker, context meter, history list |
| `ide/` | Editor tabs, editor mount, quick open, run configurations, terminal, problems, debug |
| `ctf/` | Native tool catalog and the IceSky iframe |
| `settings/` | General, Models, Skills & MCP, Runtime and Memory sections |
| `ui/` | Buttons, menus, dialogs, toasts, icons |

Each folder keeps its Chinese and English strings in a `messages.ts` built with `defineMessages` from `i18n.ts`.

### Editor

`src/editor` implements the `EditorAssets` interface of `src/renderer/ide/editor-types.ts` with [CodeMirror 6](https://codemirror.net/) and the xterm.js terminal. The IDE loads it with a dynamic import, so Vite emits it as a separate chunk the first editor or terminal visit fetches.

- Each open document has its own `EditorView`, so undo history, scroll position and the language-server session survive tab switches. The IDE model owns buffer text and saves; the editor reports edits and never writes files.
- `lsp.ts` opens one `@codemirror/lsp-client` connection per project root and server language over the Host's `/rainy/ide/lsp` WebSocket (pyright, typescript-language-server, clangd). It answers the server-to-client requests the client library does not, routes diagnostics to the Problems panel and sanitizes documentation HTML before display.
- Rename edits every file the server names; files without a view are opened as unsaved buffers through `prepareEdit`.
- A comparison (`showDiff`) is `@codemirror/merge`'s unified view on the document's own view.
- Language support loads per file type on first use (`languages.ts`).

## Build and packaging

`pnpm run build` (`scripts/build.ts`) produces:

- `dist/main.cjs`, `dist/preload.cjs`, `dist/setup/`: the Electron shell
- `dist/host.js`: the Host bundle; native and process-launched packages stay in `node_modules`
- `dist/renderer/`: the Vite build, including the editor chunk and `THIRD_PARTY_NOTICES.txt` with the license texts of every bundled npm package

`scripts/stage-windows.mjs` assembles the Windows Host runtime and `scripts/stage-linux.py` the WSL runtime. `electron-builder.config.cjs` packages the NSIS installer.
