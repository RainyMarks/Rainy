# Rainy 4.0：界面改用 GPUI

状态：进行中（分支 `gpui`，工作目录 `Rainy-gpui`）。3.9 发布后把 main 合进来，移植 3.9 新加的界面功能，作为 4.0 合并。

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
| 文档预览 | PDF、Word、Excel、所见即所得 Markdown、mermaid、给 AI 用的内置浏览器：嵌入系统自带的 WebView2（`gpui-webview`），沿用现有网页代码；Windows 11 自带 WebView2，不占安装包体积 |
| 自动更新 | 自己实现：读 GitHub 发布的 latest.yml，下载、校验 sha512 和签名，再运行 NSIS 安装包 |

不能用 Zed 的 `editor`、`terminal_view` 等 crate：它们是 GPL，和 Rainy 的 PolyForm Strict 许可证冲突。

## 阶段

1. 外壳：窗口、启动后台（先 Windows 原生，再 WSL、SSH）、RPC 连接、断线重连。
2. 聊天：会话列表、发送、流式回复（Markdown、思考块、工具卡）、审批、排队、停止、重试、模型和权限选择。
3. 工作台：文件树、编辑器（含 LSP、AI 补全）、终端、搜索替换、Git、改动审阅、项目地图、命令面板。
4. 设置：模型、网络、外观、安全、技能与 MCP、项目记忆、执行环境切换、SSH 主机。
5. 预览：图片、媒体、十六进制、PDF/Office/Markdown（WebView2）、内置浏览器、导出。
6. 发布：NSIS 安装包、首次启动校验、可选模块、自动更新、右键“用 Rainy 打开”、诊断包。

## 进度（2026-10-21）

已完成并实测：

- 外壳：Windows 原生和 WSL 后台、RPC、断线重连、切换执行环境（重启）。
- 聊天：流式回复、工具卡、审批、排队、停止、重试（失败后和倒计时中）、后台命令栏、AI 改动栏（审查、保留、撤销）、`@` 引用补全、`/compact` `/new`、粘贴/选择/拖入图片、拖入文件引用、连按两次 Esc 停止。
- 工作台：三栏布局、项目菜单、文件树（新建、重命名、删除、刷新）、标签页、编辑器（语法高亮）、Ctrl+S、外部修改冲突、未保存内容恢复（与 3.x 状态文件互通）、图片和十六进制查看、快速打开、搜索替换、Git（暂存、提交、AI 写提交说明、差异视图）、终端（alacritty_terminal，中文输入法、选择复制粘贴、回滚）、状态栏行列号。
- 设置：通用、外观、模型（含编辑器 AI、联网搜索、网络代理）、Skills 与 MCP、运行环境、项目记忆、安全、快捷键。

未做：SSH 执行环境、可选模块下载、数学公式渲染、思考计时、分叉对话、审批系统通知、步骤折叠、LSP、运行和调试、命令面板、项目地图、PDF/Office/Markdown 预览（WebView2）、内置浏览器、3.9 的“用 Rainy 打开”和所见即所得 Markdown、诊断包、单实例、窗口位置记忆、自动更新、安装包。

## 开发

- 后台用 `release/win-unpacked/resources/windows-host` 的副本（放在 `tmp/dev-host`），不要直接用主目录 `runtime/windows-host`：运行中的后台会锁住那个文件夹，导致主目录打包失败。
- 测试时 `RAINY_HOME` 指向临时目录，不碰真实配置。
