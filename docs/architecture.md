# Rainy architecture

Rainy is one Windows desktop application built from one package. There is no plugin framework: every part is a plain TypeScript module that `src/host/index.ts`, `src/main/main.ts` or `src/renderer/main.tsx` constructs explicitly. The model layer is [pi-ai](https://www.npmjs.com/package/@earendil-works/pi-ai) and the agent loop is [pi-agent-core](https://www.npmjs.com/package/@earendil-works/pi-agent-core).

## Processes

```text
Electron main (src/main)          window, updates, optional modules, Strata, WSL setup, SSH targets
  │  spawns over stdio (RAINY_CONTROL lines)
  ▼
Host (src/host), one per execution target: Windows Node, Node inside a WSL distro, or Node on a Linux server over SSH
  │  HTTP + WebSocket on 127.0.0.1:<random port>, launch-token cookie
  ▼
Renderer (src/renderer), loaded by the main window from the Host origin
```

- Electron main spawns the Host with `node host.js` (Windows), `wsl.exe --distribution <d> --exec env … node host.js` (WSL) or `ssh.exe … -- <destination> …` (SSH, see below). The Host prints `RAINY_CONTROL {"type":"ready","protocol":1,"url":…}` on stdout and accepts `stop`, `inspect-activity` and `inspect-project` lines on stdin (`src/main/transport.ts`, `src/host/control.ts`). The Host's own requests go the other way and main answers each with a `reply` line: every Host asks `credential-key` once at start, and the Windows Host asks `system-proxy {id, url}` for each origin it connects to. Main resolves that URL with Electron's `session.resolveProxy`, which applies the Windows proxy settings, bypass list and PAC scripts (`src/main/system-proxy.ts`).
- The main window loads `ready.url` (`http://127.0.0.1:<port>/?token=…`). The Host answers with an HttpOnly cookie and redirects to `/`. Every later request and WebSocket upgrade must carry that cookie.
- Main and preload are plain Electron code. Preload exposes the `__RAINY_*` bridges listed in `src/preload/preload.ts`.
- Errors, dialogs and menus written by main or the Host use `localized({ zh, en })` from `src/shared/locale.ts` in the language chosen in Settings → General: the Host reads it from `settings.json`, main from the page and, before the page loads, from `desktop.json`.
- Main starts Electron's crash reporter without uploads; `src/main/diagnostics.ts` packs versions, the scrubbed end of the logs and the newest crash dumps into the zip that Settings → General exports.

### SSH targets

An execution target has the kind `windows`, `wsl` or `ssh` (`src/main/execution-targets.ts`). SSH targets come from the aliases in `~/.ssh/config` and from the hosts added in Settings, which are saved as `sshHosts` in `desktop.json`; `RAINY_SSH_CONFIG` points at another config file for development and tests. Switching target restarts the app into the new target.

- `src/main/ssh.ts` validates destinations (`[user@]host` or an alias, never starting with `-`), builds the `ssh` arguments with `--` before the destination, and runs Python on the server through a `python3 -c` loader that receives the script and its arguments as base64, so any login shell (sh, bash, zsh, fish or csh) passes them unchanged. It also turns ssh failures into messages. The client is the Windows OpenSSH `C:\Windows\System32\OpenSSH\ssh.exe`, so the user's config, keys, ssh-agent and ProxyJump apply.
- `src/main/ssh-askpass.ts` answers ssh's prompts: ssh runs the bundled `windows-host` `node.exe` as `SSH_ASKPASS`, with `NODE_OPTIONS` requiring a small client that forwards the prompt over a named pipe to main. `src/main/ssh-prompt.ts` shows the host-key confirmation and the password, passphrase and one-time-code dialogs; answers are kept in memory for the app run only.
- First, main installs this version's Linux runtime in `~/.rainy-agent/runtime/<sha256>` on the server with `scripts/install-runtime.py`. `--fetch METADATA PIECES` makes the server download the release pieces itself and verify each one; when that fails or stays below 256 KiB/s after a minute, main downloads the archive and streams it to `--receive METADATA`. Both check the archive against the SHA-256 pinned in the signed release resources. Runtimes of earlier versions are pruned in the background.
- `SshHostTransport` in `src/main/transport.ts` starts the Host through ssh with `RAINY_PORT` set and `ssh -L` forwarding the same port from 127.0.0.1 on the PC to 127.0.0.1 on the server. It checks that the Host reports the forwarded port and waits for Windows loopback reachability, as for WSL. The Host home is `~/.rainy-agent` on the server, and it keeps its own `carrier-state`, so projects and project memory stay on the server. API keys are sealed there under the key main supplies at each start.

## Host

`src/host/index.ts` reads the environment, creates the data directories and constructs these modules in order:

| Module | Owns |
|---|---|
| `server.ts` | HTTP routes, launch-token auth, static files, the `/rpc` WebSocket, the `/rainy/ide/lsp` WebSocket |
| `rpc.ts` | Method table and event broadcast; the method and event types are `src/shared/rpc.ts` |
| `settings.ts` | `settings.json` and the API keys (`credentials.ts`) under the Host home, plus the one-time import of 1.x settings |
| `network.ts` | The proxy for model requests: follow the system (default), none, or a manual HTTP proxy. Model streaming, model discovery and MCP servers over HTTP fetch through it with undici's `ProxyAgent`, which forwards plain-HTTP requests and tunnels HTTPS; loopback, private, `.local` and single-label hosts stay direct. Following the system, the Windows Host asks main per origin (answers kept 30 seconds) and the WSL Host reads `HTTPS_PROXY`, `HTTP_PROXY` and `NO_PROXY`. Commands and terminals are not proxied |
| `agent/` | Models, tools, system prompt, sessions, compaction, project memory, skills, MCP |
| `ide/` | File tree, editor state, run/debug, terminals, language servers, formatting, the `/rainy/ide/raw` byte route, change hints |
| `map/` | Project maps: the file list, line counts, imports, stack and the AI explanation |
| `search/` | Text search across a project's roots with ripgrep, and replacement on disk (`search.text`, `search.replace`) |
| `git/` | The Git pane: status, staging, discarding, committing and the AI commit message (`git.*`) |
| `runtime/` | Interpreter discovery and selection, project registry shared with main |
| `pdfjs.ts` | Serves the pdf.js build in `resources/pdfjs` at `/rainy/pdfjs/` for the PDF viewer |

### Agent

- `agent/models.ts` stores model profiles. `agent/llm.ts` turns a profile into a pi-ai `Model` and streams it through the matching pi-ai protocol (`openai-completions`, `openai-responses`, `anthropic-messages`).
- `agent/session.ts` wraps one pi-agent-core `Agent` per open chat. Before each run it sets the transcript to a fresh system message (prompt and tool declarations) followed by the compacted conversation from the log. Tools run sequentially. It records how long each thinking block streamed (`thinkingMs` on the assistant entry, by content index).
- `agent/retry.ts` decides which failed requests a run retries by itself: overload, rate limits, dropped connections and timeouts, not authentication, quota or malformed requests (pi-ai's provider retries have already given up by then). A run retries at most three times, after 5, 10 and 20 s with ±15% jitter or the delay the provider asks for (up to 2 minutes), and publishes the wait as `retry` in `session.state` so the page can count down. `chat.retry` ends that wait early, or, when the chat is idle and its last model-visible entry is a failed or stopped reply, starts a run that answers the history as it stands without a new user message. Each failed reply stays in the transcript; an `auto-retry` notice marks the retries.
- `agent/tools/` implements `read`, `grep` and `glob` (ripgrep), `write`, `edit` and `bash` (WSL) or `pwsh` (Windows). Write and edit require a prior read of an existing file, and keep the replaced content under `snapshots/<sessionId>/` (files up to 16 MiB) so the user can undo the change; an undo is refused when the file changed since, and a `context` entry with `reverted` tells the model. Large results are cut to a head and tail window, and the full text is saved under the Host temp directory.
- The agent's changes are reviewed per chat. `chat.changes` lists the files the chat's write and edit calls changed that are neither kept nor undone, counting lines against the text before the first change (Myers' diff in `shared/line-diff.ts`; binary files and files over 4 MB are not compared) and leaving out a file changed back. `chat.changeBase` returns a file's text before the first change and now, `chat.keep` records reviewed calls as a `review` line in the chat file, and `session.changes` tells the page to refresh. With `prefs.autoKeepChanges` (on by default) a user message that starts a run first keeps every pending change; a retry or a steering message does not.
- `agent/permissions.ts` decides every tool call through pi-agent-core's `beforeToolCall` by the chat's mode. `read-only` refuses changes, commands and MCP tools not marked `readOnlyHint`; `ask` (the default) runs reads and writes inside the project roots but waits for the user on commands, other MCP tools and writes outside the project (links resolved first); `auto` runs everything. A waiting call appears in `SessionSnapshot.approvals` and is answered with `chat.approve`. "Always allow" stores a command prefix or MCP tool per project in `settings.json`; prefixes match only commands without chaining, redirection, substitution, wrappers like `sudo` or inline code like `python -c`. A refusal without a reason ends the run after the current step; a new message refuses the waiting call and is steered to the model.
- `agent/prompt.ts` assembles the system prompt: persona, @-reference note, tool notes, MCP server instructions, extra project roots, skill descriptors, the user's global prompt and the working-directory line.
- `agent/instructions.ts` injects `AGENTS.md`/`CLAUDE.md` files as a user message before the first request of a session.
- `agent/compaction.ts` replaces an older span of the conversation with a summary when the request estimate reaches the budget threshold, after a context-overflow error, or on `/compact`. While the chat's previous request started less than 54 minutes ago with the same model and reasoning level (90% of the hour Claude keeps its cache), the summary request repeats exactly what the next request would send (system prompt, tools, full history, reasoning and stream options, no `tool_choice` or `max_tokens` change) and appends one instruction, so it reads the prompt cache that request left; on Claude it marks the previous request's last user message and writes no cache of its own. A reply that calls a tool, stops early, is empty or is far over the size limit is discarded. Then, and when the cache is cold or the full history would not fit, a separate request summarizes only the span with tool calls refused and no cache written. The compaction entry records which request produced the summary and the usage of both, and an automatic compaction's usage counts toward its turn. `llm.ts` sends Claude message text as content blocks so that moving a breakpoint never changes a message's form.
- `agent/memory/` keeps per-project notes, generates them while the app is idle and recalls them once at the start of a session. Generation costs model requests, so it is off for projects without a memory record; records written before 2.1 keep their setting.
- `agent/inline.ts` serves the editor's AI. `inline.complete` asks for a suggestion at the cursor (`prefs.completionModel`, or the chat model, with reasoning off, 192 output tokens and a 15 s limit); a new request ends the previous one, and nothing is sent while `prefs.inlineCompletion` is off. `inline.edit` streams a rewrite of the marked selection with the chat model, reports the text so far as `inline.progress` events, and stops on `inline.cancel`. Replies are cleaned to what fits: code fences, a repeated line start and, in the middle of a line, anything past one line or already after the cursor are dropped.
- `agent/skills.ts` lists `SKILL.md` files from `<cwd>/.rainy/skills` and `~/.rainy-agent/skills`. `agent/mcp.ts` connects the MCP servers configured in Settings.

### Project map

- `map/scan.ts` lists a project's files with ripgrep (`.gitignore` respected, hidden files included; at most 20,000 files, of which 8,000 and 96 MB are read, within 15 s), counts lines and marks vendored, generated and minified files (by folder name, `.gitattributes` `linguist-vendored` and `linguist-generated`, or very long lines) so they do not count as code.
- `map/imports.ts` follows imports in JavaScript and TypeScript (with `tsconfig` paths), Python, C and C++, Go, Rust, Java and Kotlin, PHP, Ruby, Lua, shell, PowerShell, HTML and CSS. `map/stack.ts` names languages, frameworks and tools from manifests and imports.
- `map.get` keeps a scan for 20 seconds and runs one scan per project at a time. `shared/map-graph.ts` turns the files into at most 16 boxes per view: a directory holding over 35% of the code is split, and the boxes are linked by their imports. `map.explain` asks the chat model for a summary and a sentence per part, and stores it under `maps/`.

### Search and Git

- `search.text` runs ripgrep with `--json` in every root (`.gitignore` respected, hidden files included, `.git`, `node_modules`, `.venv` and `__pycache__` left out, files over 8 MB skipped) and returns at most 5000 matches with UTF-16 columns; regular expressions use `--engine auto`, so look-arounds fall back to PCRE2. A newer search of a project ends the older one. `shared/text-search.ts` holds the replacement both sides apply line by line, so a pattern never spans lines, as in the search: `search.replace` rewrites UTF-8 files on disk (keeping a BOM and line endings) and the page changes files with unsaved edits in their buffers.
- `git/` runs the `git` command with the runtime overlay, so the MinGit of the Windows basic component serves when Git is not installed (the editor's compare with HEAD does the same). The status comes from `git status --porcelain=v2 -z --branch --untracked-files=all -- .` with paths made relative to the root. Discarding deletes untracked files with `git clean` and restores tracked ones from the index. `git.message` sends the staged diff (or every change when nothing is staged, cut to 24,000 characters) and the last twelve commit subjects to the chat model.
- On Windows a process tree is not stopped again once its root has exited, because the process id is gone or names another program.

### Data under the Host home

The Host home is `RAINY_HOME`: `%APPDATA%\RainyAgent\native-home` for the Windows Host, `~/.rainy-agent` inside WSL and `~/.rainy-agent` on an SSH server.

| Path | Content |
|---|---|
| `settings.json` | Models (without keys), selected model, global prompt, MCP servers (with their environment and request headers), UI preferences, network proxy, default permission mode and remembered approvals |
| `.credentials.sealed.json` | API keys, each encrypted with AES-256-GCM and bound to its name, under a key the carrier keeps in `credential-key.bin` in the Electron user data, protected by Windows DPAPI (`src/main/credential-key.ts`) |
| `.credentials.json` | Version 1 key file, empty once its keys are sealed, so an older version starts without keys instead of failing; keys found there are sealed at the next start. Development runs without the carrier keep keys here as plain text |
| `chats/<sessionId>.jsonl` | One chat: a header line, then transcript entries in order |
| `chats/index.json` | Chat summaries for the history list; rebuilt from the files when missing |
| `maps/<workspaceId>.json` | The AI explanation of a project map |
| `snapshots/<sessionId>/` | Content replaced by the agent's write and edit calls, for undo; removed with the chat and after 30 days |
| `projects.json` | Projects (workspaces) and their roots |
| `ide/<workspaceId>.json` | Editor tabs, dirty buffers, layout and run configurations |
| `runtime-environments.json` | Interpreter selections per project |
| `<carrier-state>/project-memory/<projectId>/memory.v1.json` | Project memory, shared by the Windows and WSL Hosts; an SSH Host keeps its own under its home |

Chats from RainyAgent 1.x use an older session format and are not imported. Models, API keys and the global prompt are imported once from the 1.x profile.

### Transcript entries

A chat file is JSON Lines. The first line is `{"type":"header","version":1,…}`. Each later line is either `{"type":"entry","entry":…}` with one `TranscriptEntry` from `src/shared/rpc.ts` (`user`, `assistant`, `toolResult`, `context`, `compaction`, `notice` or `turn`) `{"type":"meta",…}` with a title, model, archive or pin change, or `{"type":"review","kept":[…]}` with the tool calls whose changes the user kept (readers before 2.4 skip it). The model context is rebuilt from the entries: a `compaction` entry replaces the entries it covers with its summary, and `notice` and `turn` entries never reach the model. The renderer shows every entry.

## Renderer

`src/renderer` is one React application. `rpc.ts` connects to `/rpc` and reconnects after the Host restarts.

- The page does not poll for changes it can be told about. `ide/hints.ts` in the Host watches the directories of files open in editor tabs (with `fs.watch`, up to 64 per project folder, including the folder of a deleted file) and run and debug events, and sends a throttled `ide.hint` event; the page then fetches the state with its usual requests, and checks open files at once when tabs open or close. The fallback polls (`editorPollMs`, 10 s; `executionPollMs`, 1 s during a run) cover lost hints and unwatchable folders, and stop while the window is hidden. Watchers close when the page has not checked a folder's files for 30 seconds (three fallback polls when that is longer), because Windows cannot rename a watched folder.
- KaTeX and its stylesheet load only for a message that contains `$`; highlight.js grammars load per language on first use (`ui/highlight.ts`).
- Interface preferences live in the Host's `settings.json` (`prefs.get`, `prefs.set`) and `prefs.ts` applies them. A color scheme replaces the gray ramp in `theme/schemes.css` and an accent its blue ramp, through the `data-color-scheme` and `data-accent` attributes of `<body>`; chosen fonts go in front of the font stacks in `theme/base.css`. The editor and terminal get the code font and surface colors through `EditorAppearance`.
- Empty views (no project, no open file, a new chat) draw rain behind their content with `app/RainField.tsx`: a canvas in the accent color that runs the simulation in `app/rain.ts` (streaks at the 19.5° slant of the Rainy mark, rings where drops land, a ripple where the user clicks). It pauses while the window is hidden, draws one still frame under `prefers-reduced-motion`, and is off when `prefs.rainEffects` is. The mark itself is inline SVG (`ui/RainyMark.tsx`) on the accent ramp; its bars fall when pointed at and keep falling in the top bar while the current chat works.

| Folder | Content |
|---|---|
| `app/` | Window layout: top bar, side pane with files, search and Git tabs, editor area, AI pane, bottom panel, status bar; the command palette (`CommandPalette.tsx`, commands ranked in `commands.ts`) |
| `chat/` | Transcript, markdown, tool cards, composer, model picker, context meter, history list |
| `ide/` | Editor tabs, editor mount, file viewers, quick open, run configurations, terminal, problems, debug |
| `map/` | Project map: the architecture graph (layered layout in `layout.ts`), file blocks (squarified treemap in `treemap.ts`), details, and the agent's activity from `session.tool` and streamed tool calls (`activity.ts`) |
| `review/` | The changed-files list above the composer, the review bar and the review actions |
| `search/` | The search pane |
| `git/` | The Git pane and the shared status store behind its tab badge and the status-bar branch (polled every 3 s while the pane shows, 15 s otherwise, and after file hints) |
| `settings/` | General, Appearance, Models, Skills & MCP, Runtime, Memory and Security sections |
| `ui/` | Buttons, menus, dialogs, toasts, icons |

Each folder keeps its Chinese and English strings in a `messages.ts` built with `defineMessages` from `i18n.ts`.

### Editor

`src/editor` implements the `EditorAssets` interface of `src/renderer/ide/editor-types.ts` with [CodeMirror 6](https://codemirror.net/) and the xterm.js terminal. The IDE loads it with a dynamic import, so Vite emits it as a separate chunk the first editor or terminal visit fetches.

- Each open document has its own `EditorView`, so undo history, scroll position and the language-server session survive tab switches. The IDE model owns buffer text and saves; the editor reports edits and never writes files.
- `lsp.ts` opens one `@codemirror/lsp-client` connection per project root and server language over the Host's `/rainy/ide/lsp` WebSocket (pyright, typescript-language-server, clangd). It answers the server-to-client requests the client library does not, routes diagnostics to the Problems panel and sanitizes documentation HTML before display.
- Rename edits every file the server names; files without a view are opened as unsaved buffers through `prepareEdit`.
- A comparison (`showDiff`) is `@codemirror/merge`'s unified view on the document's own view. Reviewing the agent's changes adds Keep and Undo to each changed chunk and reports the chunks left to the IDE, which saves what was undone and keeps the rest.
- `ai.ts` adds the AI features. A suggestion is requested 450 ms after typing when the rest of the line is only closing brackets or punctuation, shows as a widget after the cursor, keeps its remainder while typing matches it, and is accepted with Tab or word by word with Ctrl+Right. Ctrl+K (the context menu in Markdown and SVG, where Ctrl+K starts the preview chord) opens a prompt above the selected lines. The rewrite replaces the lines as it streams; while it is reviewed, a line diff against the original marks only the changed lines, with the lines they replaced shown above them, until Ctrl+Enter keeps it or Esc restores the original. Decorations come from a state field because block widgets cannot come from a view plugin.
- Language support loads per file type on first use (`languages.ts`).

### File viewers

`src/renderer/ide/viewers` shows what the text editor cannot. Vite emits it as a separate chunk that loads with the first tab needing a viewer. `viewer-choice.ts` picks the viewer from the file extension, the Host's read result and the tab's saved view (`source`, `preview`, `split` or `hex`).

- The Host does not decode images, audio, video or PDF: `files.read` returns `readOnlyReason: 'media'`, and the page loads the bytes from `/rainy/ide/raw` (`src/host/ide/raw.ts`). The route applies the same path checks as every file operation and refuses a file that changed between the check and the open. It answers single `Range` requests and sends `Content-Security-Policy: sandbox`, so a file opened there runs no script; the window also refuses to navigate to it. The URL carries the observed version, so a changed file gets a new URL.
- SVG is drawn through `<img>`, where its scripts do not run. Markdown is rendered by the chat renderer (`chat/Markdown.tsx`) relative to the document's directory: local images come from the byte route, local links open in the editor, and remote images load only in the system browser.
- PDF uses the pdf.js build in `resources/pdfjs`, which the Host serves at `/rainy/pdfjs/` (`src/host/pdfjs.ts`), with `isEvalSupported: false` (CVE-2024-4367).
- The hex view reads 64 KiB chunks on demand, so it opens files of any size.
- Text that is not UTF-8 (GB18030, UTF-16, Big5 and others) is decoded by the page and shown read-only.

## Build and packaging

`pnpm run build` (`scripts/build.ts`) produces:

- `dist/main.cjs`, `dist/preload.cjs`, `dist/setup/`: the Electron shell
- `dist/host.js`: the Host bundle; native and process-launched packages stay in `node_modules`
- `dist/renderer/`: the Vite build, including the editor chunk and `THIRD_PARTY_NOTICES.txt` with the license texts of every bundled npm package

`scripts/stage-windows.mjs` assembles the Windows Host runtime and `scripts/stage-linux.py` the Linux runtime that WSL and SSH targets share. `electron-builder.config.cjs` packages the NSIS installer.
