# AGENTS.md

Rainy is a lightweight Windows desktop AI workbench for office work, research and code: a native interface (`RainyAgent.exe`, Rust and GPUI), a Rust Host per execution target (Windows, WSL or a Linux server over SSH), and React viewer pages shown in WebView2 for documents. Rainy ships no Node.js. It is a single repository with no plugin framework. Read [docs/architecture.md](docs/architecture.md) before changing the Host, the RPC protocol or persisted files.

## Layout

```
native/           Cargo workspace (GNU toolchain)
  crates/protocol   RPC types shared by the Host and the interface, streaming patches
  crates/host       rainy-host: HTTP/WebSocket server, RPC, settings, projects, runtime, search, Git, project map
    src/agent/      models and model requests, chats, tools, permissions, prompt, compaction, memory, skills, MCP
    src/ide/        files, editor state, run/debug, terminals, language servers, formatting, change hints
  crates/carrier    Host launch and control requests, /rpc, SSH, WSL, modules, Strata, updates, Explorer integration
  crates/app        the GPUI window: store/ (state), ui/ (views), web/ (WebView2 panes); assets/messages.json (strings)
  installer/        NSIS script and the offline readme
  scripts/          build-hosts.sh, build-release.sh, run-ui.sh, drive.ps1 and other development tools
src/shared/       TypeScript types of the protocol the viewer pages use (rpc.ts) and code shared by those pages
src/renderer/     the viewer pages (viewer/ViewerApp.tsx): PDF, Office, Markdown, media, the project map; ui/, theme/
tests/            renderer/ (vitest, happy-dom), scripts/ (node:test)
scripts/          viewer build, Host runtime staging, IDE tools, packaging and release tooling
resources/        icons, pdf.js and the IDE tools staged into the Host runtime
toolpacks/        pinned build inputs and source manifests
```

## Commands

```sh
pnpm install
pnpm run typecheck      # build scripts and viewer pages
pnpm exec vitest run    # viewer tests (tests/renderer)
pnpm run test:node      # node:test suites in tests/scripts
pnpm run build          # dist/renderer (the viewer pages)
cd native && cargo build -p rainy -p rainy-host               # the interface and the Host (debug)
cd native && cargo test -p rainy -p rainy-carrier -p rainy-host -p rainy-protocol
bash native/scripts/build-hosts.sh                            # release Host for Windows and Linux (cargo-zigbuild)
bash native/scripts/run-ui.sh                                 # start the interface with the scratch profile tmp/ui-data
```

The interface finds the Host in `windows-host/rainy-host.exe` of its resources; for development copy `native/target/release/rainy-host.exe` into `tmp/dev-resources/windows-host/`.

Before pushing, run `typecheck`, the tests of what you changed and `build` when the viewer build or its dependencies changed. CI (`.github/workflows/rainy.yml`) runs the TypeScript checks on Ubuntu and Windows and the Host and protocol tests on Ubuntu.

## Conventions

- Rust: no `unwrap` on input from outside the process; Host errors are `HostError` with a code. A panic aborts the whole Host (`panic = "abort"`), so code that may run on a plain thread (a terminal reader, a file watcher) must not call `tokio::spawn` without a runtime handle.
- The Host and its clients talk through `/rpc`: add a method in `native/crates/host` (registered in `main.rs` or `agent/service.rs`), its types in `native/crates/protocol` and, when a viewer page calls it, in `src/shared/rpc.ts`. Validate RPC input on the Host.
- Persisted files have versions: the chat format, `settings.json`, project memory (`memory.v1.json`), IDE state. They are the 3.x formats, so 3.x and 4.x read each other's data; change a format only with a reader for the previous version.
- Anything the model sees must be reconstructable from the chat file. New model-visible input needs a transcript entry kind or a `context` entry.
- Interface text is localized: `native/crates/app/assets/messages.json` holds Chinese and English per key (`tr(cx, "module.key")`); viewer pages use the `messages.ts` of their folder. The Host writes its errors with `l(zh, en)`, chosen by the language in Settings when the message is made. No hard-coded copy.
- Colors, radii and shadows come from the theme: `native/crates/app/src/theme.rs` for the interface, the `--rainy-*` tokens in `src/renderer/theme/` for viewer pages.
- Text from projects, tools, language servers and remote servers is untrusted: cloned repositories and SSH hosts can contain anything. Never render it as HTML without sanitizing; viewer pages have privileged bridges.
- Tool calls pass `agent/permissions.rs` before they run: read-only, ask (the default) or full auto per chat. Keep tools inside their documented behavior: `read` before `write`/`edit`, output limits, spill files, and an undo snapshot for every change.
- Pin exact dependency versions (crates, npm packages, IDE tools) that are at least two weeks old.
- Comments state contracts and non-obvious reasons, not narration. Files end with one newline.
- Docs change with the code: `docs/architecture.md` for structure, `docs/desktop*.md` for build and release, README for users.

## Tests

- Host behavior: unit tests next to the code in `native/crates/host`; end-to-end checks drive a built `rainy-host` with a mock model server over `/rpc`.
- Viewer pages: `tests/renderer` with happy-dom.
- Changes visible in the window deserve a check in the running interface: `bash native/scripts/run-ui.sh`, then `native/scripts/drive.ps1` clicks, types and captures that window only.

## Secrets

Real model calls read keys from Settings. Never commit keys. Settings keeps them in `.credentials.sealed.json`, encrypted under a key the carrier protects with Windows DPAPI; a Host started without the carrier keeps them as plain text in `.credentials.json` under its private home. `RAINY_CONFIGURE_DEEPSEEK=1` makes a Host save and select the DeepSeek preset at startup.
