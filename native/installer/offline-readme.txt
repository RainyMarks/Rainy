Rainy 4.3.0 Windows x64 完整离线发行包

安装与升级
1. 运行 Rainy-4.3.0-windows-x64-setup.exe 安装核心，内含原生界面和 Windows Host，不含 Node.js。新安装默认使用 Windows 原生运行，无需先安装 WSL。
2. 完整离线交付请保留整个 offline-4.3.0 目录，包括 environment/、environment-components/、清单及 SHA256SUMS.txt。无需手动解压任何运行环境归档。安装后直接使用，无需设备码或激活码。
3. 安装版启动时静默检查 GitHub 稳定版并后台下载核心更新；“帮助 → 检查更新”可查看状态或重试。下载完成后选择稍后或重启安装，只有确认、草稿保存与 Host 清理完成后才安装；普通退出不会安装。自动更新保留已有环境，不重新下载离线环境组件。
4. 启动约 60 秒后，应用删除旧安装程序留在 resources 目录中的 strata-runtime、php、linux-runtime.tar.gz 及旧版工具目录文件（native-tools-*.json）。

工具（设置 → 工具）
Rainy 默认不带任何第三方工具，需要哪个就下载哪个，不用了随时卸载：
- Python 编程工具（约 26 MB）：Ruff、ty、debugpy，用于 Python 的补全、格式化和调试。
- JavaScript / TypeScript 编程工具（约 11 MB）：TypeScript 7 原生编译器和 js-debug。
- PHP 8.5（约 38 MB）：Windows 原生运行 PHP 时使用。
这些组件由安装包内签名的 optional-modules.json 固定，从 GitHub 发行分片下载到 %APPDATA%\RainyAgent\modules\<id>。每个分片和完整归档都校验 SHA-256，支持断点续传。编辑某种语言而缺少工具时，状态栏会提示，点击即可打开下载页。

Strata 本地大模型
在“设置 → 工具 → Strata 本地大模型”点击下载，Rainy 下载 Strata 官方最新版本，校验安装清单、GitHub 摘要和 Strata 的签名更新源，再按这台电脑的显卡安装推理引擎；没有模型运行时自动更新到最新版。
模型列表来自 Strata 官方目录，每个尺寸都标出下载大小、需要的内存以及在这台电脑上是否放得下，并标出 Strata 推荐的尺寸。点击下载（可选 Hugging Face 或 ModelScope，默认放在空间最大的非系统盘），完成后点击“启动”，再点“连接并设为默认模型”即可对话。
Strata 的文件放在 %APPDATA%\RainyAgent\tools\strata（模型除外），随 Rainy 退出一起结束；“卸载”删除它，并可选择同时删除模型。WSL NAT 无法连接 Windows loopback 时请选择 Windows 执行环境；SSH 服务器上的 Host 无法使用本机的 Strata。应用不会自动回退到云端模型。

项目与执行环境
打开文件夹作为项目，可再添加其他根目录。编辑器草稿、文件标签和布局按项目恢复，保存冲突先显示差异。
在“设置 → 运行环境”检测已有 Python、Conda、虚拟环境、Node、PHP 和 C/C++ 编译器，再选择项目解释器。检测不安装软件包，不修改已有环境或全局 PATH。
Windows 原生使用 Windows 路径及 PowerShell；WSL 使用该发行版中的路径及 Bash；SSH 使用服务器上的路径及 Bash。运行、调试、语言服务、终端和 Agent 命令共享所选项目环境。

导入离线运行组件
在运行环境页点击准备环境，选择“导入离线组件”，打开 environment-components/ 中与当前执行目标匹配的 JSON。导入完成后再次检测环境并选择解释器。
windows-basic / linux-basic：Python 3.12、pip、Node 24 和常用 Python 基础库；Windows 组件另含 PHP、Git 与 PowerShell。
windows-science-cpu / linux-science-cpu：独立 CPU 科学计算环境，包含 NumPy、SciPy、pandas、scikit-learn、图像处理、Jupyter 和常用深度学习库。
windows-science-cuda / linux-science-cuda：独立 CUDA 科学计算环境，需要本机兼容的 NVIDIA 驱动，包内不安装显卡驱动。
windows-cpp：原生 Clang C/C++ 编译器、CMake、Ninja、clangd、clang-format 和 CodeLLDB 调试器。
每个组件安装在应用自有目录。归档、清单和逐文件摘要不匹配时停止导入，不替换已有选择。

可选 WSL
需要 Linux 时，在运行环境页准备或选择 WSL。创建环境时选择 environment/ 文件夹，向导验证 WSL 安装介质及 Ubuntu 镜像；需要管理员确认或重启时按提示操作。应用不会自动重启 Windows。
Linux 运行环境分两层：底座（Linux 版 Ruff、ty、TypeScript 原生编译器、debugpy 和 js-debug，下载约 27 MB）只在内容变化时才需要联网下载；应用层（Host 和预览页面）随安装包附带。下载失败时可选择重试、改用 Windows 原生运行或退出。

SSH 远程开发
在“设置 → 运行环境 → SSH 远程开发”添加或选择主机，Host 会在 x86_64 Linux 服务器上运行，界面留在本机。需要 Windows 自带的 OpenSSH 客户端，服务器需要 python3。首次连接时服务器联网下载本版本的 Linux 运行环境，失败时由本机下载后通过 SSH 上传。

校验与验收范围
SHA256SUMS.txt 记录完整离线目录的交付文件摘要；核心同时校验发行组件目录，可选组件下载和组件导入分别验证自身文件。详细验收范围见 GitHub 仓库中的 docs/validation.md。
Develop by RainyMarks

使用许可
Rainy 自有代码使用 PolyForm Strict License 1.0.0：非商业用途（个人学习研究、学校、科研和公益机构等）可免费使用；不得修改（包括改名、换品牌和用 AI 改写）或分发，不得删除版权声明或冒充作者；包括企业内部使用在内的商业用途须另行取得商业授权。完整条款见应用内 LICENSE 和 LICENSE.Rainy，第三方组件仍适用各自许可。
