Rainy 3.0.0 Windows x64 完整离线发行包

安装与升级
1. 运行 Rainy-3.0.0-windows-x64-setup.exe 安装核心，内含 Windows Host。Strata 引擎、Windows PHP 和 Linux 运行环境不在安装包内，需要时从 GitHub 下载；模型权重另备。新安装默认使用 Windows 原生运行，无需先安装 WSL。
2. 完整离线交付请保留整个 offline-3.0.0 目录，包括 environment/、environment-components/、清单及 SHA256SUMS.txt。无需手动解压任何运行环境归档。安装后直接使用，无需设备码或激活码。
3. Rainy 3.0 不再包含 CTF 工具目录。旧版本下载的工具留在原安装位置（安装目录的 tools 文件夹或 %APPDATA%\RainyAgent\native-tools），应用不再更新或启动它们；不再需要时可自行删除。
4. 安装版启动时静默检查 GitHub 稳定版并后台下载核心更新；“帮助 → 检查更新”可查看状态或重试。下载完成后选择稍后或重启安装，只有确认、草稿保存与 Host 清理完成后才安装；普通退出不会安装。自动更新保留已有环境，不重新下载离线环境组件；更新后首次以 WSL 或 SSH 启动时会安装新版本的 Linux 运行环境。
5. 启动约 60 秒后，应用删除旧安装程序留在 resources 目录中的 strata-runtime、php、linux-runtime.tar.gz 及旧版工具目录文件（native-tools-*.json）。

可选组件
Strata 引擎、PHP 和 Linux 运行环境由安装包内签名的 optional-modules.json 固定，从 GitHub 发行分片下载到 %APPDATA%\RainyAgent\modules\<id>。每个分片和完整归档都校验 SHA-256，支持断点续传；取消后保留已下载的分片，重试时复用。
在“设置 → 运行环境 → 可选组件”下载或删除 Strata 引擎（约 560 MB）和 PHP 8.5。

Strata 本地模型
Strata 引擎包含 Strata 0.1.39、Python 3.12.14 和服务/准备/CUDA 运行依赖，下载后无需另装 Strata 或 Python。引擎缺失时，在“设置 → 模型 → Strata 本地模型”点击下载，或在“可选组件”下载。引擎面向 Windows x64、NVIDIA CUDA 13 和 580 以上驱动，内含 sm75/sm86/sm89/sm120 目标，不提供 AMD 或 Linux 推理引擎；显卡驱动由用户准备。
在“设置 → 模型 → Strata 本地模型”选择受支持的 Qwen3.8 Flash Next 主 GGUF（分片需完整并选择首片）及配套 MTP GGUF 或已准备目录。MTP 留空表示自动检测，API 服务仍需要它。发行包不含模型权重或派生 dense/expert 数据。
保存上下文、端口和必要的 KV/显存/RAM 设置，再点击启动。首次明确启动会在本机离线准备运行文件，可以取消，不会下载权重。就绪后连接并设为默认；当前 Host 核对实际模型与窗口，推理档位和输出上限仍在普通模型设置中控制。
设置私有保存在应用数据目录，模型留在用户选择的位置；应用只停止自己启动的进程。WSL NAT 无法连接 Windows loopback 时选择 Windows 执行环境，不会自动修改网络或回退云端。SSH 服务器上的 Host 无法使用本机的 Strata。
Strata 运行时迁移、Python 导入与模拟健康/聊天接口已有检查，真实 GPU 推理与性能仍以本次验收报告为准。

项目与执行环境
使用“文件 → 打开文件夹”打开项目，首次启动不会自动创建桌面项目；“添加文件夹”挂载其他根目录。编辑器草稿、文件标签和布局按项目恢复，保存冲突先显示差异。
在“设置 → 运行环境”检测已有 Python、Conda、虚拟环境、Node、PHP 和 C/C++ 编译器，再选择项目解释器。检测不安装软件包，不修改已有环境或全局 PATH。可执行文件能启动和具体库能力分别显示。
Windows 原生使用 Windows 路径及 PowerShell；WSL 使用该发行版中的路径及 Bash；SSH 使用服务器上的路径及 Bash。运行、调试、语言服务、终端和 Agent 命令共享所选项目环境。
运行中或排队中的 Agent 任务、程序、调试和终端会阻止目标切换；切换前保存草稿。已有 WSL 选择保留，旧聊天留在原 Host，项目公开身份和项目记忆在 Windows 与 WSL 之间保留。

导入离线运行组件
在运行环境页点击准备环境，选择“导入离线组件”，打开 environment-components/ 中与当前执行目标匹配的 JSON。导入完成后再次检测环境并选择解释器。
windows-basic / linux-basic：Python 3.12、pip、Node 24 和常用 Python 基础库；Windows 组件另含 PHP、Git 与 PowerShell。
windows-science-cpu / linux-science-cpu：独立 CPU 科学计算环境，包含 NumPy、SciPy、pandas、scikit-learn、图像处理、Jupyter 和常用深度学习库。
windows-science-cuda / linux-science-cuda：独立 CUDA 科学计算环境。PyTorch 2.11、torchvision 0.26 和 torchaudio 2.11 使用匹配版本；需要本机兼容的 NVIDIA 驱动，包内不安装显卡驱动。
windows-cpp：原生 Clang C/C++ 编译器、CMake、Ninja、clangd、clang-format 和 CodeLLDB 调试器。
每个组件安装在应用自有目录。归档、清单和逐文件摘要不匹配时停止导入，不替换已有选择。已有个人 Python 或 Conda 环境保持不变。

可选 WSL
需要 Linux 时，在运行环境页准备或选择 WSL。创建环境时选择 environment/ 文件夹，向导验证 WSL 安装介质及 Ubuntu 镜像；需要管理员确认或重启时按提示操作。应用不会自动重启 Windows。
安装或更新后首次以 WSL 启动时，应用联网下载本版本的 Linux 运行环境（约 280 MB），启动页显示进度；发行版中已解包本版本运行环境时不下载。下载失败时可选择重试、改用 Windows 原生运行（记录为执行目标，与在设置中切换相同）或退出。
WSL 的“准备 Ubuntu 开发工具”在明确安装后使用随包的 C/C++、GDB、CMake、Ninja、clangd 和 PHP 软件包，禁用软件下载。支持基线为 Ubuntu 26.04 amd64，其他发行版保留原有软件包并显示兼容性说明。

SSH 远程开发
在“设置 → 运行环境 → SSH 远程开发”添加或选择主机，Host 会在 x86_64 Linux 服务器上运行，界面留在本机。需要 Windows 自带的 OpenSSH 客户端，服务器需要 python3。首次连接时服务器联网下载本版本的 Linux 运行环境，失败时由本机下载后通过 SSH 上传；离线组件不用于服务器，请在服务器上用 apt、pip 等安装工具后点击“检测已有环境”。

运行与调试
支持 Windows 原生、WSL 和 SSH 的 Python、Node JavaScript、TypeScript、PHP 脚本及 C/C++ 单文件或 CMake 运行。Python、JavaScript/TypeScript 和 C/C++ 提供断点、单步、调用栈、变量和监视；PHP 仅提供运行。启动前保存修改，保存或构建失败时停止启动。
Windows 原生运行 PHP 前，在“可选组件”下载 PHP 8.5（含 json、openssl、mbstring、pdo_sqlite、curl、zip 扩展），或在运行配置中选择已安装的 php.exe；未下载时运行 PHP 文件会提示下载位置。已导入的环境组件或项目选定的 PHP 优先；WSL 和 SSH 使用该 Linux 系统中安装的 PHP。
可在 Strata 卡片启动本地模型，也可连接使用者配置的其他推理服务；安装科学计算库本身不会启动模型。聊天使用所选服务，无自动云端回退。

校验与验收范围
SHA256SUMS.txt 记录完整离线目录的交付文件摘要；核心同时校验发行组件目录，可选组件下载和组件导入分别验证自身文件。
原生 Host、程序运行与断点调试、Windows/Linux CPU/CUDA 计算和组件导入分别验收；旧版记录不代表本版本已完成验收。全新 Windows 的 UAC 和重启续装仍须各自验收，详细范围以随交付验收报告为准。
Develop by RainyMarks

使用许可
允许个人和企业内部免费使用；销售、收费托管及商业再分发须另行取得书面许可。完整条款见应用内 LICENSE，第三方组件仍适用各自许可。
