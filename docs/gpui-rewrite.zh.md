# Rainy 4.0：界面改用 GPUI

状态：功能已补齐，作为 4.0 发布（分支 `gpui`，已合入 3.9.2 的 main）。

> 4.1 起后台也换成了 Rust（`native/crates/host`，见下文“4.1：后台改用 Rust”和 [architecture.md](architecture.md)）。下文“后台一行不改”“Host（Node）”描述的是 4.0。

## 目标

- 去掉 Electron（安装包里压缩后约 103 MB 的 Chromium），界面用 Rust + GPUI 直接调显卡绘制。
- 后台（`src/host`，Node）一行不改：它在 Windows、WSL 或 SSH 服务器上运行，界面只是换了一个连接它的客户端。
- 启动更快、内存更少、长对话滚动和打字更顺。

## 结构

```text
native/                       Cargo 工作区，GNU 工具链（x86_64-pc-windows-gnu）
  crates/protocol             rainy_protocol：src/shared/rpc.ts 等协议的 Rust 类型，流式补丁
  crates/carrier              rainy_carrier：启动后台、控制通道、/rpc WebSocket 客户端、
                              API 密钥加密（DPAPI）、系统代理查询（WinHTTP）
  crates/app                  rainy（可执行文件）：GPUI 界面
```

```text
rainy.exe（GPUI）
  │  启动后台：node host.js / wsl.exe … / ssh.exe …，读 stdout 的 RAINY_CONTROL 行
  ▼
Host（Node，不变）  HTTP + WebSocket，127.0.0.1:<随机端口>
  │  GET /?token=… 换 cookie，然后 WebSocket /rpc（JSON：请求、回复、事件）
  ▼
GPUI 界面：聊天、文件树、编辑器、终端、搜索、Git、设置……
```

## 组件选择

| 部分 | 用什么 |
|---|---|
| GPUI | `gpui-pre` 0.3.9（GPUI Kit 维护者发布的 Zed 快照，Apache-2.0），精确锁定版本 |
| 控件 | GPUI Kit 0.8（`gpui-component`，Apache-2.0）：按钮、输入框、列表、树、对话框、Markdown、代码编辑器（tree-sitter + LSP） |
| 着色器 | `runtime_shaders`：运行时编译，不需要 Windows SDK |
| 终端 | `alacritty_terminal`（Apache-2.0）解析，自己用 GPUI 画；不用 Zed 的终端（GPL） |
| 文档预览 | PDF、Word、Excel、PPT、所见即所得 Markdown、mermaid、给 AI 用的内置浏览器：嵌入系统自带的 WebView2（直接用 `webview2-com`，不用 wry：wry 创建时会嵌套消息循环），沿用 3.x 的网页代码；Windows 11 自带 WebView2，只多带一个 WebView2Loader.dll |
| 自动更新 | 自己实现：读 GitHub 发布的 latest.yml，下载、校验 sha512 和签名，再运行 NSIS 安装包 |

不能用 Zed 的 `editor`、`terminal_view` 等 crate：它们是 GPL，和 Rainy 的 PolyForm Strict 许可证冲突。

## 阶段

1. 外壳：窗口、启动后台（先 Windows 原生，再 WSL、SSH）、RPC 连接、断线重连。
2. 聊天：会话列表、发送、流式回复（Markdown、思考块、工具卡）、审批、排队、停止、重试、模型和权限选择。
3. 工作台：文件树、编辑器（含 LSP、AI 补全）、终端、搜索替换、Git、改动审阅、项目地图、命令面板。
4. 设置：模型、网络、外观、安全、技能与 MCP、项目记忆、执行环境切换、SSH 主机。
5. 预览：图片、媒体、十六进制、PDF/Office/Markdown（WebView2）、内置浏览器、导出。
6. 发布：NSIS 安装包、首次启动校验、可选模块、自动更新、右键“用 Rainy 打开”、诊断包。

## 进度（2026-10-22）

已完成并实测：

- 外壳：Windows 原生和 WSL 后台、RPC、断线重连、切换执行环境（重启）。
- 聊天：流式回复、工具卡、审批、排队、停止、重试（失败后和倒计时中）、后台命令栏、AI 改动栏（审查、保留、撤销）、`@` 引用补全、`/compact` `/new`、粘贴/选择/拖入图片、拖入文件引用、连按两次 Esc 停止。
- 工作台：三栏布局、项目菜单、文件树（新建、重命名、删除、刷新）、标签页、编辑器（语法高亮）、Ctrl+S、外部修改冲突、未保存内容恢复（与 3.x 状态文件互通）、图片和十六进制查看、快速打开、搜索替换、Git（暂存、提交、AI 写提交说明、差异视图）、终端（alacritty_terminal，中文输入法、选择复制粘贴、回滚）、状态栏行列号。
- 设置：通用、外观、模型（含编辑器 AI、联网搜索、网络代理）、Skills 与 MCP、运行环境、项目记忆、安全、快捷键。
- 对话增强：思考计时、分叉对话、等待审批时任务栏闪烁、步骤折叠（简洁/详细）、上次对话恢复、回复里的链接（网页用浏览器打开，文件在 Rainy 里打开）。其中思考计时、任务栏闪烁和链接点击还没在窗口里实测。
- 数学公式：latex-rust（纯 Rust，STIX Two Math 字体），`$…$`、`$$…$$`，以及模型常写的 `\(…\)`、`\[…\]`；公式里的中文用系统字体补画；`$5 和 $10` 这类金额不当公式；不支持的写法（如 `\ce{}`）显示原文。

- SSH 远程开发：用系统自带 OpenSSH；连接时在服务器上安装运行环境（服务器自己下载，或从本机上传），再启动后端并转发端口；密码、密钥口令和主机指纹由 Rainy.exe 充当 askpass，在主窗口里询问，同一次运行只问一次；断线后提示重新连接；设置 → 运行环境里添加（先测试连接）、移除、切换主机，并列出 ~/.ssh/config 的别名。用 WSL 里的临时 sshd（127.0.0.1:22022，HOME=/tmp/rainy-ssh-home）实测：口令、主机指纹取消、全新服务器上传安装、命令、终端、断线重连。服务器自己下载基础包（需要发行包里的 optional-modules.json）还没实测。
- 后端修复：同一台服务器的两个 SSH 别名共用 ~/.rainy-agent 时，第二个别名启动会因项目登记冲突而退出；现在只记一条日志，照常启动。
- 文档预览（WebView2）：PDF（含未嵌入字体的中文论文）、Word、Excel、PPT（用本机 PowerPoint/Word/WPS 转成 PDF，照搬 3.x 的 PowerShell 脚本，最近 6 个结果缓存）、Markdown 的所见即所得/源码/预览/分栏（网页里改动同步回编辑器并标记未保存，网页里 Ctrl+S 保存，分栏实时预览），每个文件记住上次的视图。网页由后端提供（3.x 的 ViewerApp，`/?view=…`），登录令牌换成 cookie 后跳到同源地址。标签页过多时自动滚到当前标签。
- AI 内置浏览器：browser_open/read/act/screenshot 实测通过（点击、输入、回车、跳转、控制台错误和失败请求、截图）；只能打开本机地址，页面里的外链、跳转和直接打开外网地址都被拦下。页面固定按 1280×800 桌面尺寸排版（不受屏幕缩放影响），截图也是 1280×800；WebView2 自己请求的 favicon.ico 不算页面错误。
- 窗口叠放：GPUI 默认的 DirectComposition 画面盖在子窗口之上，会把 WebView2 挡住，所以启动时设置 `GPUI_DISABLE_DIRECT_COMPOSITION=1`（没有用到透明或毛玻璃窗口背景，不损失功能）。网页是独立的子窗口，GPUI 画的东西没法盖在它上面：对话框、设置、菜单（标题栏项目菜单、文件树右键菜单等）打开时网页暂时让开，关闭后回来。
- 没实测：网页里按 Ctrl+W、Ctrl+P、Ctrl+反引号等快捷键（测试脚本发不出真实的 Ctrl 状态）、改窗口大小时是否闪烁（截图工具看不出来）、提示气泡被网页挡住（悬停提示不抢焦点，无法用菜单的办法处理）。

- 编辑器语言服务（2026-10-22）：Python、JavaScript/TypeScript、C/C++ 的补全、悬停、诊断（波浪线和问题列表）、转到定义（F12）、查找所有引用（Shift+F12）、重命名符号（F2）、格式化（Shift+Alt+F）；后台的 `/rainy/ide/lsp` WebSocket，与 3.x 同一套服务器。右键菜单是系统原生菜单，测试脚本在后台窗口里打不开它，没实测。
- 编辑器 AI：行内补全（灰字，Tab 采纳，默认关闭，与 3.x 一致）、Ctrl+K 改写选区（绿色底色，Ctrl+Enter 接受，Esc 还原）、发送选区给 AI、问题列表里“用 AI 修复”。
- 运行和调试：标题栏运行按钮、运行方式菜单（按后缀、指定语言、固定的运行配置）、运行配置对话框（参数、环境变量、解释器、C/C++ 单文件或 CMake）；Ctrl+F5 运行、F5 调试（暂停时继续）、Shift+F5 停止、F9 断点、F10/F11/Shift+F11 单步；底部面板分终端、问题、调试三页，调试页有调用栈、线程、断点、变量树、监视和调试控制台，编辑器里标出断点行和暂停行。实测：Python 断点、单步、展开变量、切换栈帧、监视、求值、带参数和环境变量运行。JavaScript、C/C++ 和 PHP 没实测。
- 命令面板（F1、Ctrl+Shift+P）：3.x 的全部命令；项目地图；大纲（Markdown 标题，跟随阅读位置）；Markdown 导出 PDF（隐藏的 WebView2 打印，带康熙部首修正）和 HTML；专注、打字机、源码切换（F8、F9、Ctrl+/）；工作区添加和移除文件夹；WSL 上用 Windows 文件夹（经 wslpath 转换）和浏览执行环境的文件夹。
- 发布：NSIS 安装包（`scripts/package.ps1`）、首次启动校验资源、可选模块下载、自动更新、单实例、窗口位置记忆、诊断包、可选开启的资源管理器“通过 Rainy 打开”。

测试辅助（只在调试版生效）：`RAINY_QA_BACKGROUND` 让窗口在后台打开不抢焦点，`RAINY_QA_EXPORT_DIR` 导出时跳过保存对话框，`RAINY_QA_PICK_FOLDER` 选文件夹时跳过系统对话框。

## 4.1：后台改用 Rust（2026-10-23）

- `rainy-host`（`native/crates/host`，tokio + axum + reqwest + portable-pty + notify）取代 `src/host` 的 Node 后台，Windows、WSL 和 SSH 服务器上都是它；Linux 版在 Windows 上用 cargo-zigbuild 交叉编译（glibc 2.17）。RPC、控制通道和所有数据文件格式不变，3.x/4.0/4.1 互相读写。
- 智能体整个移植：三种模型协议（Chat Completions、Responses、Anthropic Messages，含提示缓存）、重试、流式补丁、工具（含后台命令、子智能体、联网、内置浏览器）、权限、提示词、压缩、项目记忆、技能、MCP（stdio、Streamable HTTP、HTTP+SSE，按工具开关）、编辑器 AI、`@` 补全、Git 提交说明、项目地图讲解。
- 编辑器工具改为原生：ty（Python 语言服务）、TypeScript 7 原生编译器（JS/TS 语言服务和格式化）、Ruff；Biome 不再需要。运行、调试用户自己的 JS/TS 用目标环境装的 Node.js。
- Linux 运行环境底座只剩开发工具（下载约 27 MB），应用层约 11 MB。
- Strata 回归：载体 `strata/`（引擎管理、沿用 3.x 指纹缓存的模型解析、loopback 健康检查），设置 → 模型里的 Strata 卡片。
- 实测（GUI，`run-ui.sh` + `drive.ps1`）：Windows 上对话、命令和写文件工具、MCP 工具面板（开关不重连）、Strata 卡片（模拟服务，连接并设为默认后对话走 Strata）、各设置页、PDF/Word/Excel/PPT 预览、项目地图、运行、断点调试、终端；WSL 上安装运行环境、切换、Strata 连接、对话、打开 Windows 文件夹、终端、运行；SSH 上全新服务器安装、远程文件夹浏览、运行。Host 端到端脚本 37 项（模拟模型 + /rpc）。
- QA 中发现并修好：Windows 上 `npx` 找到 Node 的无扩展名脚本（os error 193），stdio MCP 起不来；请求失败信息里带 URL（可能含密钥）；PDF 预览缺 `/rainy/pdfjs/` 路由；终端读线程里发提示调用 `tokio::spawn` 导致后台崩溃；Strata 卡片在没有组件清单时一直“正在读取”；窄面板里发送按钮被挤出窗口；MCP 工具开关会重连服务器。
- 仍未实测：真实 GPU 上的 Strata 推理、JS/C/C++/PHP 实际调试、地图页内的点击（测试脚本发不进 WebView2 子窗口）。

## 开发

- 后台用 `release/win-unpacked/resources/windows-host` 的副本（放在 `tmp/dev-resources/windows-host`），不要直接用主目录 `runtime/windows-host`：运行中的后台会锁住那个文件夹，导致主目录打包失败。
- 测试时 `RAINY_HOME` 指向临时目录，不碰真实配置。
