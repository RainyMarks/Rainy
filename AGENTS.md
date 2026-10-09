# AGENTS.md

RainyAgent is a Windows desktop AI coding workbench for CTF work: an Electron shell, a Node Host per execution target (Windows or WSL) and one React page. It is a single package with no plugin framework. Read [docs/architecture.md](docs/architecture.md) before changing the Host, the RPC protocol or persisted files.

## Layout

```
src/main/       Electron main: window, updates, CTF tool packs, optional modules, Strata, WSL setup
src/preload/    the __RAINY_* bridges the page may call
src/host/       the Host: HTTP/WebSocket server, RPC, settings, projects, activity
  agent/        models (pi-ai), sessions (pi-agent-core), tools, prompt, compaction, memory, skills, MCP
  ide/          files, editor state, run/debug, terminals, language servers, formatting
  runtime/      interpreter discovery and selection
  icesky/       the IceSky iframe routes
src/shared/     types and code both sides use; src/shared/rpc.ts is the protocol between Host and page
src/renderer/   React page: app/ (layout), chat/, ide/, ctf/, settings/, ui/ (primitives), theme/
src/editor/     CodeMirror 6 editor and xterm terminal behind src/renderer/ide/editor-types.ts
src/setup/      static setup pages shown by main
tests/          host/, renderer/, main/, scripts/, icesky/ (node:test), manual/ (run by hand)
scripts/        build, dev, staging, packaging and release tooling
resources/      IceSky, icons and tool catalogs shipped with the app
toolpacks/      native tool channel inputs
```

## Commands

```sh
pnpm install
pnpm run typecheck      # node side and renderer side
pnpm exec vitest run    # unit and component tests (tests/**/*.test.ts[x])
pnpm run test:node      # node:test suites: tests/scripts and tests/icesky
pnpm run build          # dist/main.cjs, dist/preload.cjs, dist/host.js, dist/renderer
pnpm run dev            # build, then run the Host headless and print its URL (open it in a browser)
pnpm run start          # Electron with the built files
```

Before pushing, run `typecheck`, the vitest files for what you changed, `test:node` when scripts or IceSky changed, and `build` when the build, Vite config or dependencies changed. CI (`.github/workflows/rainy.yml`) runs all of them on Ubuntu and Windows.

## Conventions

- ESM and strict TypeScript everywhere. Relative imports carry the `.ts`/`.tsx` extension. Explain every `any`.
- The Host and the page talk only through `src/shared/rpc.ts`: add a method to `HostMethods` or an event to `HostEvents`, register it in the Host, call it with `host.call` / `host.on` in the page. Validate RPC input on the Host; trust types inside one process.
- Persisted files have versions: the chat format (`CHAT_FORMAT_VERSION`), `settings.json`, project memory (`memory.v1.json`), IDE state. Change a format only with a reader for the previous version.
- Anything the model sees must be reconstructable from the chat file. New model-visible input needs a transcript entry kind or a `context` entry.
- Page text is localized: each folder's `messages.ts` defines Chinese and English with `defineMessages`. No hard-coded copy in components.
- Colors, radii and shadows come from the tokens in `src/renderer/theme/` (`--dsw-*`). Dark mode is `body[data-ds-dark-theme]`; do not hard-code colors.
- Text from projects, tools or language servers is untrusted in a CTF workbench. Never render it as HTML without sanitizing; the page has privileged bridges.
- Tools run without approval prompts. Keep tools inside their documented behavior: `read` before `write`/`edit`, output limits, spill files.
- Pin exact dependency versions that are at least two weeks old. CodeMirror core packages are pinned once in `pnpm-workspace.yaml` overrides so only one copy loads.
- Comments state contracts and non-obvious reasons, not narration. Files end with one newline.
- Docs change with the code: `docs/architecture.md` for structure, `docs/desktop*.md` for build and release, README for users.

## Tests

- Host behavior: `tests/host` with real files in temp directories and `FakeOpenAI` (`tests/host/agent-fixtures.ts`) for model traffic.
- Page components: `tests/renderer` with happy-dom; `settings-harness.tsx` provides a fake Host, `render`, `click`, `type` and `waitFor`.
- Changes visible in the window deserve a check in a real browser: `pnpm run dev`, then open the printed URL with Playwright (Chromium is preinstalled in cloud sessions).

## Secrets

Real model calls read keys from Settings or the environment (`DEEPSEEK_API_KEY`, `ANTHROPIC_API_KEY`). Never commit keys. `RAINY_CONFIGURE_DEEPSEEK=1` makes a Host save and select the DeepSeek preset at startup.
