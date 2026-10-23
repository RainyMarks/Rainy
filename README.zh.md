<div align="center">

<img src="resources/icon.png" width="88" alt="Rainy 标志">

# Rainy

### 会动手的 AI 工作台，为办公、科研和编码而生

能读写文件、执行命令、自己检查成果的 AI 智能体，<br>
连同编辑器、Office 文档预览和 SSH 远程开发，装进一个 67 MB 的 Windows 应用。

<br>

[![下载 Windows 版](https://img.shields.io/github/v/release/RainyMarks/Rainy?style=for-the-badge&label=%E4%B8%8B%E8%BD%BD%20Windows%20%E7%89%88&color=2563eb)](https://github.com/RainyMarks/Rainy/releases/latest)
&nbsp;
[![更新日志](https://img.shields.io/badge/%E6%9B%B4%E6%96%B0%E6%97%A5%E5%BF%97-4.1-0f172a?style=for-the-badge)](CHANGELOG.zh.md)

[![Downloads](https://img.shields.io/github/downloads/RainyMarks/Rainy/total?style=flat-square&color=10b981)](https://github.com/RainyMarks/Rainy/releases)
![Platform](https://img.shields.io/badge/platform-Windows%2010%2F11%20x64-0078d4?style=flat-square)
[![License](https://img.shields.io/badge/license-PolyForm%20Strict%201.0.0-f59e0b?style=flat-square)](LICENSE)
[![X](https://img.shields.io/badge/follow-%40RainyMarksqwq-000000?style=flat-square&logo=x)](https://x.com/RainyMarksqwq)

[English](README.md) · **中文**

[功能](#features) · [快速开始](#run) · [连接模型](#connect-a-model) · [隐私与安全](#privacy) · [文档](#docs) · [常见问题](#faq)

</div>

<br>

![Rainy 通过 SSH 连接 Linux 服务器，AI 在服务器上运行了脚本并报告总价](assets/zh/workbench.png)

<p align="center"><sub>通过 SSH 打开服务器上的项目，让 AI 在服务器上运行脚本，界面仍留在你的电脑上。</sub></p>

<br>

## 为什么是 Rainy

<table>
<tr>
<td width="33%" valign="top">

**能动手，也守规矩**

AI 直接改文件、跑命令、上网查资料。默认在运行命令前先问你，改过的每一处都能逐条保留或撤销。

</td>
<td width="33%" valign="top">

**服务器当本地用**

连上一台 Linux 服务器，AI、终端和调试器都在代码旁边运行，窗口还是这台电脑上的窗口。

</td>
<td width="33%" valign="top">

**什么文件都能看**

Word、Excel、PPT、PDF、Markdown、图片和音视频直接打开，写论文、整理资料和写代码在同一个窗口里。

</td>
</tr>
</table>

<a id="features"></a>
## 功能

### AI 智能体：放手让它做，结果你说了算

- **真正干活**：读写和搜索项目文件、执行命令，用 DeepSeek 联网搜索和读网页，跨对话记住项目要点。
- **三档权限**：每个对话可选只读、询问（默认，执行命令前先问）或完全自动；“总是允许”按项目记住。
- **逐处审查**：AI 改过的文件列在输入框上方，在编辑器里一处一处保留或撤销；GBK、UTF-16 文件改完仍是原编码。
- **会自查**：开发服务器放在后台一直跑；改完网页后，AI 在本机预览窗口里打开成果，看页面、控制台报错和失败的请求，再告诉你结果。
- **能分工**：费时的查找交给子 Agent，只带回一份报告，不占对话的上下文。
- **可扩展**：`.rainy/skills` 放技能，设置里添加 MCP 服务；一键接入开源的 [Windows-MCP](https://github.com/CursorTouch/Windows-MCP)，让 AI 操作电脑上的窗口和应用。

![在编辑器里审查 AI 的修改，每处改动旁都有保留和撤销](assets/zh/review.png)

<a id="ssh"></a>
### SSH 远程开发：服务器上的项目，本地般的体验

- 从 `~/.ssh/config` 里选主机，或直接填写 `user@host`。
- AI、终端、运行调试和语言服务都在服务器上处理文件，界面留在这台电脑上。
- 使用 Windows 自带的 OpenSSH 客户端，密钥、ssh-agent 和 ProxyJump 照常可用。
- 首次连接在服务器的 `~/.rainy-agent` 安装约 27 MB 的运行环境，之后几秒就能进入工作台。

![设置里的 SSH 远程开发卡片，列出了 ~/.ssh/config 中的主机](assets/zh/ssh.png)

### 文档预览：点开就能看，不是一屏十六进制

- **Word** 显示成分页的纸面；**Excel** 和 WPS 表格显示成可切换工作表的表格；**PPT** 与旧版 `.doc` 交给电脑上的 Office 转换，和原软件里看到的一样。
- **Markdown** 打开就是排好版的样子，像 Typora 一样直接编辑（见下一节）。
- **PDF**（包括没有嵌入字体的中文论文）、图片、音视频直接预览；其他二进制文件用十六进制查看。

![PPT 由电脑上的 PowerPoint 转换后直接显示在 Rainy 里](assets/zh/office.png)

### Markdown 写作：像 Typora 一样顺手

- 打开 .md 就是排好版的样子，直接在上面写；**Ctrl+/** 随时切到源码。保存时只改动过的行，其余写法原样保留。
- 侧栏**大纲**跟着阅读位置走，点标题即跳转；**Ctrl+F / Ctrl+H** 在文档里查找替换；按住 Ctrl 单击打开链接。
- 截图直接粘贴、图片拖进来，自动存进旁边的 `assets` 文件夹并写好相对路径。
- 公式和 **Mermaid** 图表平时只显示渲染结果，点一下才显示源码；YAML 前言单独显示，不会被排成标题。
- Typora 的快捷键：Ctrl+1–6 标题、Ctrl+T 表格、Ctrl+Shift+K 代码块、Ctrl+Shift+M 公式；**F8 专注模式**、**F9 打字机模式**；状态栏显示字数。
- 一键**导出 PDF / HTML**，图片、公式和图表都嵌进文件，发给别人直接能看。

### 编辑器与调试：真编辑器，AI 内置

- 边写边给建议，按 Tab 接受；选中代码按 Ctrl+K 描述改法，新旧对比后再决定保留。
- 基于 CodeMirror 6：补全、悬停提示、转到定义、重命名、诊断、差异对比、未保存内容恢复。
- 运行和调试 Python、JavaScript/TypeScript、C/C++，运行 PHP。
- 全项目搜索替换（替换前逐处预览）、命令面板、Git 面板，提交说明可以让 AI 来写。

![Ctrl+K 给函数加上空密钥检查，新增的行标为绿色](assets/zh/ai-edit.png)

<table>
<tr>
<td width="50%"><img src="assets/zh/search.png" alt="在整个项目里替换一个词，每处匹配都显示替换前后的样子"></td>
<td width="50%"><img src="assets/zh/git.png" alt="Git 面板：一个已暂存的文件和 AI 写好的提交说明"></td>
</tr>
</table>

### 项目地图：快速看懂一个陌生项目

画出项目由哪些部分组成、彼此怎么引用、用了什么技术栈；让 AI 用一句话讲清每个部分，并实时高亮 AI 正在读或改的文件。

![Rainy 自身源码的项目地图和 AI 讲解](assets/zh/map.png)

### 还有这些

| 功能 | 说明 |
|---|---|
| **资源管理器集成** | 可选，默认关闭：打开后右键文件或文件夹“通过 Rainy 打开”；.md 和代码等文本文件的“打开方式”里有 Rainy，也能设为默认应用。 |
| **三种执行环境** | 默认 Windows 原生；需要 Linux 时切到 WSL2 或一台 SSH 服务器。 |
| **项目记忆** | 开启后在空闲时整理项目要点，新对话开始时自动带上。 |
| **上下文管理** | 实时显示上下文占用，快满时自动压缩；Claude 的提示词缓存保留一小时。 |
| **轻量** | 安装包 67 MB；PHP 和 Linux 运行环境都在用到时才下载。 |
| **适配任意屏幕** | 高缩放（如 225%）和小屏幕上自动适配，换显示器或改缩放后自动调整。 |
| **细节** | 空白页飘着细雨，点一下泛起涟漪；可在 **设置 → 外观** 关闭。 |

<a id="run"></a>
## 快速开始

1. 下载并安装[最新版 Windows x64 安装包](https://github.com/RainyMarks/Rainy/releases/latest)，无需激活码。
2. 打开 Rainy，点击标题栏的 **打开文件夹**（打开项目后，这个按钮显示项目名，点开是项目菜单）。
3. 在 **设置 → 模型** 连接一个模型（见[连接模型](#connect-a-model)），然后在右侧的 AI 面板里提问。

连接 Linux 服务器：先确认终端里能用 `ssh` 登录，再到 **设置 → 运行环境 → SSH 远程开发** 选择主机并点击 **切换到此主机**，最后用标题栏项目菜单里的 **浏览当前执行环境文件夹** 打开服务器上的项目。详见[桌面指南](docs/desktop.zh.md#ssh-remote-development)。

Rainy 会在后台下载稳定版更新，下载完成后可选择 **稍后** 或 **重启安装**。

### 系统要求

| 项目 | 要求 |
|---|---|
| 操作系统 | Windows 10 / 11 x64 |
| 磁盘空间 | 安装后约 165 MB |
| WSL（可选） | WSL2；随附的 Ubuntu 开发工具向导适用于 Ubuntu 26.04 |
| SSH 服务器（可选） | x86_64 Linux，装有 `python3`，glibc 2.17 或更新；运行环境约占 110 MB |
| WebView2 | 预览 PDF、Office 和 Markdown 时使用；Windows 11 自带，Windows 10 上没有时按提示安装 |

### 按需下载的组件

| 组件 | 大小 | 何时下载 |
|---|---|---|
| Linux 运行环境 | 约 27 MB | 第一次使用 WSL 或 SSH 主机时自动下载 |
| Strata 引擎 | 约 533 MB | **设置 → 模型 → Strata 本地模型**，在 NVIDIA 显卡上运行本地模型时 |
| Ubuntu 开发工具包 | 约 165 MB | 在“准备开发工具”中点击安装时，装好后自动删除 |
| PHP | 约 38 MB | **设置 → 运行环境 → 可选组件** |

<a id="connect-a-model"></a>
## 连接模型

模型自己选，Rainy 不会悄悄换成别的模型。

| 类型 | 说明 |
|---|---|
| **DeepSeek** | 内置 DeepSeek V4.1 Flash 预设；联网搜索也使用你的 DeepSeek 密钥 |
| **Claude** | 内置 Claude Opus 5.5、Claude Haiku 5.5 预设，自适应思考，提示词缓存保留一小时 |
| **兼容接口** | 任意 OpenAI 或 Anthropic 兼容的 Base URL，可自动发现模型；中转服务同样可用 |
| **本地服务** | Ollama 等运行在本机的 OpenAI 兼容服务 |

1. 打开 **设置 → 模型**，点击预设的 **载入**，或填写供应商 ID、Base URL、协议和密钥。
2. 点击 **发现模型**，或手动填写模型 ID。
3. 填写服务实际的上下文窗口和输出上限，保存后运行流式与工具调用检查。

所选的 Windows、WSL 或 SSH 环境必须能访问模型服务。通过中转使用 Claude 时，协议保持 `anthropic-messages`，模型 ID 填 Claude 的模型名（如 `claude-opus-5-5`）。想在自己的 NVIDIA 显卡上跑模型，可在 **设置 → 模型 → Strata 本地模型** 下载 Strata 引擎，载入你的 Qwen3.8 Flash Next GGUF 并设为默认模型。

<a id="privacy"></a>
## 隐私与安全

- **密钥只在你的电脑上**：用 Windows 数据保护（DPAPI）加密保存在本机。
- **不收集使用数据**：没有遥测；崩溃记录只保存在本机，需要时由你自己导出。
- **请求只去你指定的地方**：模型请求只发往你配置的服务，可跟随系统代理或使用你设置的代理；更新和按需组件从 GitHub 下载，并逐片校验 SHA-256。
- **AI 动手前有闸门**：每个工具调用都先经过权限检查，运行命令、调用 MCP 工具和写项目外的文件默认都要你批准。
- **外来内容按不可信处理**：AI 回复里的网络图片不会自动加载，文档预览不运行其中的脚本。
- **发行包可验证**：安装文件首次启动时按签名清单逐个校验。

<a id="whats-new"></a>
## 4.1 新变化

**彻底不再需要 Node.js。** 负责对话、工具、文件和终端的后台在 Windows、WSL 和 SSH 服务器上都换成了 Rust 程序，编辑器的语言服务也改用原生程序（Python 用 ty，JavaScript 和 TypeScript 用 TypeScript 7 原生编译器）。**Strata 本地推理回来了**，在“设置 → 模型”中；每个 MCP 服务器都列出它的工具，可逐个开关。你的对话、设置和密钥原样沿用。[查看完整更新日志 →](CHANGELOG.zh.md)

<a id="docs"></a>
## 文档

| 文档 | 内容 |
|---|---|
| [桌面指南](docs/desktop.zh.md) | 可选组件、更新、SSH、模型、项目记忆和已知限制 |
| [安装与构建指南](docs/desktop-setup.zh-CN.md) | 离线安装、编辑与调试、权限与撤销、从源码构建 |
| [更新日志](CHANGELOG.zh.md) | 每个版本的要点 |
| [验收记录](docs/validation.md) | 每个版本完成的构建、安装和运行检查 |
| [架构](docs/architecture.md) | 进程、Host、协议和数据格式，面向开发者 |

<a id="faq"></a>
## 常见问题

<details>
<summary><b>Rainy 收费吗？</b></summary>
<br>
非商业用途免费：个人学习、研究、业余项目，以及学校、科研机构、公益组织和政府机构，都可以免费使用 RainyMarks 发布的版本。商业用途（包括在公司里用于工作和为付费客户做项目）需要向 RainyMarks 取得商业授权，详见<a href="#license">许可</a>。
</details>

<details>
<summary><b>可以修改 Rainy，或者拿它的代码做自己的项目吗？</b></summary>
<br>
不可以。许可只允许使用，不允许修改、分发，也不允许基于它的代码做新作品。改名、换图标或界面、翻译，或者手工或用 AI 改写代码，都算修改；删除版权声明、把 Rainy 或其中任何部分说成是自己做的，同样不被允许。想提交改进，请先在 <a href="https://github.com/RainyMarks/Rainy/issues">GitHub Issues</a> 说明，得到 RainyMarks 的书面同意后再动手。
</details>

<details>
<summary><b>我的代码和文件会被上传吗？</b></summary>
<br>
Rainy 不收集使用数据。AI 需要的内容只会发给你自己配置的模型服务；联网搜索只在你有 DeepSeek 密钥且没有关闭时出现。
</details>

<details>
<summary><b>必须安装 WSL 吗？</b></summary>
<br>
不需要。Rainy 默认在 Windows 原生环境运行；项目需要 Linux 时，再切换到 WSL2 或一台 SSH 服务器。
</details>

<details>
<summary><b>支持 macOS 或 Linux 桌面吗？</b></summary>
<br>
桌面端目前只支持 Windows 10 / 11 x64。Linux 可以作为 SSH 远程开发的服务器使用。
</details>

<details>
<summary><b>AI 改错了怎么办？</b></summary>
<br>
AI 改过的文件都列在输入框上方，可以在编辑器里逐处撤销，也可以撤销整轮对话的修改。修改前的内容由 Rainy 自己保存（单个文件 16 MB 以内），撤销不依赖 Git。
</details>

<details>
<summary><b>没有网络能用吗？</b></summary>
<br>
可以连接本机的模型服务（如 Ollama）。完整的离线发行包和安装方法见<a href="docs/desktop-setup.zh-CN.md">安装与构建指南</a>。
</details>

<a id="run-from-source"></a>
## 从源码构建

<details>
<summary>展开构建步骤</summary>
<br>

需要 Node.js `^22.19 || >=24`、pnpm `11.7.0`，以及[构建指南](docs/desktop-setup.zh-CN.md#从源码构建)列出的 Windows/WSL 环境。`build-inputs/strata-runtime.tar.gz` 等固定版本的二进制资源由 bootstrap 命令从发行资产恢复，不存放在 Git 中。

```powershell
git clone https://github.com/RainyMarks/Rainy.git
cd Rainy
pnpm install --frozen-lockfile
node scripts/bootstrap-release-inputs.mjs --manifest toolpacks/build-inputs.v1.json
powershell.exe -NoProfile -ExecutionPolicy Bypass -File scripts/package.ps1 -Distribution Ubuntu -ComponentSource release/offline-4.1.1/environment-components
```

只有 WSL 构建发行版名称不同时，才替换命令中的 `Ubuntu`；`4.1.1` 换成 `package.json` 中的版本号。构建原生界面和 Host 还需要 Rust 的 GNU 工具链（`x86_64-pc-windows-gnu`）、MSYS2 UCRT64、`cargo-zigbuild` 和 Zig。从源码构建只限自己非商业使用：构建出的安装包不能分发，修改代码需要 RainyMarks 的书面同意。

日常开发时，`pnpm run dev` 构建并单独启动 Host，打印可在浏览器中打开的地址；界面在 `native/` 里，用 `cargo build -p rainy --offline` 构建，`bash native/scripts/run-ui.sh` 以临时数据目录启动。开发者可从[架构](docs/architecture.md)和 [AGENTS.md](AGENTS.md) 开始。

</details>

## 反馈与支持

- **问题和建议**：提交到 [GitHub Issues](https://github.com/RainyMarks/Rainy/issues)。
- **版本动态**：在 X 上关注 [@RainyMarksqwq](https://x.com/RainyMarksqwq)。
- **觉得好用**：给仓库点个 ⭐，让更多人看到它。

<a id="license"></a>
## 许可

Rainy 自有的代码和文档使用 [PolyForm Strict License 1.0.0](LICENSE.Rainy)（PolyForm 严格许可）。

- **可以免费使用**：个人学习、研究、业余项目等没有商业目的的个人用途，以及公益组织、学校、公共科研机构、公共安全与卫生机构、环保组织和政府机构，可以免费下载、安装和使用 Rainy。
- **不能修改和分发**：无论是否收费，都不能修改 Rainy、基于它的代码做新作品，或者把它（包括修改后的版本）发给别人。改名、换品牌、换图标或界面、翻译、移植，以及手工或用 AI 改写代码或文档，都算修改。也不能删除或改动版权声明、把 Rainy 或其中任何部分说成是自己的作品，或者使用 Rainy 的名称和图标。
- **需要书面授权**：任何商业用途（包括在公司里用于工作和为付费客户开发），以及上面没有允许的其他事情（例如提交代码改进），都需要 RainyMarks 的书面同意。请通过 [GitHub Issues](https://github.com/RainyMarks/Rainy/issues) 或 X 上的 [@RainyMarksqwq](https://x.com/RainyMarksqwq) 联系。

GitHub 的服务条款允许用户在 GitHub 上查看和 fork 本仓库，但这不包含修改或分发的权利。你用 Rainy 打开、编辑和构建的项目，以及它为你生成的内容，不受这份许可约束。

来自 DeepSeek Harness 的界面代码仍按 [MIT 许可](LICENSE.upstream)授权，第三方组件适用各自的许可（见[第三方声明](THIRD_PARTY_NOTICES.md)）。3.8.0 及更早的版本仍按发布时的 Rainy Source Available License 1.0 授权；那份许可同样要求保留版权声明、注明修改，不得把修改版说成官方版本，并写明改名、换界面或换品牌不能免除这些义务。本项目属于源码可用项目，不是 OSI 认可的开源软件。适用范围见 [LICENSE](LICENSE)；以上中文说明只为方便理解，以英文条款为准。

<br>

<div align="center">
<sub>Developed by <a href="https://github.com/RainyMarks">RainyMarks</a></sub>
</div>
