# Changelog

Highlights of each release. Installers, resource pieces and full notes are on [GitHub Releases](https://github.com/RainyMarks/Rainy/releases); [Validation](docs/validation.md) records the checks completed for each release.

English | [中文](CHANGELOG.zh.md)

## 3.8 · 2026-10-19

Lighter: smaller downloads, less CPU while idle and while a reply streams.

- **The WSL / SSH runtime base shrinks from about 277 MB to about 53 MB**: it is packed with xz and leaves out what the Host never reads (Node.js headers, a second copy of Ruff, the debugger's sources and other platforms' binaries). The offline Ubuntu 26.04 development packages (about 165 MB) are downloaded only when you prepare the development tools, and the download is deleted once they are installed. This update downloads the new base once.
- **Less work while idle**: the rain behind empty views draws at most about 60 frames a second (24 while the window is in the background) at a lower canvas resolution, about a third of the idle CPU it used on high-refresh displays; the running spinner turns on the GPU instead of being repainted every frame.
- **Smoother long replies**: while a reply streams, only the turn that grows renders again and only its last Markdown block is parsed again; the Host sends the page only the new text, with the whole reply every 2 seconds, so far less goes over an SSH tunnel.
- **Faster start**: Settings and code highlighting load on first use, cutting the startup script from 1010 KB to 790 KB; math ships WOFF2 fonts only.

## 3.7 · 2026-10-19

The agent keeps servers running, checks its own work and hands searches to sub-agents.

- **Background commands**: a development server or watcher keeps running in the background; the agent gets an ID, goes on working, reads the new output when it needs it and stops the command when done. The commands are listed above the message box with a stop button each and end when the chat closes.
- **It checks its own work**: after changing a web application the agent opens it in a preview window on this machine, reads the page, its console errors and its failed requests, clicks and types, and looks again. The preview opens local addresses only and carries no sign-ins.
- **Sub-agents**: a long search goes to a sub-agent that only reads and answers with one report, so the conversation keeps its room; sub-agents started in one step run at the same time.
- **Faster suggestions**: editor suggestions stream in as they are written, are requested 0.18 s after typing stops, and continue right after Tab accepts one.

## 3.6 · 2026-10-18

Updates no longer set up the environment again, and Rainy is lighter.

- **No Linux runtime download after an update**: every update used to download the Linux runtime (about 280 MB) again on the first WSL or SSH start. It now comes in two layers: a base that rarely changes (Node.js, dependencies, IDE tools) and an application layer of a few MB that ships with the installer. While the base is unchanged, an update swaps in the new application layer without downloading or reinstalling anything; the update from 3.5 to 3.6 reuses the environment you already have, too.
- **Interactive answers removed**: they suit plain conversation better than Rainy's agent work, so they are gone, with the chart and 3D libraries they shipped.

## 3.5 · 2026-10-17

- **Every screen and scaling**: at high display scaling (such as 225%) or on small screens the menus no longer run under the search box; the window fits the screen's work area when it opens, when it moves to another display and when the scaling changes; the chat pane follows the window width until you resize it.

## 3.4 · 2026-10-17

Notes and documents are easier to work on.

- **WYSIWYG Markdown**: a .md file opens formatted, and you type on it as in Typora; headings, lists, task lists, tables, code blocks and LaTeX math take shape as you write, and `/` inserts any of them. Source, Preview and Split are one click away. Saving changes only the lines you edited, so escapes, blank lines and table spelling elsewhere in the file stay as they were.
- **Chinese PDFs no longer come out blank**: PDFs that do not embed their Chinese fonts, as many papers and official documents do not, used to show only their English text; now the Chinese shows too.
- **Password-protected Office files** say that they need their password and offer Open in default app, instead of hanging on a password prompt nobody can see.

## 3.3 · 2026-10-17

The AI can go online: look up current documentation, papers, version numbers and error messages, then read the pages it needs in full.

- **Search the web** with DeepSeek's own search engine: titles, links, dates and summaries, with `site:`, `"exact phrase"`, `lang:zh` and `after:2025` syntax. Links in the result card open in your browser.
- **Read a web page** as Markdown. Reading a site for the first time asks you first, and you can remember the site; fully automatic chats do not ask.
- **DeepSeek only**: it uses the API key of the DeepSeek model you added, and your DeepSeek account pays (5 yuan per 1,000 searches or page reads; failed and empty requests are free). Without a DeepSeek key the two tools do not appear. Turn it off in **Settings → Models → Web search**.

## 3.2 · 2026-10-17

Rainy turns from CTF to office work, research and coding. Open a folder of papers, reports and spreadsheets and click a file to read it, instead of a screen of hex.

![A PowerPoint deck converted by PowerPoint on this PC and shown in Rainy](assets/office.png)

- **Word**: Rainy draws .docx files itself as paginated paper with headings, tables, pictures, headers and footers, no Office needed. For the exact Word layout, click **Office layout**.
- **PowerPoint and legacy Word**: .pptx, .ppt and .doc files are turned into PDF by the Microsoft Office (or WPS) installed on your PC, so charts, tables and fonts match the original and the text can be extracted. Without Office, Rainy lists the text of each slide.
- **Excel**: .xlsx, .xls and .ods files show as tables with fixed row and column headings, merged cells and a tab per sheet.
- **Open in default app**: one click hands a document to Word, PowerPoint, Excel or WPS for editing (documents only; programs are never started).
- **Small files explained**: a `~$` file says it is an Office lock file and who has which document open; a `.lnk` shortcut shows where it points.

## 3.1 · 2026-10-16

Rainy now looks like its name.

![Rain on an empty editor and a new chat, with a ripple where the background was clicked](assets/rain.png)

- **Rain on empty views**: the welcome view, an editor with no file open and a new chat show light, slanted rain in your accent color, with small rings where drops land. Click the background and a ripple spreads, pushing and lighting the rain it passes. It follows the color scheme and accent, pauses while the window is hidden, stays still when Windows asks for reduced motion, and can be turned off in **Settings → Appearance → Motion**.
- **A living mark**: the Rainy mark takes your accent color. Point at it and its three bars fall like rain; in the top bar they keep falling while the current chat works, and finish their fall when it stops.
- **Reasoning as a rain cloud**: the reasoning row shows a rain cloud instead of the atom icon inherited from DeepSeek Harness, and its drops fall while the model thinks.
- **New typefaces (3.1.1)**: Latin text uses the bundled Hanken Grotesk and code, the editor and the terminal use Geist Mono, so Rainy looks the same on every PC; Chinese stays in Microsoft YaHei. Your own choices in **Settings → Appearance → Fonts** still come first.
- A refreshed application icon with a light-to-deep blue tile, and the last DeepSeek Harness names removed from the interface code.

## Earlier releases

3.0 added SSH remote development and dropped the CTF toolkit; 2.5 added project-wide search and replace, the command palette and the Git panel; 2.4 the project map and review of the agent's changes; 2.3 inline AI completion and Ctrl+K edits. See the [release notes](https://github.com/RainyMarks/Rainy/releases) for every version.
