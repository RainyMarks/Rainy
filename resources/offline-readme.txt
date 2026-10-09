RainyAgent 1.0.10 Windows x64 完整离线发行包

安装与升级
1. 运行 RainyAgent-1.0.10-windows-x64-setup.exe 安装核心，内含 Windows Host。Strata 引擎、Windows PHP 和 WSL 运行环境不在安装包内，需要时从 GitHub 下载；模型权重另备。新安装默认使用 Windows 原生运行，无需先安装 WSL。
2. 完整离线交付请保留整个 offline-1.0.10 目录，包括 environment/、environment-components/、rainy-unit-*.tar.gz 工具归档、清单及 SHA256SUMS.txt。无需手动解压任何工具或运行环境归档。
3. rainy-unit-*.tar.gz 工具归档放在安装程序旁时，安装程序安装其中每个单元；不带归档的核心更新保留已有工具，之后可在“常用工具”中按工具下载。归档校验失败会停止工具安装。
4. 手动安装或修复工具前保存工作并关闭 RainyAgent、原生工具及其命令行窗口。安装程序检查占用，不自动结束这些进程。安装后直接使用，无需设备码或激活码。
5. 安装版启动时静默检查 GitHub 稳定版并后台下载核心更新；“帮助 → 检查更新”可查看状态或重试。下载完成后选择稍后或重启安装，只有确认、草稿保存与 Host 清理完成后才安装；普通退出不会安装。自动更新保留已有工具与环境，不重新下载工具或离线环境组件；更新后首次以 WSL 启动时会下载新版本的 WSL 运行环境。
6. 启动约 60 秒后，应用删除旧安装程序留在 resources 目录中的 strata-runtime、php、linux-runtime.tar.gz、native-tools-metadata.json、native-tools-download.json 和 native-tools-catalog.json。

可选组件
Strata 引擎、PHP 和 WSL 运行环境由安装包内签名的 optional-modules.json 固定，从 GitHub 发行分片下载到 %APPDATA%\RainyAgent\modules\<id>。每个分片和完整归档都校验 SHA-256，支持断点续传；取消后保留已下载的分片，重试时复用。
在“设置 → 运行环境 → 可选组件”下载或删除 Strata 引擎（约 560 MB）和 PHP 8.5。

Strata 本地模型
Strata 引擎包含 Strata 0.1.39、Python 3.12.14 和服务/准备/CUDA 运行依赖，下载后无需另装 Strata 或 Python。引擎缺失时，在“设置 → 模型 → Strata 本地模型”点击下载，或在“可选组件”下载。引擎面向 Windows x64、NVIDIA CUDA 13 和 580 以上驱动，内含 sm75/sm86/sm89/sm120 目标，不提供 AMD 或 Linux 推理引擎；显卡驱动由用户准备。
在“设置 → 模型 → Strata 本地模型”选择受支持的 Qwen3.8 Flash Next 主 GGUF（分片需完整并选择首片）及配套 MTP GGUF 或已准备目录。MTP 留空表示自动检测，API 服务仍需要它。发行包不含模型权重或派生 dense/expert 数据。
保存上下文、端口和必要的 KV/显存/RAM 设置，再点击启动。首次明确启动会在本机离线准备运行文件，可以取消，不会下载权重。就绪后连接并设为默认；当前 Host 核对实际模型与窗口，推理档位和输出上限仍在普通模型设置中控制。
设置私有保存在应用数据目录，模型留在用户选择的位置；应用只停止自己启动的进程。WSL NAT 无法连接 Windows loopback 时选择 Windows 执行环境，不会自动修改网络或回退云端。
Strata 运行时迁移、Python 导入与模拟健康/聊天接口已有检查，真实 GPU 推理与性能仍以本次验收报告为准。

项目与执行环境
使用“文件 → 打开文件夹”打开项目，首次启动不会自动创建桌面项目；“添加文件夹”挂载其他根目录。编辑器草稿、文件标签和布局按项目恢复，保存冲突先显示差异。
在“设置 → 运行环境”检测已有 Python、Conda、虚拟环境、Node、PHP 和 C/C++ 编译器，再选择项目解释器。检测不安装软件包，不修改已有环境或全局 PATH。可执行文件能启动和具体库能力分别显示。
Windows 原生使用 Windows 路径及 PowerShell；WSL 使用该发行版中的路径及 Bash。运行、调试、语言服务、终端和 Agent 命令共享所选项目环境。
运行中或排队中的 Agent 任务、程序、调试和终端会阻止目标切换；切换前保存草稿。已有 WSL 选择保留，旧聊天留在原 Host，项目公开身份和项目记忆跨目标保留。

导入离线运行组件
在运行环境页点击准备环境，选择“导入离线组件”，打开 environment-components/ 中与当前执行目标匹配的 JSON。导入完成后再次检测环境并选择解释器。
windows-basic / linux-basic：Python 3.12、pip、Node 24 和常用 Python 基础库；Windows 组件另含 PHP、Git 与 PowerShell。
windows-science-cpu / linux-science-cpu：独立 CPU 科学计算环境，包含 NumPy、SciPy、pandas、scikit-learn、图像处理、Jupyter 和常用深度学习库。
windows-science-cuda / linux-science-cuda：独立 CUDA 科学计算环境。PyTorch 2.11、torchvision 0.26 和 torchaudio 2.11 使用匹配版本；需要本机兼容的 NVIDIA 驱动，包内不安装显卡驱动。
windows-cpp：原生 Clang C/C++ 编译器、CMake、Ninja、clangd、clang-format 和 CodeLLDB 调试器。
每个组件安装在应用自有目录。归档、清单和逐文件摘要不匹配时停止导入，不替换已有选择。已有个人 Python 或 Conda 环境保持不变。

可选 WSL
需要 Linux 时，在运行环境页准备或选择 WSL。创建环境时选择 environment/ 文件夹，向导验证 WSL 安装介质及 Ubuntu 镜像；需要管理员确认或重启时按提示操作。应用不会自动重启 Windows。
安装或更新后首次以 WSL 启动时，应用联网下载本版本的 WSL 运行环境（约 340 MB），启动页显示进度；发行版中已解包本版本运行环境时不下载。下载失败时可选择重试、改用 Windows 原生运行（记录为执行目标，与在设置中切换相同）或退出。
WSL 的“准备 Ubuntu 开发工具”在明确安装后使用随包的 C/C++、GDB、CMake、Ninja、clangd 和 PHP 软件包，禁用软件下载。支持基线为 Ubuntu 26.04 amd64，其他发行版保留原有软件包并显示兼容性说明。

运行与调试
支持 Windows 原生和 WSL 的 Python、Node JavaScript、TypeScript、PHP 脚本及 C/C++ 单文件或 CMake 运行。Python、JavaScript/TypeScript 和 C/C++ 提供断点、单步、调用栈、变量和监视；PHP 仅提供运行。启动前保存修改，保存或构建失败时停止启动。
Windows 原生运行 PHP 前，在“可选组件”下载 PHP 8.5（含 json、openssl、mbstring、pdo_sqlite、curl、zip 扩展），或在运行配置中选择已安装的 php.exe；未下载时运行 PHP 文件会提示下载位置。已导入的环境组件或项目选定的 PHP 优先；WSL 使用发行版自身的 PHP。
可在 Strata 卡片启动本地模型，也可连接使用者配置的其他推理服务；安装科学计算库本身不会启动模型。目录及离线工具不要求模型，聊天和 IceSky 的 AI 功能使用所选服务，无自动云端回退。

常用工具与数据
原生工具目录共 38 项，按 Web 与接口、流量分析、逆向调试、取证与文件、隐写与图像、音频与信号、编码与数据分组，具体条目与版本见工具清单.json。目录数量、文件就绪和“已发送启动请求”不代表全部工具已完成功能验收。未验收入口仍显示“待验证”。
每个工具目录、共享运行时（Java 21、.NET 8）和目录文件各有一个 rainy-unit-*.tar.gz 归档；未变化的工具在新版本中沿用原归档，不会重复下载。“常用工具”中未下载的工具显示“下载 · 大小”，已下载工具可打开或移除；“全部下载”“更新已下载的工具”和“检查工具更新”位于顶部。移除会删除该工具及不再使用的运行时，无需联网。文件缺失的卡片提供“修复”，只重新下载缺失或摘要不符的单元。
1.0.10 之前由离线安装程序装入应用目录的工具在原处更新；其他情况下工具位于 %APPDATA%\RainyAgent\native-tools。工具根目录中，工具位于 tools/，共享运行时位于 runtime/windows/；个人设置、工作台草稿及项目数据分别保留。卸载应用保留工具、环境和用户数据。
安装会把被替换或移除的工具目录移入 .rainy-toolpack/backups/。安装完成后，与已保存清单一致的备份文件会被删除，只留下用户文件和有改动的文件；启动约 60 秒后，旧版本留下的备份也按同样规则清理。未完成的切换或回滚保留全部备份，不要手动清理恢复记录。

校验与验收范围
SHA256SUMS.txt 记录完整离线目录的交付文件摘要；核心同时校验发行组件目录，工具安装、可选组件下载和组件导入分别验证自身文件。
原生 Host、程序运行与断点调试、Windows/Linux CPU/CUDA 计算、组件导入及工具归档复用分别验收；旧版记录不代表本版本已完成验收。全新 Windows 的 UAC、重启续装及所有第三方工具原生界面仍须各自验收，详细范围以随交付验收报告为准。
Develop by NCUCyberBase

使用许可
允许个人和企业内部免费使用；销售、收费托管及商业再分发须另行取得书面许可。完整条款见应用内 LICENSE，第三方组件仍适用各自许可。
