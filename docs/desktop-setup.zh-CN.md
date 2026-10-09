# RainyAgent 桌面版

本指南说明离线安装、常用工具、模型设置及从源码打包。RainyAgent 复用固定版本 DSH 的 Agent 循环、会话协议和共享 Web 界面；Windows Electron 管理窗口、Windows 原生或 WSL Host 生命周期及供人操作的 Windows 工具。功能范围和测量结果见 [验收记录](validation.md)。

## 安装与使用

使用 Windows x64，新安装默认原生执行，无需先安装 WSL。核心安装包包含 Windows Host；Strata 引擎、Windows PHP 和 WSL Linux 运行环境是按需下载的可选组件，常用工具也按工具单独下载。主模型与 MTP 权重由用户提供。完整离线目录另含 WSL 安装介质、Ubuntu 26.04.1 镜像，以及 Windows/Linux 基础、科学计算 CPU/CUDA 和 Windows C/C++ 组件。已有 Python、Conda、项目虚拟环境或 WSL 可直接检测并复用，不向它们安装软件包。也可连接自行管理的推理服务。

1. 运行 `RainyAgent-1.0.10-windows-x64-setup.exe` 安装核心。完整离线部署请保留 `environment/`、`environment-components/`、全部 `rainy-unit-*.tar.gz` 工具归档及校验清单。工具归档为可选项：放在安装程序旁时，安装程序会安装其中每个单元；不带归档更新时保留已安装工具，之后可在“常用工具”中按工具下载。安装包可选择目录，Windows Authenticode 签名情况以最终产物记录为准。
2. 提供工具归档时，工具安装窗口会校验文件和目标空间再安装。点击取消后等待安全停止；使用同一组归档重新运行安装程序可重试并复用已验证暂存。归档损坏时应换成匹配文件后重试。
3. 从桌面 RainyAgent 快捷方式启动，无需设备码、激活码或授权管理器。已有保存的 WSL 目标继续保留；新安装先使用 Windows。运行环境页可以选择已有 WSL，或点击准备环境并选择外置 `environment/` 目录创建专用 Ubuntu。需要系统组件时明确请求管理员确认；若提示重启，保存其他工作、自行重启后再次打开应用。首次启动会校验全部发行资源，之后启动直接打开；已记录的 WSL 目标跳过环境检查窗口。
4. 安装或更新后首次以 WSL 启动时，应用会联网下载本版本的 WSL 运行环境（约 340 MB），启动页显示进度；发行版中已解包本版本运行环境时不下载。下载失败时可选择“重试”“改用 Windows 原生运行”（记录为执行目标，与在设置中切换相同）或“退出”。
5. 若原发行版丢失，恢复向导保留原名称供处理。执行目标切换前保存草稿，运行中或排队中的任务、程序、调试和终端会阻止切换。项目公开身份与项目记忆跨目标保留，旧聊天及其运行状态留在原 Host，重新选择原目标即可访问。
6. 通过“文件 → 打开文件夹”创建或打开项目；初次启动不会自动创建桌面项目。“添加文件夹”挂载额外根目录。Windows 原生目标直接使用 Windows 路径；WSL 目标映射至发行版路径。图片和无原生路径的粘贴内容仍走上传流程。
7. 打开顶部设置中的“模型”，可通过 Strata 卡片下载引擎（首次使用时），再选择自己的主模型与配套 MTP 权重并启动。连接其他服务时，填写供应商、服务地址、协议及可选密钥后即可点击“发现模型”，无需预先知道模型 ID 或上下文长度；选定或手填模型 ID，设置实际上下文长度并保存，再执行“验证流式与工具调用”。模型发现不保存配置，也不能替代这项诊断。
8. 新会话使用保存的默认模型。已有会话在原有模型选择器中切换。停止按钮取消当前请求和命令；`/compact` 手动压缩。重启后从项目中重新打开已保存的会话继续处理。

WSL 系统组件安装、管理员确认和 Windows 重启续装按具体发行产物分别验收；控制器模拟测试不能替代这些系统操作。已完成项目与剩余限制见[验收记录](validation.md)。

模型不可用时显示请求错误，由用户选择另一个模型；没有自动云端回退。会话标题来自首条消息，不产生标题模型调用。安装版在启动时静默检查 GitHub `RainyMarks/RainyAgent` 的稳定版并后台下载核心更新；“帮助 → 检查更新”提供手动状态和重试。下载完成后选择“稍后”或“重启安装”。只有确认、草稿保存和 Host 清理完成后才启动安装；保存失败保留窗口，普通退出不会安装。自动更新不选择预发布版或降级，也不重新下载常用工具、WSL 介质或科学计算组件；更新后首次以 WSL 启动时会下载新版本的 WSL 运行环境。用安装程序离线安装工具前，请关闭相关工具窗口；安装程序检查占用，不自动结束这些进程。

工具升级在继续提供的工具目录保留已声明的个人设置和用户新增文件，并依据已安装版本的文件清单移除新版不再包含的旧程序文件。安装会把被替换或移除的工具目录移入 `.rainy-toolpack/backups/<transactionId>/`；安装提交后，路径、大小和 SHA-256 与已保存版本清单一致的备份文件会被删除，只留下用户文件和有改动的文件供手动恢复。启动约 60 秒后，应用对旧版本留下的备份执行同样的清理，从而释放旧版占用的数 GB 备份；未完成的切换或回滚会保留全部备份。旧清单之外的目录保持不动。用户路径与新版冲突或旧清单缺失时，安装程序在替换前停止并指出相关路径。`.rainy-toolpack` 保留版本清单、剩余备份和恢复记录；卸载应用会保留 `tools/`、`runtime/` 及用户数据。

窗口控制与单行顶部工具栏共用区域。拖动顶部空白处可移动窗口，双击可最大化或还原；编辑和缩放的键盘快捷键仍可使用。窗口按上次关闭时的大小、位置和最大化状态重新打开；首次启动时若屏幕容纳不下默认大小则直接最大化。

新工作区默认显示文件树和主编辑区；AI 对话与底部终端、输出、问题和调试按需展开。已有工作区保留保存的面板开关，可用“视图 → 专注编辑”收起 AI 和底部面板。文件栏只有一行操作，编辑器操作与文件标签共用一行，CTF 工具按需打开为可关闭的标签。顶部统一提供工作区、搜索、设置和面板入口。模型与 Skills/MCP 位于同一设置弹窗的不同页面，关闭时清空尚未保存的密钥输入。主页和“关于”显示 `Develop by NCUCyberBase`；推理运行时显示“思考中”，英文界面为“Thinking...”，计时文字按当前语言显示。

## 编辑、运行与调试

先打开工作区，再从文件树打开文件。Monaco 提供完整文本编辑、多标签、查找替换、撤销和差异视图；使用保存操作写入磁盘。默认可编辑上限为 5 MiB，二进制、无法无损解码及超限文件只读。超限 UTF-8 文件最多预览 64 KiB，二进制以十六进制最多预览 4 KiB。快速打开可搜索尚未展开的文件树路径，结果过多时提示截断。更改查询、切换项目或重新打开窗口会撤销旧结果；等待或搜索失败时不能误打开上一轮的文件。

顶栏中部的项目切换器可切换已有项目，“搜索文件”显示快速打开的快捷键。窗口底部状态栏显示执行环境、运行与调试状态、光标行列、选中长度、语言、行尾和编码。

Host 重启后恢复活动工作区，未保存缓冲区按工作区恢复；外部修改与保存版本冲突会保留草稿并要求比较处理。选中代码后可发送到右侧 AI，路径、范围与文本会进入普通用户消息。

在“设置 → 运行环境”检查解释器与库能力，手动选择已有解释器，或从 `environment-components/` 选择匹配目标的 JSON 导入离线组件。Python/Node/PHP 基础、CPU 科学计算、CUDA 科学计算和 Windows C/C++ 分别提供；选中的环境会用于该项目的 Agent 命令、终端、运行及语言服务。科学组件使用配套的 PyTorch、torchvision、torchaudio，CUDA 实算还需要兼容驱动。同一页的“可选组件”可下载或删除 Strata 引擎（约 560 MB）和 PHP 8.5：组件保存在 `%APPDATA%\RainyAgent\modules\<id>`，从 GitHub 发行分片下载，逐片及整体校验 SHA-256，支持断点续传，取消后保留已下载的分片。WSL 的“准备 Ubuntu 开发工具”仅在 Ubuntu 26.04 amd64 明确安装经过校验的 C/C++、GDB、CMake、clangd 与 PHP 软件包，安装时不访问软件源。

运行按钮默认按当前文件后缀选择语言；需要时点击旁边的“运行方式”为该文件改选 Python、JavaScript、TypeScript、PHP、C 或 C++，之后会记住。在 Windows 原生环境运行 PHP 前，请在“设置 → 运行环境 → 可选组件”下载 PHP 8.5（含 json、openssl、mbstring、pdo_sqlite、curl、zip 扩展），或在运行配置中选择已安装的 `php.exe`；未下载时运行 PHP 文件会提示下载位置。已安装的环境组件或项目选定的 PHP 优先；WSL 目标使用发行版自身的 PHP。也可在运行配置中保存当前文件或 Python 模块、参数、工作目录和可选的目标平台绝对解释器路径。支持 Windows 原生与 WSL 的 Python、Node、TypeScript、PHP 运行，以及 C/C++ 单文件和 CMake 构建；默认 Debug 构建，失败时停止启动。运行与调试前保存全部修改，失败则中止。Python、JavaScript/TypeScript、C/C++ 调试提供断点、单步、调用栈、变量和监视；PHP 仅运行。此版本只调试由工作区启动的程序。

## Strata 本地模型

Strata 引擎不随安装包附带，是约 560 MB 的可选组件，包含 Niko1221/Strata 0.1.39、Python 3.12.14 以及服务、准备脚本和 CUDA 运行依赖；下载后不需要预装 Strata、创建 Python 环境或手工运行转换命令。推理引擎面向 Windows x64 与 NVIDIA CUDA 13，要求 580 或更新的 NVIDIA 驱动，提供 `sm75`、`sm86`、`sm89`、`sm120` 目标；本包不提供 AMD 或 Linux 推理引擎。安装程序不安装显卡驱动。

1. 准备受支持的 Qwen3.8 Flash Next 主模型 GGUF 及全部分片，并准备与主模型匹配的 MTP GGUF 或已准备 MTP 目录。发行包不含主模型、MTP 权重或派生 dense/expert 文件；不要只保留一个不完整分片。
2. 打开“设置 → 模型 → Strata 本地模型”。引擎尚未下载时点击“下载”，也可在“设置 → 运行环境 → 可选组件”下载。选择主 GGUF 的首片、模型目录或兼容 Strata profile。选择配套 MTP 文件或目录；留空仅表示自动检测模型附近的匹配文件，不表示关闭 MTP。
3. 设置引擎上下文长度和本地端口后保存。高级运行设置提供 KV 缓存、保留显存和常驻 RAM 预算。设置写入载体应用数据目录中的私有 `strata/settings.json`，不把开发者机器路径作为默认值，也不移动用户模型权重。
4. 点击“启动本地模型”。首次启动按所选主模型和 MTP 文件在本机离线准备运行数据，再加载服务；过程中可点击“取消准备或启动”。缺失分片、MTP 不匹配或运行组件缺失时按错误提示处理，不会下载模型或切换到云端。
5. 状态就绪后点击“连接并设为默认”。当前 Host 验证实际模型及上下文，保存为本地 Chat Completions 配置；推理档位和请求输出上限继续在上方普通模型配置中调整。仅本应用启动的进程可被停止，已有外部服务不会被终止。

Strata 服务在 Windows loopback 上运行。WSL Host 只有在能访问该地址时才可连接；NAT 导致不可达时，在“运行环境”中切换到 Windows 后使用 Strata。应用不修改防火墙或 WSL 网络配置。

Strata 运行时已进行独立路径迁移、Python 导入和模拟健康/聊天接口检查；真实 GPU 推理和性能尚未在本次发行验收中确认。具体产物、结果与剩余限制见[验收记录](validation.md)，运行时可导入不等于模型推理已通过。

## 模型设置

本地模型填入服务实际的 Base URL、模型 ID、已配置上下文窗口和输出上限。100,000 窗口默认预留 16,000 输出和 8,000 余量，输入上限为 76,000，约 70,000 时触发压缩。服务没有密钥时留空；已有密钥留空保存表示保留。API 密钥写入所选 Host 的私有凭据文件，不写入模型配置、源码或状态接口。

地址必须从所选执行目标能够访问。Windows 目标可直接使用本机服务的 localhost；WSL 内服务通常使用 `http://127.0.0.1:端口/v1`。Windows 服务在 WSL 镜像网络下可能共享 localhost，NAT 下需要实际可达的 Windows 地址及既有访问许可。Rainy 不修改防火墙或 WSL 网络配置，以连接测试为准。

新建 API 模型默认使用 OpenAI Responses，本地模型默认使用 OpenAI-compatible Chat Completions；Anthropic Messages 和手动指定的协议继续可用。Chat Completions 可选择 `max_tokens` 或 `max_completion_tokens`，以及 OpenAI、DeepSeek、Qwen 推理格式。推理档位必须与实际服务能力一致；不支持工具调用的模型在诊断中显示“仅确认聊天可用”。诊断使用已保存的配置，并只提交一个无副作用的工具 schema。已有模型配置保留原协议，切换时在设置中保存一次。

应用的 DeepSeek V4.1 Flash 预设使用模型 ID `deepseek-flash`，地址 `https://api.deepseek.com`，窗口 1,000,000，最大输出 393,216，推理 `max`，协议为 Responses。本地窗口的默认输出限制不会覆盖这个显式 API 配置。来源：[模型列表](https://api-docs.deepseek.com/api/list-models/)、[Responses API](https://api-docs.deepseek.com/guides/responses_api/)、[推理与工具调用](https://api-docs.deepseek.com/guides/thinking_mode/)。

请求预算覆盖系统提示、工具定义、项目指令、扩展和历史。缺少服务端 tokenizer 时使用保守的多语言估计，并用实际 usage 向上校准；界面明确标为估计。其他工具结果超限后保存完整文本，只把有限首尾片段和原文位置送给模型。read 使用自己的行窗口，不对普通文件或 spill 文件读取结果再次生成 spill；默认上限为 2,000 行、每行 2,000 字符和 50 KiB 所选文本。完整请求连同包装仍受总输入预算限制，超限在供应商调用前拒绝。超长行有明确截断标记且原文件不变；offset/limit 不能分页读取单行后半。摘要使用当前模型，完整辅助请求包含保留的直接用户原文作为准确参考，并按同一完整输入计量。摘要指令只记录已验证的进度、错误、待办和下一步，不改写用户目标或约束；参考原文不进入替换范围。失败保留原记录。最近一个已接纳输入的轮次中，直接用户消息（包括该轮中途补充的输入）在当前表层原位保留全文；新轮次接纳首条用户消息后才切换保护范围。更早轮次仍可摘要，已经遮蔽的原文不会回填。自动、空闲手动和显式范围压缩均保留这些当前轮消息；受保护输入本身超限会明确报错。上下文溢出至多压缩重试一次，仍超限就停止发送。原始会话日志不会被摘要替换或删除。

高级计数配置位于 `~/.rainy-agent/profiles/rainy/cordis.patch.yml` 的 `rainy-policy` 条目，可设置 `tokenizers: { local: "http://127.0.0.1:端口/count" }`。该自定义接口接收 `provider/model/system/messages/tools`，必须按实际部署的聊天模板计数，POST 返回 `{ "tokens": 1234, "model": "当前模型", "chatTemplate": "模板版本" }`；模型不匹配、模板标识缺失或计数失败时回落到估计。通用 `/tokenize` 接口不能未经适配直接替代。共享同一服务但地址不同的供应商可用 `endpointGroups` 指向同一个队列键。每个队列按到达顺序发送请求：本机回环地址（localhost、`*.localhost`、127.0.0.1、[::1]）默认一次一个，其他端点默认最多同时四个；`localEndpointConcurrency` 与 `remoteEndpointConcurrency` 修改这两个默认值，`endpointConcurrency` 按队列键（分组名、Base URL 的 origin 或供应商 ID）单独设置。分组中任一供应商使用回环地址时整组按本机处理；局域网或其他非回环地址上的单路服务需在 `endpointConcurrency` 中设为 1。

在“设置 → 模型 → 全局提示词”中填写希望所有会话都遵守的要求（例如“始终用中文回答”），保存后立即对所有会话生效，并计入上下文预算；默认最多 4,000 字，留空即关闭。

## 项目记忆

项目记忆按载体的项目身份保存，Windows 与 WSL 共享项目记录，解释器、shell 等环境事实保留各自执行端范围。“使用记忆”和“自动生成”分别开关；支持查看、编辑、删除和清空。编辑携带页面显示的版本号，冲突时需刷新；后台生成不会覆盖人工修正。删除保留源事件水位及排除摘要，避免旧会话再次生成已删除内容。

任务完成后空闲 60 秒触发，每项目最多 10 分钟一次。后台使用已选择模型，不调用工具，不自动切换云端；完整输入最多 4,096 tokens、输出最多 512 tokens，30 秒超时，前台新输入会取消它。有效记忆总量最多 1,024 估算 tokens，新聊天自动读取最多 512 tokens 且不超过输入预算的 5%，包含来源和说明。生成失败、取消或版本冲突均保留旧记录。后台调用的确切输入、结果和用量进入日志；记忆作为带来源的历史资料进入聊天，不拥有指令或授权地位。源文件变化会使相关自动事实失效。

项目指令的单源与完整批次上限为 64 KiB；超限会指明文件并停止发送，不静默截断必要要求。修正动态指令后会在下一请求前重新检查。上下文页将系统提示、工具定义、项目指令、记忆、扩展、历史与协议包装分项显示为估算；专用计数服务确认当前模型与模板后才标记其总数为实测。首发预览读取当前项目与已保存模型，不创建聊天、不调用模型；未提交附件与发送时才发生的变更不在预览中。

## Skills 与 MCP

所有扩展默认关闭。从顶部设置切换到“Skills 与 MCP”，确认会话并读取配置；从已打开的会话进入时默认选中该会话。项目 Skills 位于 `.rainy/skills/<名称>/SKILL.md`，用户 Skills 位于 `~/.rainy-agent/skills/<名称>/SKILL.md`。只将已选择 Skill 的短描述与原文读取路径加入系统提示，正文由现有文件读取工具按需加载。扩展说明和工具定义合计不得超过配置上限或模型输入预算的 20%。

显式启用示例：

```json
{
  "skills": ["project/style-guide"],
  "servers": [
    {
      "serverName": "project-tools",
      "transport": "stdio",
      "command": "/absolute/path/to/server",
      "args": [],
      "tools": ["selected_tool"]
    }
  ]
}
```

MCP 也支持 `streamable-http` 与 `url`。只向该会话注册 `tools` 中列出的工具；撤销选择会关闭其连接。会话恢复时重新加载它明确保存的选择。当前界面使用 JSON 配置，不提供插件市场或 MCP OAuth 登录。

## CTF 工具

工具按需逐个下载。“常用工具”顶部显示“已下载 N / M 款工具”；未下载工具的卡片显示“下载 · 大小”，已下载工具显示“打开”和“移除”，移除需再次点击确认。顶部的“全部下载（大小）”一次下载全部工具，“更新已下载的工具（大小）”更新带“有更新”标签的工具；“已下载”筛选只显示已下载工具。下载过程中可查看进度、取消或重试；重试复用已下载内容。

下载某个工具时会同时安装它需要的运行时（Java 21、.NET 8），并把其他已下载工具更新到同一目录修订，只下载有变化的单元；未变化的工具不会重复下载。“移除”删除该工具以及不再被其他工具使用的运行时，无需联网。首次打开目录会检查工具更新，也可点击“检查工具更新”；检查失败不会删除已有工具。1.0.6 之前由离线安装程序放入应用目录的工具在原处更新，其他工具保存在 `%APPDATA%\RainyAgent\native-tools`；应用更新后仍保留，离线时可使用已下载工具。

点击顶部“CTF 工具”即可使用，不必先创建或选择聊天。默认“常用工具”目录共 38 项，按 Web 与接口、流量分析、逆向调试、取证与文件、隐写与图像、音频与信号、编码与数据分组显示；完整名单见[桌面功能说明](desktop.zh.md#ctf-工作台)。可按名称或用途搜索、按分组或“已下载”筛选、收藏工具及查看最近启动记录。收藏与最近记录按 Windows 用户保存，在聊天之间共享。Burp 收藏迁移到 Yakit，旧 Burp 最近记录移除，不会伪造 Yakit 启动记录。该目录只供人操作，不占用 Agent 的工具定义和提示词预算。

桌面工具打开独立窗口，命令行工具打开已配置依赖的 Windows 终端，离线网页打开隔离工具窗口。x64dbg 与 x32dbg 共用一个条目，并分别提供按钮；FFmpeg 同包包含 ffprobe 与 ffplay。程序按安装目录定位，Windows 工具留在 Windows 侧，Agent 项目终端使用当前执行目标。

Yakit 使用官方 1.4.8-0919 完整发行包，内置 `bins/yak.zip` 提供 Yak 1.4.8-beta19 的离线恢复来源，无需单独下载引擎。其数据目录遵循 Yakit 已配置的 `YAKIT_HOME`，Windows 默认值为 `Yakit.exe` 旁的 `yakit-projects`。原生界面的首次初始化仍须在 Yakit 内完成。工具包保留 AGPLv3 许可证及上游声明。ImHex 位于 Reverse 分类，可用于二进制编辑。

自有 IDA Pro 9.5 使用正式安装和自己的许可证。将其安装到本机工具根目录的 `tools/ida/`，通过 `%APPDATA%/RainyAgent/native-tools.local.json` 指定该绝对根目录及 ID 为 `ida` 的工具条目，入口为 `tools/ida/ida.exe`。应用优先使用这一本机条目，后续下载或更新公共工具时保留选择；安装路径、配置和许可证留在本机。配置字段与约束见[本机工具目录说明](desktop.md#ctf-workbench)。

下载的 IDA 附带独立的 Python 3.12.14、匹配的 SIP 绑定和 `imp` 兼容模块；启动时使用应用数据目录下的独立 `IDAUSR`。StegSolve、JADX 使用共享的 Java 21.0.12.1，dnSpy 6.6.0 附带 .NET Desktop 10.0.9。multimon-ng 默认打开帮助终端；分析 WAV 时，在音频所在目录依次运行 `sox.exe -R -t wav sample.wav -esigned-integer -b 16 -r 22050 -t raw sample.raw` 和 `multimon-ng.exe -a DTMF -t raw sample.raw`，文件名换成实际文件名。

“已发送某工具的启动请求”仅表示系统接受了请求；首次确认窗口、界面是否就绪及实际样例操作需分别验证。文件和依赖存在不等于通过功能验收：缺少文件时禁止启动，未经实际验收时显示“待验证”。版本按目录记录显示，具体版本依据与文件摘要保存在工具清单中。目录显示 38 项不能作为“38 项全部通过”的结论。

工具文件缺失的卡片提供“修复”按钮，只重新下载文件缺失或 SHA-256 不符的单元。无法联网时，退出 RainyAgent 并关闭所有工具及其命令行窗口，把全部 `rainy-unit-*.tar.gz` 归档放在匹配的安装 EXE 旁再运行安装程序，安装程序会安装其中每个单元。

选择“IceSky”标签可使用原有 22 项浏览器工具。首次选择时才加载一份 iframe，返回目录或收起工作台后保留该实例。选中的聊天拥有独立草稿；未选聊天或空白“新会话”页使用独立通用草稿。切换聊天、重新加载和退出时继续使用已有的保存流程。

IceSky 从安装包本地加载。工作台和 RainyAgent 的模型设置分别保存；需要 AI 改写或翻译时，在 IceSky 内单独配置接口。浏览器把该次请求与密钥交给所选本机 Host 限量转发，Host 不保存密钥或提示词。需要 Agent 协助处理时，将工作台结果复制并粘贴到会话中。

## 从源码构建

构建需要 Windows Node `^22.19 || >=24`、仓库锁定的 pnpm、Git，以及所选 WSL2 中的 Python 3.12+ 和 HTTPS 访问。环境介质的签名验证需要 GnuPG，可通过 `RAINY_GPG` 指定 `gpg.exe`。目标平台专用的 npm 包按锁文件的 SHA-512 校验，Node 发行包按官方 SHA-256 校验。

日常开发在仓库根目录运行 `pnpm run build`（只构建外壳与 Host 时用 `pnpm run build:host`），输出 `dist/main.cjs`、`dist/preload.cjs`、`dist/setup/`、`dist/host.js`、`dist/renderer/` 及 `resources/editor/`。`pnpm run dev` 构建后以 `tmp/dev-home` 为数据目录直接启动 Host 并打印工作台地址，`--no-build`、`--home <目录>`、`--port <端口>` 可调整，Ctrl+C 通过控制行停止 Host。`pnpm start` 从仓库根目录启动 Electron，Windows 原生目标需要先由 `scripts/stage-windows.mjs` 暂存 `runtime/windows-host`。`pnpm run test` 运行 Vitest，`pnpm run test:node` 运行 `tests/scripts` 中的脚本测试。

从仓库根目录运行：

```powershell
pnpm install --frozen-lockfile
node scripts/bootstrap-release-inputs.mjs --manifest toolpacks/build-inputs.v1.json
powershell.exe -NoProfile -ExecutionPolicy Bypass -File scripts/package.ps1 -Distribution Ubuntu -ReuseNativeToolsRelease release/offline-1.0.6 -ComponentSource release/offline-1.0.6/environment-components
```

[bootstrap-release-inputs.mjs](../scripts/bootstrap-release-inputs.mjs)按仓库中的 [1.0.6 输入清单](../toolpacks/build-inputs.v1.json)从固定版本的资源存档下载发行分片、重组并校验原始文件，恢复 Git 不保存的 IDE、Strata、工具、WSL 和运行环境输入；其中 `build-inputs/strata-runtime.tar.gz` 会展开到 `resources/strata-runtime`，供源码检出直接运行时使用，安装包不包含该目录。已有完整输入目录可通过 `--inputs-dir` 指定，仍需通过清单校验。打包入口 [package.ps1](../scripts/package.ps1)校验环境媒体和组件归档，运行 `build.ts --release`，由 `runtime-graph.mjs` 从 `node_modules/.pnpm` 计算 Host 外部依赖（node-pty、ripgrep、Pyright、TypeScript 语言服务、TypeScript、Prettier、tsx）的生产依赖闭包，分别生成 Windows 与 Linux Host，再由 `prepare-shell.mjs` 暂存 `build/shell` 并生成 NSIS 安装包。`-ReuseNativeToolsRelease` 复用刚恢复到 `release/offline-1.0.6` 的工具输入，`-ComponentSource` 指向其 `environment-components`。`-Distribution` 必须匹配准备好的 WSL 构建发行版名称。最终产物与干净 checkout 的实测情况由[验收记录](validation.md)记录，以上命令不代表已经完成该项验收。

Windows Host 构建先按官方 SHA-256 校验 Node ZIP，再使用 Windows 随附的 .NET ZIP 解压器展开；这一环节无需额外安装压缩工具。

生产构建使用独立 Ed25519 发行密钥。默认在忽略提交的 `build/release-signing-key.pem` 创建并复用本机构建密钥；`RAINY_RELEASE_SIGNING_KEY` 可指定已有私钥文件，路径缺失、格式不符或与暂存公钥不匹配时构建失败。公开资源只包含发行公钥，载体将该公钥嵌入并校验签名资源清单。该流程不读取客户授权数据库。

1.0.6 完整离线目录为 `release/offline-1.0.6/`，包含核心安装程序、原生工具文件、`environment/` WSL 介质、`environment-components/` 独立运行环境及递归 SHA-256 清单。WSL 镜像、科学计算大依赖、Strata 引擎、PHP 组件和 WSL 运行环境都位于核心 EXE 之外；安装包只附带固定这三个可选组件归档的签名资源 `optional-modules.json`。构建核对 IDE 固定来源、Linux APT 索引和组件逐文件摘要；Windows Host 使用实体依赖文件，Linux Host 使用包内相对链接。构建暂存位于 `runtime/` 与 Linux `/var/tmp`，最终容量和验收以本次产物报告为准。

## 已知限制和后续工作

当前发布目标是 Windows x64，可选择 Windows 原生或 WSL2。应用提供可下载的 Strata 引擎和离线运行环境，模型权重与兼容显卡驱动由使用者准备，启动模型需要明确操作。程序运行、断点调试、CPU/CUDA 组件导入与计算，以及各模型端点按具体产物分别验收。没有精确 tokenizer 的服务使用估计，其误差会影响压缩时机。

38 表示工具包目录的工具数量。命令行和离线网页功能检查按当前工具、版本与被测产物分别记录，旧版验收总数不沿用到新版。无头浏览器截图或 GIF 使用明确标注的桥接适配器，展示真实目录数据、搜索、分类和收藏。Windows 原生工具界面、首次启动提示、UAC 和干净机重启仍未验收。最终逐工具矩阵关联具体产物，不能由目录可用状态或进程创建成功推定。

源码保留上游模块以便追踪更新，生产启动组合和打包依赖另行裁剪。部分 DSH 内部库仍有传递依赖，但未装载的工具不会进入模型请求。安装包的 Windows Authenticode 签名状态和更新验收以本次发行记录为准。原始上游许可及组件归属见 [第三方说明](../THIRD_PARTY_NOTICES.md)。
