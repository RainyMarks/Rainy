# Rainy 桌面版

本指南说明离线安装、模型设置、SSH 远程开发及从源码打包。Rainy 是面向 Windows 的轻量 AI 编程工作台，由 Windows Electron 外壳、按执行目标运行的 Host（Windows 原生、WSL 或通过 SSH 连接的 Linux 服务器）和一个 React 页面组成：外壳管理窗口和 Host 生命周期，Host 运行智能体、编辑器服务和文件操作。功能范围和测量结果见 [验收记录](validation.md)。

## 安装与使用

使用 Windows x64，新安装默认原生执行，无需先安装 WSL。核心安装包包含 Windows Host；Strata 引擎、Windows PHP 和 Linux 运行环境是按需下载的可选组件。主模型与 MTP 权重由用户提供。完整离线目录另含 WSL 安装介质、Ubuntu 26.04.1 镜像，以及 Windows/Linux 基础、科学计算 CPU/CUDA 和 Windows C/C++ 组件。已有 Python、Conda、项目虚拟环境或 WSL 可直接检测并复用，不向它们安装软件包。也可连接自行管理的推理服务。

1. 运行 `Rainy-3.1.0-windows-x64-setup.exe` 安装核心。完整离线部署请保留 `environment/`、`environment-components/` 及校验清单。安装包可选择目录，Windows Authenticode 签名情况以最终产物记录为准。
2. 从旧版本升级时，旧版下载的 CTF 工具留在原安装位置（安装目录的 `tools` 文件夹或 `%APPDATA%\RainyAgent\native-tools`），Rainy 3.0 不再管理它们；不再需要时可自行删除这些文件夹。
3. 从桌面 Rainy 快捷方式启动，无需设备码、激活码或授权管理器。已有保存的 WSL 目标继续保留；新安装先使用 Windows。运行环境页可以选择已有 WSL，或点击准备环境并选择外置 `environment/` 目录创建专用 Ubuntu。需要系统组件时明确请求管理员确认；若提示重启，保存其他工作、自行重启后再次打开应用。首次启动会校验全部发行资源，之后启动直接打开；已记录的 WSL 目标跳过环境检查窗口。
4. 安装或更新后首次以 WSL 启动时，应用会联网下载本版本的 Linux 运行环境（约 280 MB），启动页显示进度；发行版中已解包本版本运行环境时不下载。下载失败时可选择“重试”“改用 Windows 原生运行”（记录为执行目标，与在设置中切换相同）或“退出”。
5. 若原发行版丢失，恢复向导保留原名称供处理。执行目标切换前保存草稿，运行中或排队中的任务、程序、调试和终端会阻止切换。项目公开身份与项目记忆在 Windows 与 WSL 之间保留，旧聊天及其运行状态留在原 Host，重新选择原目标即可访问。
6. 通过“文件 → 打开文件夹”创建或打开项目；初次启动不会自动创建桌面项目。“添加文件夹”挂载额外根目录。Windows 原生目标直接使用 Windows 路径；WSL 目标映射至发行版路径。图片和无原生路径的粘贴内容仍走上传流程。
7. 打开顶部设置中的“模型”，可通过 Strata 卡片下载引擎（首次使用时），再选择自己的主模型与配套 MTP 权重并启动。连接其他服务时，填写供应商、服务地址、协议及可选密钥后即可点击“发现模型”，无需预先知道模型 ID 或上下文长度；选定或手填模型 ID，设置实际上下文长度并保存，再执行“验证流式与工具调用”。模型发现不保存配置，也不能替代这项诊断。
8. 新会话使用保存的默认模型。已有会话在原有模型选择器中切换。停止按钮取消当前请求和命令；`/compact` 手动压缩。重启后从项目中重新打开已保存的会话继续处理。

WSL 系统组件安装、管理员确认和 Windows 重启续装按具体发行产物分别验收；控制器模拟测试不能替代这些系统操作。已完成项目与剩余限制见[验收记录](validation.md)。

模型不可用时显示请求错误，由用户选择另一个模型；没有自动云端回退。过载、限流、断线或超时这类临时失败会在倒计时后自动重试最多三次，错误处的“重试”可再次请求而不重发消息。会话标题来自首条消息，不产生标题模型调用。安装版在启动时静默检查 GitHub `RainyMarks/Rainy` 的稳定版并后台下载核心更新；“帮助 → 检查更新”提供手动状态和重试。下载完成后选择“稍后”或“重启安装”。只有确认、草稿保存和 Host 清理完成后才启动安装；保存失败保留窗口，普通退出不会安装。自动更新不选择预发布版或降级，也不重新下载 WSL 介质或科学计算组件；更新后首次以 WSL 或 SSH 启动时会安装新版本的 Linux 运行环境。

窗口控制与单行顶部工具栏共用区域。拖动顶部空白处可移动窗口，双击可最大化或还原；编辑和缩放的键盘快捷键仍可使用。窗口按上次关闭时的大小、位置和最大化状态重新打开；首次启动时若屏幕容纳不下默认大小则直接最大化。

新工作区默认显示文件树和主编辑区；AI 对话与底部终端、输出、问题和调试按需展开。已有工作区保留保存的面板开关，可用“视图 → 专注编辑”收起 AI 和底部面板。文件栏只有一行操作，编辑器操作与文件标签共用一行。顶部统一提供工作区、搜索、设置和面板入口。模型与 Skills/MCP 位于同一设置弹窗的不同页面，关闭时清空尚未保存的密钥输入。主页和“关于”显示 `Developed by RainyMarks`；推理运行时显示“思考中”和已用秒数，完成后显示“已思考 12s”，英文界面为“Thinking...”，计时文字按当前语言显示。

## 编辑、运行与调试

先打开工作区，再从文件树打开文件。CodeMirror 6 编辑器提供完整文本编辑、多标签、查找替换（Ctrl+F / Ctrl+H）、转到行（Ctrl+G）、撤销和内联差异视图；使用保存操作（Ctrl+S）写入磁盘。Python、JavaScript/TypeScript 和 C/C++ 文件有补全、悬停提示、诊断、转到定义（F12）、查找引用（Shift+F12）和重命名（F2）；右键菜单也提供这些操作以及“发送到 AI”。点击行号左侧可切换断点。默认可编辑上限为 5 MiB，二进制、无法无损解码及超限文件只读。超限 UTF-8 文件最多预览 64 KiB。图片、Markdown、SVG、PDF、音视频直接在编辑区预览，Markdown 与 SVG 可切换源码 / 预览 / 分栏（Ctrl+Shift+V、Ctrl+K V）；任何文件都能以十六进制打开，GBK、UTF-16 等文本以只读方式显示。快速打开可搜索尚未展开的文件树路径，结果过多时提示截断。更改查询、切换项目或重新打开窗口会撤销旧结果；等待或搜索失败时不能误打开上一轮的文件。

顶栏中部的项目切换器可切换已有项目，“搜索文件”显示快速打开的快捷键。窗口底部状态栏显示执行环境（如 `Windows`、`WSL · Ubuntu`、`SSH · 主机`）、运行与调试状态、光标行列、选中长度、语言、行尾和编码。

Host 重启后恢复活动工作区，未保存缓冲区按工作区恢复；外部修改与保存版本冲突会保留草稿并要求比较处理。选中代码后可发送到右侧 AI，路径、范围与文本会进入普通用户消息。

在“设置 → 运行环境”检查解释器与库能力，手动选择已有解释器，或从 `environment-components/` 选择匹配目标的 JSON 导入离线组件。Python/Node/PHP 基础、CPU 科学计算、CUDA 科学计算和 Windows C/C++ 分别提供；选中的环境会用于该项目的 Agent 命令、终端、运行及语言服务。科学组件使用配套的 PyTorch、torchvision、torchaudio，CUDA 实算还需要兼容驱动。同一页的“可选组件”可下载或删除 Strata 引擎（约 560 MB）和 PHP 8.5：组件保存在 `%APPDATA%\RainyAgent\modules\<id>`，从 GitHub 发行分片下载，逐片及整体校验 SHA-256，支持断点续传，取消后保留已下载的分片。WSL 的“准备 Ubuntu 开发工具”仅在 Ubuntu 26.04 amd64 明确安装经过校验的 C/C++、GDB、CMake、clangd 与 PHP 软件包，安装时不访问软件源。

运行按钮默认按当前文件后缀选择语言；需要时点击旁边的“运行方式”为该文件改选 Python、JavaScript、TypeScript、PHP、C 或 C++，之后会记住。在 Windows 原生环境运行 PHP 前，请在“设置 → 运行环境 → 可选组件”下载 PHP 8.5（含 json、openssl、mbstring、pdo_sqlite、curl、zip 扩展），或在运行配置中选择已安装的 `php.exe`；未下载时运行 PHP 文件会提示下载位置。已安装的环境组件或项目选定的 PHP 优先；WSL 和 SSH 目标使用该 Linux 系统中安装的 PHP。也可在运行配置中保存当前文件或 Python 模块、参数、工作目录和可选的目标平台绝对解释器路径。支持 Windows 原生、WSL 和 SSH 的 Python、Node、TypeScript、PHP 运行，以及 C/C++ 单文件和 CMake 构建；默认 Debug 构建，失败时停止启动。运行与调试前保存全部修改，失败则中止。Python、JavaScript/TypeScript、C/C++ 调试提供断点、单步、调用栈、变量和监视；PHP 仅运行。此版本只调试由工作区启动的程序。

## SSH 远程开发

SSH 远程开发让智能体、终端、运行调试和语言服务在 Linux 服务器上运行，界面仍在本机，用法类似 VS Code Remote-SSH。

准备工作：

- 本机使用 Windows 自带的 OpenSSH 客户端（`C:\Windows\System32\OpenSSH\ssh.exe`），`~/.ssh/config`、密钥、ssh-agent 和 ProxyJump 都照常有效。缺少时，在 Windows“设置 → 系统 → 可选功能”中添加“OpenSSH 客户端”。
- 服务器需要 x86_64 Linux、`python3`，以及足以运行 Node 22 的 glibc。Rainy 不做系统级安装，所有文件放在服务器的 `~/.rainy-agent` 中。

连接步骤：

1. 打开“设置 → 运行环境 → SSH 远程开发”。`~/.ssh/config` 中的主机会自动列出；其他主机填写 `user@host` 或别名及端口，点击“连接并添加”。Rainy 先测试一次连接（登录、Linux x86_64 和 `python3`），通过后才保存。也可在命令面板中选择“连接到 SSH 主机…”。
2. 首次连接新服务器时，Rainy 显示主机密钥指纹，确认后写入 `~/.ssh/known_hosts`；密钥变化时拒绝连接并说明原因。需要密码、密钥口令或一次性验证码时，Rainy 弹出小窗口询问；输入内容只在本次运行期间保存在内存中，不写入磁盘。
3. 点击“切换到此主机”，或在执行目标列表中选择该主机。Rainy 重启并进入新目标。
4. 首次连接会安装本版本的 Linux 运行环境（归档约 280 MB）：服务器先自行从 GitHub 下载并逐片校验；失败或一分钟后速度仍低于 256 KiB/s 时，改由本机下载后通过 SSH 上传。之后启动直接复用，测试中约 2 秒进入工作台。

使用说明：

- 通过应用内的文件夹浏览器打开服务器上的文件夹；从本机拖入的文件不会映射为服务器路径。
- 解释器和工具请在服务器终端中用 apt、pip、nvm 等安装，然后在“运行环境”页点击“检测已有环境”。
- 模型请求从服务器的网络发出，模型设置和 API 密钥按执行目标分别保存；本机的 Strata 本地模型无法从服务器访问，请配置服务器能访问的模型地址。
- 项目、对话和项目记忆保存在服务器上，切换回本机时不会带过来。
- Host 只监听服务器的 127.0.0.1，通过 `ssh -L` 转发到本机同一端口，不在公网接口上监听。
- 连接断开时，点击“重新连接”会重新打开 Rainy，对话历史保留在服务器上。启动失败时可选择“重试”“改用 Windows 原生运行”或“退出”。

## Strata 本地模型

Strata 引擎不随安装包附带，是约 560 MB 的可选组件，包含 Niko1221/Strata 0.1.39、Python 3.12.14 以及服务、准备脚本和 CUDA 运行依赖；下载后不需要预装 Strata、创建 Python 环境或手工运行转换命令。推理引擎面向 Windows x64 与 NVIDIA CUDA 13，要求 580 或更新的 NVIDIA 驱动，提供 `sm75`、`sm86`、`sm89`、`sm120` 目标；本包不提供 AMD 或 Linux 推理引擎。安装程序不安装显卡驱动。

1. 准备受支持的 Qwen3.8 Flash Next 主模型 GGUF 及全部分片，并准备与主模型匹配的 MTP GGUF 或已准备 MTP 目录。发行包不含主模型、MTP 权重或派生 dense/expert 文件；不要只保留一个不完整分片。
2. 打开“设置 → 模型 → Strata 本地模型”。引擎尚未下载时点击“下载”，也可在“设置 → 运行环境 → 可选组件”下载。选择主 GGUF 的首片、模型目录或兼容 Strata profile。选择配套 MTP 文件或目录；留空仅表示自动检测模型附近的匹配文件，不表示关闭 MTP。
3. 设置引擎上下文长度和本地端口后保存。高级运行设置提供 KV 缓存、保留显存和常驻 RAM 预算。设置写入载体应用数据目录中的私有 `strata/settings.json`，不把开发者机器路径作为默认值，也不移动用户模型权重。
4. 点击“启动本地模型”。首次启动按所选主模型和 MTP 文件在本机离线准备运行数据，再加载服务；过程中可点击“取消准备或启动”。缺失分片、MTP 不匹配或运行组件缺失时按错误提示处理，不会下载模型或切换到云端。
5. 状态就绪后点击“连接并设为默认”。当前 Host 验证实际模型及上下文，保存为本地 Chat Completions 配置；推理档位和请求输出上限继续在上方普通模型配置中调整。仅本应用启动的进程可被停止，已有外部服务不会被终止。

Strata 服务在 Windows loopback 上运行。WSL Host 只有在能访问该地址时才可连接；NAT 导致不可达时，在“运行环境”中切换到 Windows 后使用 Strata。SSH 服务器上的 Host 无法使用本机的 Strata。应用不修改防火墙或 WSL 网络配置。

Strata 运行时已进行独立路径迁移、Python 导入和模拟健康/聊天接口检查；真实 GPU 推理和性能尚未在本次发行验收中确认。具体产物、结果与剩余限制见[验收记录](validation.md)，运行时可导入不等于模型推理已通过。

## 模型设置

本地模型填入服务实际的 Base URL、模型 ID、已配置上下文窗口和输出上限。100,000 窗口默认预留 16,000 输出和 8,000 余量，输入上限为 76,000，约 70,000 时触发压缩。服务没有密钥时留空；已有密钥留空保存表示保留。API 密钥写入所选 Host 的私有凭据文件，并用 Windows 数据保护（DPAPI）为当前账户保管的钥匙加密，不写入模型配置、源码或状态接口。

地址必须从所选执行目标能够访问。Windows 目标可直接使用本机服务的 localhost；WSL 内服务通常使用 `http://127.0.0.1:端口/v1`。Windows 服务在 WSL 镜像网络下可能共享 localhost，NAT 下需要实际可达的 Windows 地址及既有访问许可。SSH 目标的请求从服务器发出，地址须从服务器可达。Rainy 不修改防火墙或 WSL 网络配置，以连接测试为准。

新建 API 模型默认使用 OpenAI Responses，本地模型默认使用 OpenAI-compatible Chat Completions；Anthropic Messages 和手动指定的协议继续可用。Chat Completions 可选择 `max_tokens` 或 `max_completion_tokens`，以及 OpenAI、DeepSeek、Qwen 推理格式。推理档位必须与实际服务能力一致；不支持工具调用的模型在诊断中显示“仅确认聊天可用”。诊断使用已保存的配置，并只提交一个无副作用的工具 schema。已有模型配置保留原协议，切换时在设置中保存一次。

应用的 DeepSeek V4.1 Flash 预设使用模型 ID `deepseek-flash`，地址 `https://api.deepseek.com`，窗口 1,000,000，最大输出 393,216，推理 `max`，协议为 Responses。本地窗口的默认输出限制不会覆盖这个显式 API 配置。来源：[模型列表](https://api-docs.deepseek.com/api/list-models/)、[Responses API](https://api-docs.deepseek.com/guides/responses_api/)、[推理与工具调用](https://api-docs.deepseek.com/guides/thinking_mode/)。

请求预算覆盖系统提示、工具定义、项目指令、记忆和历史，发送前使用保守的多语言估计，并按最近 5 次请求服务商报告的实际用量校正低估；输入框旁的上下文占用显示估算值，或上一请求由服务商返回的实际用量。工具输出超限时保存完整文本，只把有限的首尾片段和文件位置送给模型。read 使用自己的行窗口，默认上限为 2,000 行、每行 2,000 字符和 50 KiB；超长行有明确截断标记且原文件不变。达到压缩阈值时，较早的一段对话由当前模型摘要替换：最新的用户请求保留原文，工具调用与结果不会被拆开；摘要逐字保留仍然有效的用户指令，记录已验证的进度、准确路径、错误、待办和下一步。上一请求的缓存仍有效时，摘要请求复用该请求的完整前缀，大部分输入按缓存读取计价；展开压缩记录可查看所用请求和 token 用量。上下文溢出时至多压缩并重试一次。输入 `/compact` 可随时手动压缩。摘要失败时保留原记录；被摘要的原文仍保存在对话文件中。

发往同一服务地址的请求按到达顺序排队：本机回环地址（localhost、`*.localhost`、127.0.0.1、[::1]）一次一个，其他地址最多同时四个。

“设置 → 模型 → 网络代理”决定模型请求如何连到服务：“跟随系统”（默认）、“不使用代理”，或“手动设置”HTTP 代理地址（如 `127.0.0.1:7890`，可带 `用户名:密码@`；不支持 SOCKS 代理，已保存的密码显示为 `***`）。跟随系统时，Windows Host 会针对每个服务询问 Windows 使用哪个代理，因此代理设置、“不使用代理的地址”列表和 PAC 脚本都会生效，系统设置变化后 30 秒内生效。在 WSL 中，“跟随系统”使用 WSL 传给 Host 的 `HTTPS_PROXY`、`HTTP_PROXY` 和 `NO_PROXY` 变量，不读取 `~/.bashrc` 等 shell 配置文件；需要代理时请选“手动设置”。“测试连接”在保存前用所选方式访问当前模型的 Base URL。模型发现和 HTTP 方式的 MCP 服务器使用同一代理，修改设置后连接失败的 MCP 服务器会自动重连。本机、局域网、`.local` 和不带点的主机名（如 `gpu-box`）始终直连，AI 运行的命令和终端不走代理。

在“设置 → 模型 → 全局提示词”中填写希望所有会话都遵守的要求（例如“始终用中文回答”），保存后立即对所有会话生效，并计入上下文预算；默认最多 4,000 字，留空即关闭。

## 项目记忆

项目记忆按载体的项目身份保存，Windows 与 WSL 共享项目记录，解释器、shell 等环境事实保留各自执行端范围；SSH 目标的项目记忆保存在服务器上。“使用记忆”和“自动生成”分别开关；自动生成会额外产生模型请求费用，新项目默认关闭，已有记忆记录的项目保持原设置；支持查看、编辑、删除和清空。编辑携带页面显示的版本号，冲突时需刷新；后台生成不会覆盖人工修正。删除保留源事件水位及排除摘要，避免旧会话再次生成已删除内容。

任务完成后空闲 60 秒触发，每项目最多 10 分钟一次。后台使用已选择模型，不调用工具，不自动切换云端；完整输入最多 4,096 tokens、输出最多 512 tokens，30 秒超时，前台新输入会取消它。有效记忆总量最多 1,024 估算 tokens，新聊天自动读取最多 512 tokens 且不超过输入预算的 5%，包含来源和说明。生成失败、取消或版本冲突均保留旧记录。记忆作为带来源的历史资料进入聊天，不拥有指令或授权地位。源文件变化会使相关自动事实失效。

项目指令（`AGENTS.md`、`CLAUDE.md` 及其 `.local` 版本）合计上限为 64 KiB，在对话首次请求前加入一次；智能体进入子目录工作时再加入该目录的指令。“设置 → 模型”的上下文预算卡片分项显示系统提示、工具定义、项目指令、记忆和历史的估算值；预览读取当前项目与已保存模型，不创建聊天、不调用模型。

## AI 权限与撤销

每个对话可以在输入框旁切换权限：“只读”只能读取和搜索文件；“询问”（默认）在项目内直接读写，运行命令、调用 MCP 工具和写入项目外的文件前先显示审批条，可选择允许、总是允许或拒绝，拒绝时可以告诉 AI 改用什么做法；“完全自动”不再询问。“总是允许”按项目记录命令前缀或 MCP 工具，只匹配不含 `;`、`|`、`>`、`$(` 等的简单命令，可在“设置 → 安全”中查看和移除，也可在那里设置新对话的默认权限。等待批准时窗口不在前台会发出系统通知，历史列表显示“等待批准”。

AI 的写入和编辑会保存修改前的内容（单个文件不超过 16 MiB，随对话删除，30 天后清理）。每张改动卡片可“撤销此修改”，每轮可“撤销本轮修改”；文件之后又被改动过时不会覆盖。命令行造成的改动不在撤销范围内。

## Skills 与 MCP

Skills 无需手动启用：项目 Skills 位于 `.rainy/skills/<名称>/SKILL.md`，用户 Skills 位于 `~/.rainy-agent/skills/<名称>/SKILL.md`。系统提示中只放 Skill 的名称、短描述和文件路径（合计最多 4,096 tokens），正文由读取工具按需加载。“设置 → Skills 与 MCP”列出当前项目读到的 Skills。

MCP 服务在同一页面统一添加、编辑、启用或删除，对所有对话生效。每个服务可选择 stdio（命令、参数、环境变量）或 streamable HTTP（URL 和请求头，每行一个 `名称: 值`，例如 `Authorization: Bearer …`）。环境变量和请求头保存在 `settings.json` 中，不在保存模型密钥的凭据存储中。工具以 `mcp__<服务>__<工具>` 的名称提供给模型，单次调用 60 秒超时；服务提供的说明会加入系统提示。当前不提供插件市场或 MCP OAuth 登录。

## 从源码构建

构建需要 Windows Node `^22.19 || >=24`、仓库锁定的 pnpm、Git，以及所选 WSL2 中的 Python 3.12+ 和 HTTPS 访问。环境介质的签名验证需要 GnuPG，可通过 `RAINY_GPG` 指定 `gpg.exe`。目标平台专用的 npm 包按锁文件的 SHA-512 校验，Node 发行包按官方 SHA-256 校验。

日常开发在仓库根目录运行 `pnpm run build`（只构建外壳与 Host 时用 `pnpm run build:host`），输出 `dist/main.cjs`、`dist/preload.cjs`、`dist/setup/`、`dist/host.js` 和 `dist/renderer/`（含编辑器分块与 `THIRD_PARTY_NOTICES.txt`）。`pnpm run dev` 构建后以 `tmp/dev-home` 为数据目录直接启动 Host 并打印工作台地址，`--no-build`、`--home <目录>`、`--port <端口>` 可调整，Ctrl+C 通过控制行停止 Host。`pnpm start` 从仓库根目录启动 Electron，Windows 原生目标需要先由 `scripts/stage-windows.mjs` 暂存 `runtime/windows-host`。`pnpm run test` 运行 Vitest，`pnpm run test:node` 运行 `tests/scripts` 中的 node:test 测试。开发和测试时可用环境变量 `RAINY_SSH_CONFIG` 让 ssh 读取另一份配置文件。

从仓库根目录运行：

```powershell
pnpm install --frozen-lockfile
node scripts/bootstrap-release-inputs.mjs --manifest toolpacks/build-inputs.v1.json
powershell.exe -NoProfile -ExecutionPolicy Bypass -File scripts/package.ps1 -Distribution Ubuntu -ComponentSource release/offline-3.1.0/environment-components
```

[bootstrap-release-inputs.mjs](../scripts/bootstrap-release-inputs.mjs)按仓库中的[输入清单](../toolpacks/build-inputs.v1.json)从固定版本的资源存档下载发行分片、重组并校验原始文件，恢复 Git 不保存的 IDE、Strata、WSL 和运行环境输入；其中 `build-inputs/strata-runtime.tar.gz` 会展开到 `resources/strata-runtime`，供源码检出直接运行时使用，安装包不包含该目录。已有完整输入目录可通过 `--inputs-dir` 指定，仍需通过清单校验。打包入口 [package.ps1](../scripts/package.ps1)校验环境媒体和组件归档，运行 `build.ts --release`，由 `runtime-graph.mjs` 从 `node_modules/.pnpm` 计算 Host 外部依赖（node-pty、ripgrep、Pyright、TypeScript 语言服务、TypeScript、Prettier、tsx）的生产依赖闭包，分别生成 Windows 与 Linux Host，再由 `prepare-shell.mjs` 暂存 `build/shell` 并生成 NSIS 安装包。`-ComponentSource` 指向恢复到 `release/offline-3.1.0` 的 `environment-components`。`-Distribution` 必须匹配准备好的 WSL 构建发行版名称。最终产物与干净 checkout 的实测情况由[验收记录](validation.md)记录，以上命令不代表已经完成该项验收。

Windows Host 构建先按官方 SHA-256 校验 Node ZIP，再使用 Windows 随附的 .NET ZIP 解压器展开；这一环节无需额外安装压缩工具。

生产构建使用独立 Ed25519 发行密钥。默认在忽略提交的 `build/release-signing-key.pem` 创建并复用本机构建密钥；`RAINY_RELEASE_SIGNING_KEY` 可指定已有私钥文件，路径缺失、格式不符或与暂存公钥不匹配时构建失败。公开资源只包含发行公钥，载体将该公钥嵌入并校验签名资源清单。该流程不读取客户授权数据库。

完整离线目录为 `release/offline-3.1.0/`，包含核心安装程序、`environment/` WSL 介质、`environment-components/` 独立运行环境及递归 SHA-256 清单。WSL 镜像、科学计算大依赖、Strata 引擎、PHP 组件和 Linux 运行环境都位于核心 EXE 之外；安装包只附带固定这三个可选组件归档的签名资源 `optional-modules.json`。构建核对 IDE 固定来源、Linux APT 索引和组件逐文件摘要；Windows Host 使用实体依赖文件，Linux Host 使用包内相对链接。构建暂存位于 `runtime/` 与 Linux `/var/tmp`，最终容量和验收以本次产物报告为准。

## 诊断信息

窗口崩溃时会在本机保存崩溃记录，不会上传。遇到问题时，在“设置 → 通用 → 诊断信息”点击“导出…”，把生成的 zip 附在反馈里即可：其中有版本和系统信息、日志末尾和最近 5 份崩溃记录，日志里的密钥、令牌、密码和 Windows 用户名已被隐去，不含设置、对话和 API 密钥。报错、对话框和菜单使用“设置 → 通用”里选择的界面语言。

## 已知限制和后续工作

当前发布目标是 Windows x64，可选择 Windows 原生、WSL2 或通过 SSH 连接的 x86_64 Linux 服务器。应用提供可下载的 Strata 引擎和离线运行环境，模型权重与兼容显卡驱动由使用者准备，启动模型需要明确操作。程序运行、断点调试、CPU/CUDA 组件导入与计算，以及各模型端点按具体产物分别验收。没有精确 tokenizer 的服务使用估计，其误差会影响压缩时机。

安装包的 Windows Authenticode 签名状态和更新验收以本次发行记录为准。组件许可与归属见 [第三方说明](../THIRD_PARTY_NOTICES.md)。
