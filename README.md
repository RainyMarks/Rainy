<div align="center">

<img src="resources/icon.png" width="88" alt="Rainy logo">

# Rainy

### The AI workbench that gets things done, for office work, research and code

An agent that edits your files, runs commands and checks its own work,<br>
with an editor, Office previews and SSH remote development, in one 180 MB Windows app.

<br>

[![Download for Windows](https://img.shields.io/github/v/release/RainyMarks/Rainy?style=for-the-badge&label=Download%20for%20Windows&color=2563eb)](https://github.com/RainyMarks/Rainy/releases/latest)
&nbsp;
[![Changelog](https://img.shields.io/badge/Changelog-3.9-0f172a?style=for-the-badge)](CHANGELOG.md)

[![Downloads](https://img.shields.io/github/downloads/RainyMarks/Rainy/total?style=flat-square&color=10b981)](https://github.com/RainyMarks/Rainy/releases)
![Platform](https://img.shields.io/badge/platform-Windows%2010%2F11%20x64-0078d4?style=flat-square)
[![License](https://img.shields.io/badge/license-PolyForm%20Strict%201.0.0-f59e0b?style=flat-square)](LICENSE)
[![X](https://img.shields.io/badge/follow-%40RainyMarksqwq-000000?style=flat-square&logo=x)](https://x.com/RainyMarksqwq)

**English** · [中文](README.zh.md)

[Features](#features) · [Quick start](#run) · [Connect a model](#connect-a-model) · [Privacy](#privacy) · [Docs](#docs) · [FAQ](#faq)

</div>

<br>

![Rainy connected to a Linux server over SSH: the agent ran a script on the server and reported the total](assets/workbench.png)

<p align="center"><sub>Open a project on a server over SSH and let the agent run code there, while the window stays on your PC.</sub></p>

<br>

## Why Rainy

<table>
<tr>
<td width="33%" valign="top">

**Hands-on, and on a leash**

The agent edits files, runs commands and searches the web. It asks before commands by default, and you keep or undo every change it made.

</td>
<td width="33%" valign="top">

**A server that feels local**

Connect to a Linux server and the agent, terminals and debugger run next to your code, in the same window on your PC.

</td>
<td width="33%" valign="top">

**Opens everything you work with**

Word, Excel, PowerPoint, PDF, Markdown, images and media open in place, so papers, reports and code share one window.

</td>
</tr>
</table>

<a id="features"></a>
## Features

### An AI agent: let it work, keep the final say

- **Real work**: reads, searches and edits project files, runs commands, searches the web and reads pages through DeepSeek, and remembers the project across chats.
- **Three permission modes**: each chat runs read-only, asks before commands (the default), or runs fully automatically; "Always allow" is remembered per project.
- **Review every change**: the files it changed are listed above the message box; keep or undo each change in the editor. GBK and UTF-16 files keep their encoding.
- **Checks its own work**: development servers keep running in the background; after changing a web app, the agent opens it in a local preview, reads the page, console errors and failed requests, and tells you what it saw.
- **Delegates**: a long search goes to a sub-agent that answers with one report, so the conversation keeps its room.
- **Extensible**: skills in `.rainy/skills`, MCP servers in Settings, and one click adds computer use through the open-source [Windows-MCP](https://github.com/CursorTouch/Windows-MCP).

![Reviewing the agent's changes in the editor, with Keep and Undo beside each change](assets/review.png)

<a id="ssh"></a>
### SSH remote development: the server's project, a local feel

- Pick a host from `~/.ssh/config`, or enter `user@host`.
- The agent, terminals, run and debug, and language services work on the server's files; the interface stays on your PC.
- It uses the OpenSSH client built into Windows, so your keys, ssh-agent and ProxyJump just work.
- The first connection installs a runtime of about 53 MB in `~/.rainy-agent` on the server; later connections reach the workbench in seconds.

![The SSH remote development card in Settings, listing hosts from ~/.ssh/config](assets/ssh.png)

### Document previews: open a file, see the document

- **Word** as paginated paper; **Excel** and OpenDocument spreadsheets as tables with a tab per sheet; **PowerPoint** and legacy `.doc` converted by the Office on your PC, so they look as they do there.
- **Markdown** opens formatted and is edited in place, like Typora (see the next section).
- **PDF** (including Chinese papers without embedded fonts), images, audio and video in place; other binaries in a hex view.

![A PowerPoint deck converted by PowerPoint on the PC and shown in Rainy](assets/office.png)

### Markdown writing: as smooth as Typora

- A .md file opens formatted and you write right on it; **Ctrl+/** switches to the source at any time. Saving rewrites only the lines you changed.
- The **outline** in the side pane follows your reading and jumps to any heading; **Ctrl+F / Ctrl+H** finds and replaces in the document; Ctrl+click opens links.
- Paste a screenshot or drop an image: it is saved to an `assets` folder next to the document with a relative link.
- Math and **Mermaid** diagrams show their rendering until you click them; front matter gets its own box instead of becoming a heading.
- Typora's shortcuts: Ctrl+1–6 headings, Ctrl+T table, Ctrl+Shift+K code block, Ctrl+Shift+M math; **F8 focus mode**, **F9 typewriter mode**; a word count in the status bar.
- **Export to PDF or HTML** with images, math and diagrams embedded, ready to send.

### Editor and debugger: a real editor with AI built in

- Suggestions as you type (Tab accepts); select code, press Ctrl+K and describe the change, then compare and keep it.
- CodeMirror 6 with completion, hover, go to definition, rename, diagnostics, diffs and recovery of unsaved work.
- Run and debug Python, JavaScript/TypeScript and C/C++; run PHP.
- Project-wide search and replace with a preview of every change, a command palette, and Git with AI-written commit messages.

![Ctrl+K adding an empty-key check to a function, with the new lines in green](assets/ai-edit.png)

<table>
<tr>
<td width="50%"><img src="assets/search.png" alt="Replacing a word across the project, each match showing its before and after"></td>
<td width="50%"><img src="assets/git.png" alt="The Git panel with a staged file and an AI-written commit message"></td>
</tr>
</table>

### Project map: understand an unfamiliar project fast

See what a project is made of, how its parts import each other and which stack it uses; get a one-line AI explanation of each part, with the files the agent is reading or changing highlighted live.

![The project map of Rainy's own source with the AI explanation](assets/map.png)

### And more

| Feature | What it does |
|---|---|
| **Explorer integration** | Optional, off until you turn it on: right-click a file or folder for "Open with Rainy"; Rainy is listed under "Open with" for .md, code and other text files and can be their default app. |
| **Three execution environments** | Native Windows by default; switch to WSL2 or an SSH server when a project needs Linux. |
| **Project memory** | When turned on, collects a project's key facts while idle and brings them into new chats. |
| **Context management** | Shows how full the context is and compacts it automatically before it runs out; Claude's prompt cache lasts an hour. |
| **Light** | A 180 MB installer; the Strata engine, PHP and the Linux runtime download only when needed. |
| **Any screen** | Fits high scaling (such as 225%) and small screens, and adjusts when you change displays or scaling. |
| **Details** | A light rain falls behind empty views and ripples where you click; turn it off in **Settings → Appearance**. |

<a id="run"></a>
## Quick start

1. Download and install [the latest Windows x64 installer](https://github.com/RainyMarks/Rainy/releases/latest). No activation code is required.
2. Open Rainy and choose **File → Open folder**.
3. Connect a model in **Settings → Models** (see [Connect a model](#connect-a-model)), then open **AI assistant** on the right.

To work on a Linux server, make sure `ssh` logs in from a terminal, pick the host under **Settings → Runtime → SSH remote development**, select **Switch to this host**, then open a project with **File → Browse folders on current host**. See the [desktop guide](docs/desktop.md#ssh-remote-development).

Rainy downloads stable updates in the background and then offers **Later** or **Restart and install**.

### System requirements

| Item | Requirement |
|---|---|
| Operating system | Windows 10 / 11 x64 |
| Disk space | About 570 MB installed |
| WSL (optional) | WSL2; the bundled Ubuntu development tools wizard supports Ubuntu 26.04 |
| SSH server (optional) | x86_64 Linux with `python3`; the runtime takes about 250 MB |
| Local inference (optional) | NVIDIA GPU, CUDA 13, driver 580 or newer |

### Components downloaded on demand

| Component | Size | When |
|---|---|---|
| Linux runtime | about 53 MB | Automatically, the first time you use WSL or an SSH host |
| Ubuntu development packages | about 165 MB | When you install the development tools; deleted once installed |
| PHP | about 38 MB | **Settings → Runtime → Optional components** |
| Strata engine | about 560 MB | **Settings → Runtime → Optional components** |

<a id="connect-a-model"></a>
## Connect a model

Bring your own model; Rainy never switches to another one behind your back.

| Kind | Notes |
|---|---|
| **DeepSeek** | DeepSeek V4.1 Flash preset; web search uses your DeepSeek key too |
| **Claude** | Claude Opus 5.5 and Claude Haiku 5.5 presets, adaptive thinking, an hour of prompt cache |
| **Compatible APIs** | Any OpenAI- or Anthropic-compatible Base URL, with model discovery; relays work too |
| **Local servers** | Ollama and other OpenAI-compatible servers on your machine |
| **Strata** | Local inference on an NVIDIA GPU; you supply the Qwen3.8 Flash Next weights |

1. Open **Settings → Models** and select **Load** on a preset, or enter a provider ID, Base URL, protocol and key.
2. Select **Discover models**, or enter the model ID.
3. Set the service's actual context window and output limit, save, then run the streaming and tool-call check.

The selected Windows, WSL or SSH environment must reach the model service. Through a relay, keep the `anthropic-messages` protocol and the Claude model ID (for example `claude-opus-5-5`). See the [desktop guide](docs/desktop.md#strata-local-inference) for preparing Strata and its resource controls.

<a id="privacy"></a>
## Privacy and security

- **Keys stay on your PC**, encrypted with Windows data protection (DPAPI).
- **No telemetry**: crash reports stay on your machine until you export them yourself.
- **Requests go only where you say**: model requests go to the services you configured, through the system proxy or one you set; updates and on-demand components come from GitHub, each piece checked by SHA-256.
- **A gate before every action**: each tool call passes a permission check; commands, MCP tools and writes outside the project ask you first by default.
- **Outside content is untrusted**: web images in replies are not loaded automatically, and document previews do not run the scripts inside them.
- **Verifiable releases**: on first start the installed files are checked one by one against a signed manifest.

<a id="whats-new"></a>
## What's new in 3.9

**Write Markdown as in Typora, and open files from Explorer.** An outline in the side pane, find and replace in the document, pasted images saved for you, Mermaid diagrams, focus and typewriter modes, and export to PDF or HTML; turn on "Open with Rainy" in Settings if you want it. 3.9.1 makes that integration opt-in. [Read the full changelog →](CHANGELOG.md)

<a id="docs"></a>
## Documentation

| Document | Covers |
|---|---|
| [Desktop guide](docs/desktop.md) | Optional components, updates, SSH, Strata, models, project memory and known limits |
| [Setup and build guide (Chinese)](docs/desktop-setup.zh-CN.md) | Offline installation, editing and debugging, permissions and undo, building from source |
| [Changelog](CHANGELOG.md) | Highlights of each release |
| [Validation](docs/validation.md) | The build, installation and runtime checks completed for each release (Chinese) |
| [Architecture](docs/architecture.md) | Processes, the Host, the protocol and data formats, for developers |

<a id="faq"></a>
## FAQ

<details>
<summary><b>Is Rainy free?</b></summary>
<br>
Noncommercial use is free: personal study, research and hobby projects, and schools, research institutions, charities and government bodies may use the releases RainyMarks publishes at no cost. Commercial use, including use at work in a company and projects for paying clients, needs a commercial license from RainyMarks; see the <a href="#license">license</a>.
</details>

<details>
<summary><b>Can I modify Rainy or build my own project on its code?</b></summary>
<br>
No. The license permits use only: no changes, no distribution and no new works based on its code. Renaming, changing the icon or interface, translating, or rewriting the code by hand or with AI all count as changes; removing copyright notices or presenting Rainy or any part of it as your own work is not permitted either. To propose an improvement, describe it in <a href="https://github.com/RainyMarks/Rainy/issues">GitHub Issues</a> first and wait for written permission from RainyMarks.
</details>

<details>
<summary><b>Is my code uploaded anywhere?</b></summary>
<br>
Rainy collects no usage data. What the agent needs goes only to the model services you configured; web search appears only when you have a DeepSeek key and have not turned it off.
</details>

<details>
<summary><b>Do I need WSL?</b></summary>
<br>
No. Rainy runs natively on Windows by default; switch to WSL2 or an SSH server when a project needs Linux.
</details>

<details>
<summary><b>Is there a macOS or Linux desktop app?</b></summary>
<br>
The desktop app runs on Windows 10 / 11 x64 only. Linux works as the server for SSH remote development.
</details>

<details>
<summary><b>What if the agent gets something wrong?</b></summary>
<br>
Every file it changed is listed above the message box: undo single changes in the editor, or a whole turn at once. Rainy keeps the earlier content itself (files up to 16 MB), so undo does not depend on Git.
</details>

<details>
<summary><b>Does it work offline?</b></summary>
<br>
Yes, with a model server on your machine (such as Ollama) or Strata on an NVIDIA GPU. The full offline release and its installation are described in the <a href="docs/desktop-setup.zh-CN.md">setup guide</a>.
</details>

<a id="run-from-source"></a>
## Build from source

<details>
<summary>Show the build steps</summary>
<br>

Building the Windows release requires Node.js `^22.19 || >=24`, pnpm `11.7.0`, and the Windows/WSL prerequisites in the [build guide](docs/desktop-setup.zh-CN.md#从源码构建). Pinned binaries such as `build-inputs/strata-runtime.tar.gz` are restored from release assets by the bootstrap command rather than stored in Git.

```powershell
git clone https://github.com/RainyMarks/Rainy.git
cd Rainy
pnpm install --frozen-lockfile
node scripts/bootstrap-release-inputs.mjs --manifest toolpacks/build-inputs.v1.json
powershell.exe -NoProfile -ExecutionPolicy Bypass -File scripts/package.ps1 -Distribution Ubuntu -ComponentSource release/offline-3.9.1/environment-components
```

Replace `Ubuntu` only when your WSL build distribution has another name, and `3.9.1` with the version in `package.json`. Building from source is for your own noncommercial use: the installer you build may not be distributed, and changing the code needs written permission from RainyMarks.

For development, `pnpm run dev` builds the app and starts a Host without Electron, then prints a URL to open in a browser; `pnpm run start` runs Electron on the built files. Developers can start with the [architecture](docs/architecture.md) and [AGENTS.md](AGENTS.md).

</details>

## Feedback and support

- **Bugs and ideas**: open an issue on [GitHub Issues](https://github.com/RainyMarks/Rainy/issues).
- **Release news**: follow [@RainyMarksqwq](https://x.com/RainyMarksqwq) on X.
- **Like it?** A ⭐ on this repository helps others find it.

<a id="license"></a>
## License

Rainy's own code and documentation are licensed under the [PolyForm Strict License 1.0.0](LICENSE.Rainy).

- **Free to use**: personal uses without a commercial purpose, such as study, research and hobby projects, and charitable organizations, educational institutions, public research organizations, public safety or health organizations, environmental protection organizations and government institutions may download, install and use Rainy at no cost.
- **No changes or distribution**: for any purpose, paid or not, you may not change Rainy, make new works based on its code, or give copies of it, changed or not, to others. Renaming, rebranding, changing the icon or interface, translating, porting, and rewriting its code or documentation by hand or with AI all count as changes. You also may not remove or alter copyright notices, present Rainy or any part of it as your own work, or use the Rainy name or icon.
- **Written permission required**: any commercial use, including use at work in a company and work for paying clients, and anything else these terms do not allow, such as contributing a change. Contact RainyMarks through [GitHub Issues](https://github.com/RainyMarks/Rainy/issues) or [@RainyMarksqwq](https://x.com/RainyMarksqwq) on X.

GitHub's Terms of Service let users view and fork this repository on GitHub, but that grants no right to change or distribute it. Projects you open, edit or build with Rainy, and output it produces for you, are not covered by these terms.

Interface code derived from DeepSeek Harness stays under its [MIT license](LICENSE.upstream), and third-party components keep their own licenses (see [third-party notices](THIRD_PARTY_NOTICES.md)). Rainy 3.8.0 and earlier releases remain under the Rainy Source Available License 1.0 they were published with; it too requires keeping copyright notices, identifying modifications and not presenting a modified version as an official release, and states that changing the name, interface or branding does not remove those conditions. This is a source-available project, not OSI-approved open-source software. See [LICENSE](LICENSE) for the scope.

<br>

<div align="center">
<sub>Developed by <a href="https://github.com/RainyMarks">RainyMarks</a></sub>
</div>
