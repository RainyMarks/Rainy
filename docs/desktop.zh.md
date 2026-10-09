# RainyAgent 桌面版

[English](desktop.md) | 中文

RainyAgent 使用 Windows x64 Electron 外壳；核心安装包包含 Windows Host。Strata 引擎、Windows PHP 和 WSL Linux 运行环境是可选组件，原生工具按工具单独下载。模型权重由用户自行提供。它保留上游 Agent 循环、会话持久化和 Web 聊天界面，默认组合四个工具：`read`、`write`、`edit`，以及所选平台的 Shell（`pwsh` 或 `bash`）。上游基线为 0.1.7-rc.2 版本。

[中文安装与配置指南](desktop-setup.zh-CN.md)负责产品流程、配置参考和源码构建说明。[验收记录](validation.md)区分模拟服务与真实 API 调用，并记录尚未完成的验收项。[第三方说明](../THIRD_PARTY_NOTICES.md)介绍上游和打包依赖。

## 离线安装与恢复

[1.0.10 发行包](https://github.com/RainyMarks/RainyAgent/releases/tag/v1.0.10)提供核心安装程序。[1.0.10 资源存档](https://github.com/RainyMarks/RainyAgent/releases/tag/v1.0.10-resources)存放 PHP 组件和 WSL 运行环境分片；[1.0.6 资源存档](https://github.com/RainyMarks/RainyAgent/releases/tag/v1.0.6-resources)存放原生工具归档。[1.0.0 资源存档](https://github.com/RainyMarks/RainyAgent/releases/tag/v1.0.0-resources)仍存放 Strata 归档、`environment/` WSL 安装介质和 `environment-components/` 运行环境归档。核心可直接在 Windows 原生启动，无需 WSL 或科学计算库。源码打包按[输入清单](../toolpacks/build-inputs.v1.json)把固定版本的输入恢复至 `release/offline-2.0.0`。原生工具为可选项：可在目录中按工具联网下载；离线安装时，把全部 `rainy-unit-*.tar.gz` 归档放在安装程序旁，安装程序会安装其中每个单元。不带这些归档时，安装程序保留现有工具。

工具安装窗口显示当前阶段、文件及阶段进度。点击取消后请求安全停止，并等待必要的回滚。使用同一组归档重新运行安装程序时，会重新校验归档并复用已校验的暂存文件。归档损坏或空间不足会在工具目录替换前停止安装。更新前请保存工作并关闭 RainyAgent、原生工具及其命令行窗口；安装程序报告占用，不自动结束这些进程。

第三方 Electron ASAR 归档按完整二进制文件安装、校验和备份。目录检查和离线 HTTP 响应通过未修改的磁盘文件系统读取这些文件。载体自身的打包页面继续使用正常的 ASAR 加载与完整性保护。[真实 Electron 测试](../tests/manual/toolpack-electron.mjs) 在私有临时目录检查安装、取消、重试、用户文件保留、目录检查及原始归档传输。

升级会在继续提供的工具目录保留已声明的设置和用户新增文件。每个已安装版本保存文件清单，用于区分程序文件和用户文件；新版不再包含的旧程序文件会被移除。安装会把被替换或移除的工具目录移入 `.rainy-toolpack/backups/<transactionId>/`。安装提交后，路径、大小和 SHA-256 与已保存版本清单一致的备份文件会被删除，只留下用户文件和有改动的文件供手动恢复。启动约 60 秒后，应用对旧版本留下的备份执行同样的清理；未完成的切换或回滚会保留全部备份。旧清单之外的目录保持不动。用户路径与新版冲突或旧清单缺失时，升级会在替换前停止并指出相关路径。恢复记录、版本文件清单和剩余备份保留在 `.rainy-toolpack` 中。卸载应用会保留 `tools/`、`runtime/`、恢复记录及用户数据。

新安装默认使用 Windows，已有保存的 WSL 选择继续保留。运行环境设置列出已注册 WSL2 发行版，也可使用外置离线介质准备应用自有的 Ubuntu 26.04.1 环境。安装 WSL 系统组件必须明确操作，并按 Windows 要求请求管理员确认及重启。已有发行版及数据保留。原发行版丢失时打开恢复入口，不静默切换目标。干净 Windows 上的管理员确认和重启恢复仍是独立验收项。

## 可选组件

核心安装包不含 Strata 引擎、Windows PHP 和 WSL Linux 运行环境归档（`linux-runtime.tar.gz`）。签名的载体资源 `optional-modules.json` 固定每个组件的归档，应用在需要时从 GitHub 发行分片下载到 `%APPDATA%\RainyAgent\modules\<id>`。每个分片和组装后的归档都校验 SHA-256，中断的下载通过 HTTP Range 续传，取消后保留已下载的分片供下次使用。

“设置 → 运行环境 → 可选组件”可下载和删除 Strata 引擎（约 560 MB）及 PHP。Strata 引擎就是已作为 1.0.0 构建输入 `build-inputs/strata-runtime.tar.gz` 发布的归档；[Strata 本地推理](#strata-local-inference)说明其内容和用法。PHP 用于 Windows 原生目标；“代码工作区”一节说明项目如何选择 PHP。

WSL 运行环境（约 340 MB）与每个 RainyAgent 版本对应，发布在 1.0.10 资源存档中。安装或更新后首次以 WSL 启动时自动下载，启动页显示进度；发行版中已解包本版本运行环境时不下载。下载失败时，对话框提供“重试”“改用 Windows 原生运行”和“退出”。改用 Windows 原生运行会把 Windows 记录为执行目标，与在设置中切换相同。以 WSL 启动约一分钟后，已安装的应用会删除旧版本解压在 `~/.rainy-agent/runtime` 中的运行环境，保留本版本以及仍有进程在使用的运行环境。

启动约 60 秒后，安装版会删除旧安装程序留在 `resources` 目录中的资源：`strata-runtime`、`php`、`linux-runtime.tar.gz`、`native-tools-metadata.json`、`native-tools-download.json` 和 `native-tools-catalog.json`。

## 应用更新

安装版在启动时检查 GitHub `RainyMarks/RainyAgent` 的稳定发行版，并在有新版本时后台下载核心更新。“帮助 → 检查更新”提供手动状态查看与重试。准备完成后可选择“稍后”或“重启安装”；只有确认、草稿保存和 Host 清理完成后才启动安装。保存失败会保留应用窗口。普通退出不会安装已下载的更新，更新器不会选择预发布版或降级已安装版本。

自动更新替换核心应用并保留已安装的工具与环境，不会重新下载原生工具、WSL 介质或 CPU/CUDA 环境组件；更新后首次以 WSL 启动时会下载该版本的 WSL 运行环境。CPU/CUDA 环境组件仍从匹配的离线发行文件导入。[验收记录](validation.md)按发布产物记录已经完成的更新检查。

## 运行时约定

Host（[架构说明](architecture.md)）不包含官方账号、遥测、办公运行库、浏览器与电脑操作、定时任务、多 Agent 工具和插件市场。一个选定的 Windows 或 WSL Host 负责命令、终端、模型连接及会话；Windows 外壳负责窗口、原生工具窗口及共享项目目录。生命周期控制使用标准输入输出，应用数据使用认证后的 HTTP/WebSocket。切换执行目标前保存草稿，并拒绝运行中或排队中的 Agent 工作、程序、调试及终端；修改目标选择前冻结新执行。

WSL 启动在 Host 报告就绪后，还会等待 Windows 侧 loopback TCP 可达。连接拒绝会在原有 90 秒启动期限内重试，其他连接错误会使启动失败。该检查不发送 HTTP 请求或认证 token。取消、超时及 Host 退出都会停止探测，transport 关闭会等待所拥有的 socket 和子进程退出。Windows 原生启动不执行跨系统探测。

窗口立即显示启动页并报告每个准备步骤。窗口按上次关闭时保存的大小、位置和最大化状态重新打开；首次启动或保存位置所在的显示器已断开时，默认大小在主显示器居中，主显示器较小时最大化。之前启动已记录的 WSL 目标会跳过环境检查窗口：启动只用一次 WSL 调用映射载体路径并解包 Host，该目标不可用时才打开检查。每次启动都会把各阶段耗时追加到 `host.log`。

原生窗口控制与单行顶部工具栏共用区域。顶部空白处支持拖动和双击最大化，编辑和缩放快捷键仍可使用。统一设置分别提供模型、上下文、项目记忆、Skills、MCP 和运行环境。运行环境页可选择执行目标、检查已有解释器、导入发行包认可的离线组件及下载可选组件。

新工作区显示左侧文件树和主编辑区，需要时通过顶栏按钮展开 AI 对话和底部面板；“视图 → 专注编辑”收起两者。已有工作区保留保存的面板开关与宽度。文件操作只有一行，编辑器操作与已开标签共用一行，CTF 工具按需打开为可关闭的标签。空间不足时文件栏可覆盖编辑器显示。文件标签、对话及保留的 CTF 工作台分别保存状态。主页和“关于”署名为 `Develop by NCUCyberBase`。推理运行时英文显示“Thinking...”，中文显示“思考中”；计时控件使用对应的本地化文字。

## 代码工作区

项目从用户明确打开的文件夹开始，新安装不自动创建桌面项目。“添加文件夹”挂载其他目录，每个根目录拥有稳定标识，跨根文件操作按该标识限定范围。Windows 项目使用原生路径；WSL 项目使用在所选发行版中映射并规范化的路径。已有工作区保留身份、编辑恢复和原执行目标中的聊天。切换项目时把公开项目身份绑定至新目标，恢复目录挂载，并在 Host 就绪前选中该工作区；旧聊天及编辑恢复仍留在原 Host。

CodeMirror 6 编辑器提供多文件标签、按文件类型加载的语法高亮、查找替换、转到行、撤销、显式保存和内联差异视图。文件操作保留 UTF-8 BOM 和换行方式。二进制、非 UTF-8 及超限文档只读，默认可编辑上限为 5 MiB。超限 UTF-8 文件最多预览 64 KiB，不截断字符；二进制以十六进制最多预览 4 KiB。快速打开搜索工作区路径，限制结果数量，并提示截断。更改查询、切换项目或重新打开窗口会使旧结果失效；搜索等待或失败时不能打开旧结果。

语言服务接收 Host 文件 URI。语言服务返回的 URI 写法不同时（例如编码后的盘符冒号或 Windows 上不同的大小写），编辑器仍将其对应到同一文件，包括 Windows 盘符根和 UNC 共享。

恢复存储按工作区保留未保存缓冲区、已开标签及面板尺寸。外部修改会刷新未编辑的缓冲区；已编辑的缓冲区保留文本并提供比较。版本冲突会阻止保存，直到用户处理已显示的差异。运行与调试先保存当前工作区的全部修改，任一保存失败就停止启动。

持续的语言服务分析尚未保存的文档：Python 使用 Pyright，JavaScript/TypeScript 使用 TypeScript Language Server，C/C++ 使用 clangd。补全、悬停提示、签名帮助、诊断、转到定义（F12）、查找引用（Shift+F12）和重命名（F2）都来自这些语言服务；重命名也会修改未打开的文件，这些文件随后以未保存标签页显示。格式化使用随包 Ruff、Prettier 或已准备的 clang-format。Python 解释器选择与 CMake 编译数据库也用于配置编辑器语言服务。选中代码通过普通用户消息发送，路径、范围和文本一起进入会话记录。

运行按钮按当前文件后缀对应的语言运行该文件。旁边的菜单可为该文件改选其他语言（工作区会记住），或固定运行另一个入口程序的已保存配置；保存当前文件的配置不会将其固定。运行配置按工作区保留入口、参数、解释器或编译器、工作目录及 CMake 选项。Python 支持文件、模块和已有虚拟环境；JavaScript、TypeScript 使用 Node；PHP 支持直接运行脚本；C/C++ 支持单文件及 CMake 构建，失败时停止启动。debugpy、js-debug、Windows CodeLLDB 与 Linux GDB DAP 调试由工作区启动的程序；界面提供断点、单步、调用栈、变量、监视和停止。不提供外部进程附加或 PHP 调试。

运行环境检测检查项目虚拟环境、Conda 注册记录、常见 Python 目录、PATH 和已导入组件，不向这些已有环境安装软件包。所选解释器通过每个进程的环境配置项目工具、终端、运行及语言服务。可执行文件就绪和库能力分别显示：能运行 Python 不代表具备 PyTorch 或 CUDA。另一执行平台的解释器会被拒绝。“设置 → 运行环境 → 可选组件”为 Windows 原生目标下载 PHP 8.5 及 json、openssl、mbstring、pdo_sqlite、curl、zip 扩展。下载前，在 Windows 原生目标运行 PHP 文件会提示下载位置；也可在运行配置中选择已安装的 `php.exe`。已安装的环境组件或项目选定的 PHP 优先。WSL 目标使用发行版自身的 PHP。

离线组件提供隔离的 Python 3.12、Node 24、Windows PHP、原生 C/C++ 工具，以及独立 CPU 或 CUDA 科学计算环境。科学组件包含 NumPy、SciPy、pandas、scikit-learn、图像处理、Jupyter 和常用深度学习库，并配套 PyTorch 2.11、torchvision 0.26 与 torchaudio 2.11。导入先核对发行目录、归档及逐文件清单，再选择按摘要存放的代际。已有用户环境和全局 PATH 保持不变。CUDA 需要兼容的 NVIDIA 驱动。Ubuntu 26.04 amd64 开发工具向导使用经过校验的 C/C++、GDB、CMake、clangd 和 PHP 软件包，安装时禁用下载。

<a id="ctf-workbench"></a>
桌面应用允许已认证 Host 同源的会话和工具页面复制文本。历史对话中的工具调用按工具类型显示为终端、读取、差异或通用卡片。

## CTF 工作台

应用用户数据目录中的私有 `native-tools.local.json` 可使用现有本机安装覆盖工具目录。其版本 1 对象包含绝对路径 `root` 和版本 1 的 `catalog`，工具条目字段与内置目录一致，入口使用 `tools/<id>/` 下的安装相对路径。相同 ID 替换下载条目，其他 ID 扩展目录。本机安装在工具更新后仍保持选中，不显示发布者功能验收标记。IDA Pro 应指向正式安装的程序，并单独激活自己的许可证；安装程序和许可证均不属于公共工具下载渠道。

每个工具目录、每个共享运行时（Java 21、.NET 8）和目录文件都是一个安装单元，各有独立归档 `rainy-unit-<20 位十六进制>.tar.gz`。归档名由该单元的文件清单得出，因此未变化的工具在各发行版间沿用同一归档，不会再次下载。1.0.6 版共有 41 个归档，合计约 2.1 GB；最大的 IDA 约 383 MB，7-Zip 等小工具约 1 MB。1.0.6 之前由离线安装程序放入应用目录的工具在原处更新；其他情况下工具位于 `%APPDATA%\RainyAgent\native-tools`。

工具通道使用签名格式 2：`toolpacks/native-tools-channel.v2.signed.json`（签名域 `RainyAgent/tool-channel/v2`）及下载来源 `toolpacks/native-tools-source.v2.json`。RainyAgent 2.x 从 `main` 分支获取该文件的新修订。`apps/rainy-desktop/toolpacks/` 保留已安装 1.x 客户端读取的位置：供 1.0.6 及以后版本使用的 `native-tools-channel.v2.signed.json` 逐字节副本，以及供 1.0.5 及更早版本使用的版本 1 `native-tools-channel.signed.json`。`build-tool-channel.mjs` 与 `sign-tool-channel.mjs` 的输出为 `toolpacks/native-tools-channel.v2.signed.json` 时会同时重写该副本。安装程序以 `resources/native-tools-channel.signed.json` 附带通道；应用采用随包通道与缓存通道中修订号较高者，并忽略发布者的旧修订。应用拒绝无效签名和不匹配的目录，也不会从通道接受新的信任公钥。发布者先打包已暂存的工具，再以更大的 `--revision` 构建并签名通道：

```sh
node scripts/package-native-tools.mjs --stage <stage> --output <dir> [--previous <earlier metadata.json>]
node scripts/build-tool-channel.mjs --metadata <dir>/native-tools-metadata.json --archives <dir> --catalog <stage>/tools/manifest.json --version <x.y.z> --revision <n> --pieces <pieces dir> --source-output toolpacks/native-tools-source.v2.json --output toolpacks/native-tools-channel.v2.signed.json [--previous-source <earlier source>] [--key <publisher key>]
```

将输出列出的分片上传到 `v<version>-resources` Release，并提交两个 toolpacks 文件及 1.x 副本。上一版来源清单中已有的归档保留原发布位置。签名身份须匹配 `resources/native-tools-public-keys.json`；重新构建应用只需保留这些公钥，无需持有工具签名私钥。

“常用工具”顶部汇总“已下载 N / M 款工具”。未下载工具的卡片以“下载 · 大小”代替“打开”；已下载工具显示“打开”和“移除”，移除需再次点击确认。“有更新”标签标出过期工具。顶部按钮“全部下载（大小）”和“更新已下载的工具（大小）”位于“检查工具更新”旁。安装前逐个校验传输分片和归档的 SHA-256；下载支持取消、断点续传和重试。下载某个工具时会同时安装它需要的运行时，并把其他已下载工具更新到同一目录修订，只下载有变化的单元。“移除”删除该工具以及不再被剩余工具使用的运行时，无需联网。已安装工具可离线使用，重启和应用更新后仍保留。首次访问目录会检查签名工具通道，“检查工具更新”可再次检查。

未选择会话时也能打开 CTF 入口。默认“常用工具”页按任务分组列出工具，可搜索名称和用途、按分组或“已下载”筛选，并显示收藏、最近启动请求、版本及可用状态。收藏和最近记录属于 Windows 用户，在会话之间共享。已保存的 Burp 收藏迁移到 Yakit；旧 Burp 最近启动记录会移除，不会据此记录一次 Yakit 启动。工具包目录包含以下 38 项；后续目录新增的工具按其签名分类显示在“Web 与接口”“逆向调试”或“其他”下：

| 分组 | 条目 |
|---|---|
| Web 与接口 · 3 | Yakit、Bruno、curl |
| 流量分析 · 2 | Wireshark、pcapfix |
| 逆向调试 · 7 | IDA、x64dbg/x32dbg、Detect It Easy、ImHex、JADX、dnSpy、PyInstaller Extractor |
| 取证与文件 · 7 | Binwalk、ExifTool、7-Zip、GNU strings、qpdf、WinMerge、ripgrep |
| 隐写与图像 · 8 | StegSolve、ImageLSBViewer、PNGcheck、TweakPNG、ImageMagick、GIMP、Tesseract OCR、QRazyBox |
| 音频与信号 · 5 | Audacity、Sonic Visualiser、SoX、multimon-ng、FFmpeg |
| 编码与数据 · 6 | CyberChef、Qalculate!、jq、yq、SQLite、DB Browser for SQLite |

x64dbg 与 x32dbg 共用一个条目，分别提供启动按钮。FFmpeg 包含 ffprobe 和 ffplay。桌面程序以普通窗口独立打开，命令行工具使用准备好环境的 Windows 终端，离线网页使用不带原生启动桥接的隔离窗口。成功提示为“已发送 {name} 的启动请求”，仅确认系统已接受请求。文件可用、界面就绪和功能验收属于不同观察。`status: ready` 表示所需文件及依赖存在；独立的 `verified` 标志对应匹配目录的已记录验收。未经验证的工具保留“待验证”标记，缺少依赖时禁止启动。目录不增加 Agent 工具或提示词。

Yakit 1.4.8-0919 的 `bins/yak.zip` 包含官方 Yak 引擎 1.4.8-beta19，供其内置引擎恢复机制使用，无需另行下载该引擎。Yakit 遵循已配置的 `YAKIT_HOME`；Windows 默认目录为 `Yakit.exe` 旁的 `yakit-projects`。工具包保留 Yakit 的 [AGPLv3 许可证](https://github.com/yaklang/yakit/blob/v1.4.8-0919/LICENSE.md)及上游声明。ImHex 继续位于 Reverse 分类，用于二进制编辑。

IDA 附带独立的 Python 3.12.14、匹配的 SIP 绑定和 `imp` 兼容模块。RainyAgent 为它使用应用数据目录下独立的 `IDAUSR`，并清除继承的 Python、Qt 环境设置。StegSolve 和 JADX 使用共享的 Java 21.0.12.1；dnSpy 6.6.0 附带 .NET Desktop 10.0.9。原生工具启动时只搜索包内程序和 Windows 系统工具，不依赖第三方系统 PATH。正常的操作系统 DLL 和设备驱动仍由 Windows 提供。

Windows 版 multimon-ng 读取 16 位单声道 raw 音频；同目录附带 SoX，用于转换其他格式。在含有 `sample.wav` 的目录中运行 `sox.exe -R -t wav sample.wav -esigned-integer -b 16 -r 22050 -t raw sample.raw`，再运行 `multimon-ng.exe -a DTMF -t raw sample.raw`。工具卡片会打开显示帮助内容的终端，不会自动采集麦克风声音。

工具文件缺失的卡片提供“修复”，只重新下载文件缺失或 SHA-256 不符的单元。

IceSky 标签打开固定版本的浏览器工作台。一个保留的 iframe 提供其原有 22 个手动工具，各工具的模板与组件在首次选择时加载。切回目录会保留该 iframe。每个会话拥有独立草稿；未选中会话或处于空白“新会话”页面时打开 IceSky，会使用单独的通用草稿。工作台跟随应用的主题和字号。导航与状态控件跟随应用语言，捆绑工具正文保留原有语言。

所选 Host 的持久化存储保留可编辑文本、选项、文本结果和文件元信息。草稿不恢复文件内容或生成的二进制产物；重启后，恢复的草稿会在需要时提示重新选择本地文件。草稿不包含 API 凭据。AI 请求经认证的限量转发连接 IceSky 内配置的接口，包括所选执行目标的本地端点；转发层不留存请求密钥或正文。

文本变换、解码和分词在可取消的 worker 中执行。输入变化后，自动计算等待 160 ms，最多接受 64 KiB 的 UTF-8 输入；更大的输入需点击“计算”。Token 结果每页呈现 100 项。工具加载、计算和草稿保存失败时，当前草稿仍可重试或导出。切换会话、刷新工作台、通过原生菜单或快捷键重新加载窗口，以及退出应用，都会等待待保存的草稿完成写入；刷新或退出保存失败时保持窗口打开。

<a id="strata-local-inference"></a>
## Strata 本地推理

Strata 引擎是约 560 MB 的可选组件，包含 Niko1221/Strata 0.1.39、Python 3.12.14，以及所需服务、准备脚本和 CUDA 运行依赖。引擎缺失时，“模型 → Strata 本地模型”显示“下载”按钮；“运行环境 → 可选组件”也列出该引擎。用户无需另装 Strata 或 Python。推理引擎面向 Windows x64、NVIDIA CUDA 13 和 580 或更新驱动，内含 `sm75`、`sm86`、`sm89`、`sm120` 目标。本包不提供 AMD 或 Linux 推理引擎。[运行时来源清单](../toolpacks/strata-runtime.sources.json)固定输入与许可。

用户提供受支持的 Qwen3.8 Flash Next 主模型 GGUF 及全部分片，以及配套 MTP GGUF 或已准备 MTP 目录。运行时不分发主模型/MTP 权重或派生的 dense/expert 文件。在“模型 → Strata 本地模型”中选择主模型、MTP 来源或兼容 profile，然后保存。MTP 路径留空时尝试从主模型附近检测匹配文件；API 服务仍然需要 MTP。只有明确点击启动后，才在本机准备所需模型文件并加载服务，不下载权重。准备和启动均可取消。

卡片提供引擎上下文长度、loopback 端口、KV 缓存、保留显存和常驻 RAM 预算。设置私有保存在载体应用数据目录中的 `strata/settings.json`，选中的模型文件留在安装目录之外。只能停止该管理器启动的进程；已有外部服务单独标识，不会被卡片结束。每次请求的推理和输出上限继续由普通模型配置管理。

“连接并设为默认”先让当前 Host 验证实际已加载模型与窗口，再保存本地端点。即使项目 Host 使用 WSL，Strata 仍在 Windows 运行。若 WSL NAT 无法访问 Windows loopback，请为 Strata 选择 Windows 执行环境；应用不修改网络设置，也不替换成云端模型。

当前 Strata 验证覆盖引擎运行时迁移、Python 导入以及模拟服务的健康检查与聊天衔接，尚不能说明真实 GPU 推理、吞吐量或跨硬件兼容性。[验收记录](validation.md)负责具体被测产物和剩余项目。

## 模型体验

每次请求发送前都会估算大小：系统提示、工具定义和消息。输入框旁的上下文占用显示该估算，或上一请求由服务商返回的实际用量。估算达到压缩阈值时，较早的一段对话会被摘要替换；这段范围不包含最新的用户请求，也不会拆开工具调用和它的结果。摘要由当前对话的模型撰写。上一请求的提示缓存仍有效时（该请求在 54 分钟内发出，且模型和推理档位相同），摘要请求原样重复该请求，只在末尾加一条指令，因此大部分输入按缓存读取计价；缓存已过期，或模型调用了工具而没有直接回答时，改为单独请求只摘要那一段。展开对话里的压缩记录可以看到用了哪种请求，以及输入、缓存和输出 token 数。摘要会逐字保留仍然有效的用户指令和约束，并记录已验证的进展、准确路径、错误、未完成的工作和下一步；文件、工具或网页中出现的指令只作为数据记录，不会变成任务。模型看到的摘要是助手写的检查点，而不是用户的新请求。上下文溢出错误会先压缩再重试一次；`/compact` 按需压缩。摘要失败时历史保持不变。明确设置的输出上限（包括 DeepSeek 预设的最大值）优先于默认值。

发往同一端点来源的请求按到达顺序排队。本机回环服务（Base URL 主机为 localhost、`*.localhost`、127.0.0.1 或 [::1]）一次只运行一个请求，其他端点最多并行四个，因此并行的对话和后台请求不必等待彼此的完整输出流。五分钟没有任何输出的流会被中止，单个请求最长运行 30 分钟。

“设置 → 模型 → 全局提示词”保存的指令（最多 4,000 个字符）会进入所有对话的系统提示，包括已打开的对话。压缩摘要、项目记忆和连接检查使用各自的提示词；留空则不添加内容。

模型设置保存在所选 Host 的 `settings.json`；API 密钥保存在 `.credentials.json`（权限 0600），同名环境变量优先。新的 API 模型默认使用 OpenAI Responses，本地模型默认使用 Chat Completions，均可按服务修改。DeepSeek 预设使用 1,000,000 上下文 tokens。两个 Claude 预设共用 Anthropic 密钥，只提供模型支持的推理档位，并将提示词缓存保留一小时。发现模型只需要供应商、Base URL、协议和可选密钥，不保存任何内容；保存和单独的流式/工具调用检查需要完整配置。RainyAgent 不会静默替换为其他模型。首次启动时会一次性导入 1.x profile 中的模型、密钥和全局提示词；1.x 的对话不导入。

Skills 是 `<项目>/.rainy/skills` 和 `~/.rainy-agent/skills` 下包含 `SKILL.md` 的文件夹。系统提示中只放它们的名称和描述（合计最多 4,096 tokens），模型需要时再读取对应文件。MCP 服务在“设置 → Skills 与 MCP”中统一配置，通过 stdio（命令、参数、环境变量）或 streamable HTTP（URL 和 `Authorization` 等请求头）服务所有对话。环境变量和请求头保存在 `settings.json` 中，不在保存模型密钥的凭据存储中。其工具名为 `mcp__<服务>__<工具>`，调用 60 秒超时，服务说明会加入系统提示。IDA 预设通过 `uvx` 启动 IDA MCP 服务。

工具执行无需审批。读取工具按行分页，默认 2,000 行、每行 2,000 字符、50 KiB；长行截断标记清晰可见，原文件不受影响。写入和编辑要求先读取已存在的文件，并保留其 BOM 与主要换行符。Shell 命令（WSL 中为 `bash`，Windows 中为 `pwsh`）默认 60 秒超时，最长 10 分钟；较长输出在回复中保留开头和结尾，完整文本保存在 Host 临时目录的文件中。工作区指令（`AGENTS.md`、`CLAUDE.md` 及其 `.local` 版本，最多 64 KiB）在对话首次请求前加入一次，智能体进入子目录工作时再加入该目录的指令。对话标题取自第一条用户消息。

## 项目记忆

项目记忆使用跨执行端共享的载体项目身份。Windows 与 WSL 共用项目记录，环境事实保留对应执行端范围。使用与自动生成分别开关。人工编辑必须提供当前显示的修订号，自动更新不能覆盖或移除人工修正的条目。删除保留来源水位与排除摘要，防止旧证据重新生成已删除内容。

完成一轮后，后台等待前台空闲 60 秒，每项目最多每 10 分钟处理一次。它使用所选模型、不带工具，输入最多 4,096 tokens、输出最多 512 tokens，30 秒超时，不自动回退云端。新的前台输入会取消辅助请求。有效条目最多保存 1,024 估算 tokens；新聊天载入最多 512 tokens，且不超过输入预算的 5%，包含召回说明。失败、取消或修订冲突会保留旧记录。召回作为标明为历史资料的上下文条目进入对话，后续请求从对话文件中重放。来源文件变化会使关联的自动事实失效；原始会话日志仍然可用。

## 使用许可与发行完整性

[RainyAgent 源码可用许可](../LICENSE)允许个人和企业内部免费使用、修改及非商业再分发；销售、收费托管和商业再分发需要另行书面许可。应用启动和执行不读取设备码、激活码或有效期，设置中没有激活入口。既有授权文件保留在原处，不参与启动或执行判断。项目、模型配置和聊天仍使用原有存储位置。

生产载体使用构建时嵌入的发行公钥验证资源清单，并保留 Electron ASAR 完整性保护。构建密钥独立于用户数据：默认保存在被忽略的 build 目录，也可通过 RAINY_RELEASE_SIGNING_KEY 指定已有 Ed25519 私钥文件；私钥不进入发行包。Windows Authenticode 签名使用另行配置的证书。

已安装发行版首次启动时对完整签名清单计算哈希，然后记录包含清单及文件大小、修改时间和文件 ID 的验证戳。之后启动若验证戳匹配则无需计算哈希。工作台加载约一分钟后，后台检查每天至多比较一次这些文件元数据，仅在其变化时重新计算清单哈希。检查失败会删除验证戳并报告受损资源。

## 已知限制与后续工作

发行目标为 Windows x64；签名状态及平台、模型验收以[验收记录](validation.md)中的具体产物为准。其他 WSL2 发行版与模型端点需分别验收。MCP 服务不支持 OAuth 登录，也没有扩展市场。请求前的 token 数是估算值，每次请求后由服务商返回的实际用量替换。

[发行验收记录](validation.md)负责浏览器性能测量与验收范围。本地 OCR、PDF 渲染和 BPE 在浏览器中运行；单项验收不代表所有输入文档的保真度或长时间 OCR 稳定性。

38 表示工具包内容数量，不代表通过功能验收的工具数量。命令行与离线网页检查按工具和产物分别记录，旧发行版的验收总数不适用于扩充后的目录。原生第三方 GUI 行为、首次启动提示和实际 UAC 同意交互仍需独立验收。干净 Windows 的安装与重启结果由最终验收记录按被测产物分别记录。无头浏览器的目录证据使用明确的桥接适配器，覆盖目录界面。逐工具验收与目录、安装器和环境测试分别记录；最终验收记录需关联被测产物和剩余项目。

<details>
<summary>IceSky 构建与定向检查</summary>

桌面构建在打包前校验固定版本的离线依赖校验和，并重新生成资源清单。浏览器资源包含本地处理所需的文档库、PDF worker、OCR 核心和语言数据。[IceSky 集成补丁](../resources/icesky/RAINY_PATCH.md)负责记录源码补丁与依赖。

在仓库根目录运行以下定向检查：

```sh
node --test tests/icesky/*.test.mjs
pnpm exec vitest run tests/main/saved-reload.test.ts tests/host/icesky-host.test.ts
```

这些检查不调用模型接口。

</details>
