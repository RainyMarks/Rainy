//! Chinese and English interface strings from `assets/messages.json`, the one place they are written (3.x kept them in
//! `src/renderer/**/messages.ts`). Keys are `module.key`, such as `chat.send`; `{name}` placeholders are filled from the
//! variables given.

use std::collections::HashMap;
use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Locale {
    #[default]
    Zh,
    En,
}

impl Locale {
    pub fn from_code(code: &str) -> Self {
        if code == "en" { Self::En } else { Self::Zh }
    }
}

type Table = HashMap<String, String>;

/// Strings of what 3.x's Electron main process showed (startup, targets, SSH, downloads, updates, quitting) and of
/// the MCP JSON import, whose keys the carrier hands over: `(key, Chinese, English)`.
const NATIVE: &[(&str, &str, &str)] = &[
    ("native.starting", "正在启动执行后端…", "Starting the execution backend…"),
    ("native.quit", "退出", "Quit"),
    ("native.ok", "确定", "OK"),
    ("native.hostStopped", "执行后端已停止", "The execution backend stopped."),
    ("native.switchTarget", "切换执行环境", "Switch execution environment"),
    ("native.aiEditReview", "新代码已放进编辑器（绿色底色）。可以接受、拒绝，或改写要求后按 Enter 重新生成。", "The new code is in the editor, tinted green. Accept it, reject it, or change the request and press Enter to generate again."),
    ("native.closeMap", "关闭项目地图", "Close the project map"),
    ("native.projectMap", "项目地图", "Project map"),
    ("native.noFormatter", "这个文件没有可用的格式化程序", "No formatter is available for this file"),
    ("native.noReferences", "没有找到引用", "No references found"),
    ("native.unsavedRoot", "先保存或关闭这个文件夹里有改动的文件，再把它移出项目", "Save or close the modified files in this folder before removing it from the project"),
    ("native.cut", "剪切", "Cut"),
    ("native.copy", "复制", "Copy"),
    ("native.paste", "粘贴", "Paste"),
    ("native.selectAll", "全选", "Select all"),
    ("native.languageStarting", "{language} 语言服务启动中", "{language} language service starting"),
    ("native.languageFailed", "{language} 语言服务不可用", "{language} language service unavailable"),
    ("native.languageToolsMissing", "{language}：点这里到“设置 → 工具”下载编程工具", "{language}: click to download its tools in Settings → Tools"),
    ("native.switchConfirm", "切换到 {target} 后 Rainy 会重新启动。已有对话保留在原来的执行环境。", "Rainy restarts in {target}. Existing chats stay in their execution environment."),
    ("native.switch", "切换", "Switch"),
    ("native.busyBeforeSwitch", "执行环境里还有任务在运行，请先结束再切换。", "Tasks are still running in this execution environment. Finish them before you switch."),
    ("native.retry", "重试", "Retry"),
    ("native.useWindows", "改用 Windows 原生运行", "Run on Windows instead"),
    ("native.keepMine", "保留我的修改并覆盖", "Keep my edits and overwrite"),
    ("native.projects", "项目", "Projects"),
    ("native.sshConnecting", "正在连接 {target}…", "Connecting to {target}…"),
    ("native.sshDownloading", "服务器正在下载 Linux 运行环境 {done} / {total} MB", "The server is downloading the Linux runtime {done} / {total} MB"),
    ("native.sshUploading", "正在上传 Linux 运行环境到 {name} {done} / {total} MB", "Uploading the Linux runtime to {name} {done} / {total} MB"),
    ("native.sshFirstTime", "第一次连接这台服务器", "Rainy has not connected to this server before"),
    ("native.sshFingerprint", "请向服务器管理员核对上面的指纹。信任后会记入 ~/.ssh/known_hosts，以后密钥变化时 SSH 会拒绝连接。", "Check the fingerprint above with the server's administrator. Once trusted it is recorded in ~/.ssh/known_hosts, and SSH refuses to connect if the key ever changes."),
    ("native.sshTrust", "信任并连接", "Trust and connect"),
    ("native.sshVerify", "SSH 需要验证", "SSH needs to verify you"),
    ("native.continue", "继续", "Continue"),
    ("native.sshLost", "与 {target} 的连接已断开", "The connection to {target} was lost"),
    ("native.sshLostDetail", "远程后端随连接一起停止了，会话历史保存在服务器上。重新连接会重新打开 Rainy。", "The remote backend stopped with the connection. Chat history is kept on the server. Reconnecting reopens Rainy."),
    ("native.reconnect", "重新连接", "Reconnect"),
    ("native.later", "稍后", "Later"),
    ("native.hostStoppedDetail", "{reason}。会话历史已保留，重新启动 Rainy 即可继续。", "{reason}. Your chat history is kept; restart Rainy to continue."),
    ("native.processKilled", "进程已被终止", "The process was terminated"),
    ("native.exitCode", "退出代码：{code}", "Exit code: {code}"),
    ("native.restart", "重新启动", "Restart"),
    ("native.startFailed", "Rainy 启动失败", "Rainy could not start."),
    ("native.windowsHostMissing", "Windows 原生运行文件缺失，请安装完整发行包。", "The files for running natively on Windows are missing. Install the full Rainy package."),
    ("native.verifying", "首次启动，正在校验安装文件 {done} / {total}", "First start: verifying installed files {done} / {total}"),
    ("native.trustInvalid", "发行清单无效：{detail}", "The Rainy file list is not valid: {detail}"),
    ("native.trustEncoding", "发行清单编码无效", "The Rainy file list is not encoded correctly."),
    ("native.trustSignature", "发行清单签名无效", "The Rainy file list has an invalid signature."),
    ("native.trustDuplicates", "资源清单包含重复路径", "The file list contains duplicate paths."),
    ("native.trustOutside", "发行资源路径越界", "A Rainy file path points outside the installation."),
    ("native.trustMissing", "发行资源缺失：{path}", "A Rainy file is missing: {path}"),
    ("native.trustDamaged", "发行资源损坏：{path}", "A Rainy file is damaged: {path}"),
    ("native.trustReplaced", "发行资源路径被替换：{path}", "The folder of a Rainy file was replaced: {path}"),
    ("native.trustMismatch", "发行资源校验失败：{path}", "A Rainy file failed verification: {path}"),
    ("native.trustUnreadable", "发行资源读取失败：{detail}", "A Rainy file could not be read: {detail}"),
    ("native.resourcesChanged", "Rainy 安装文件已被修改或损坏", "Rainy installation files were changed or damaged."),
    (
        "native.resourcesRecheck",
        "{message}\n下次启动时会重新完整校验；如果问题仍然存在，请重新安装 Rainy。",
        "{message}\nAll files will be checked again at the next start. If the problem remains, reinstall Rainy.",
    ),
    ("native.launchSsh", "无法在 SSH 服务器上打开本机文件", "Files on this computer cannot be opened on the SSH server"),
    (
        "native.launchSshDetail",
        "当前执行环境是 {target}。要打开本机的文件或文件夹，请先在设置的“运行环境”页切换到 Windows 或 WSL。\n\n{paths}",
        "The current execution environment is {target}. To open files or folders on this computer, switch to Windows or WSL on the Runtime page of Settings first.\n\n{paths}",
    ),
    ("native.launchMissing", "找不到要打开的文件或文件夹", "The file or folder to open was not found"),
    ("native.launchFailed", "无法打开所选的文件或文件夹", "The selected file or folder could not be opened"),
    ("native.groupSystem", "系统集成", "System integration"),
    ("native.contextMenuTitle", "资源管理器右键菜单", "Explorer context menu"),
    ("native.contextMenuDescription", "默认关闭。开启后，文件、文件夹和文件夹空白处的右键菜单里会出现“通过 Rainy 打开”（Windows 11 中在“显示更多选项”里）；关闭即移除", "Off unless you turn it on. When on, right-clicking a file, a folder or the background of a folder shows \"Open with Rainy\" (on Windows 11 under \"Show more options\"); turning it off removes it"),
    ("native.fileTypesTitle", "注册为文件的打开方式", "Offer Rainy for file types"),
    ("native.fileTypesDescription", "默认关闭。开启后，Markdown、文本和源代码文件的“打开方式”列表里会出现 Rainy；还没有默认应用的文件类型，Windows 可能直接用 Rainy 打开。关闭即移除，Rainy 不会改你已选的默认应用", "Off unless you turn it on. When on, Rainy is listed under \"Open with\" for Markdown, text and source code files, and Windows may open types that have no default app in Rainy. Turning it off removes this; Rainy never changes a default app you chose"),
    ("native.defaultAppsTitle", "设为默认应用", "Make Rainy the default app"),
    ("native.defaultAppsOpen", "打开系统设置…", "Open system settings…"),
    ("native.defaultAppsDescription", "Windows 只允许你自己选择默认应用：在打开的系统设置里把 .md 等类型改为 Rainy；也可以右键文件，选择“打开方式 → 选择其他应用”，选 Rainy 并点“始终”", "Windows lets only you choose default apps: in the settings that open, set Rainy for .md and other types. You can also right-click a file, choose \"Open with → Choose another app\", pick Rainy and click \"Always\""),
    ("native.shellUnavailable", "只有安装版 Rainy 可以修改这些设置", "Only an installed Rainy can change these settings"),
    ("native.shellFailed", "资源管理器集成没有更改", "Explorer integration was not changed"),
    ("native.sshAskpassFailed", "Rainy 无法启动用于回答 SSH 验证提问的服务。", "Rainy could not start the service that answers SSH questions."),
    ("native.startingWsl", "正在启动 WSL · {distro}…", "Starting WSL · {distro}…"),
    ("native.linuxDownloading", "正在下载 Linux 运行环境 {done} / {total} MB", "Downloading the Linux runtime {done} / {total} MB"),
    ("native.linuxDownloadFailed", "Linux 运行环境下载失败", "The Linux runtime could not be downloaded."),
    (
        "native.linuxDownloadDetail",
        "{error}\nWSL 和 SSH 远程开发用的 Linux 运行环境底座有变化，需要联网下载一次，已下载的部分会保留。",
        "{error}\nThe base of the Linux runtime that WSL and SSH remote development use has changed and needs a one-time download. What was already downloaded is kept.",
    ),
    ("native.linuxNotDownloaded", "未下载 Linux 运行环境，Rainy 已停止启动。", "Rainy did not start because the Linux runtime was not downloaded."),
    ("native.linuxMismatch", "下载的 WSL 运行环境与此版本不一致", "The downloaded WSL runtime does not match this version of Rainy."),
    ("native.modulesUnavailable", "此版本未提供可下载的组件", "This version does not offer components for download."),
    ("native.modulesInvalid", "此版本的组件清单无效：{detail}", "The component list of this version is not valid: {detail}"),
    ("native.moduleNotOffered", "此版本未提供可下载的组件：{id}", "This version does not offer this component for download: {id}"),
    ("native.moduleBusy", "另一个组件正在下载，请稍候", "Another component is downloading. Please wait."),
    ("native.moduleClosing", "Rainy 正在关闭", "Rainy is closing."),
    ("native.moduleRemoveBusy", "请等待组件下载完成后再删除", "Wait for the component download to finish before removing it."),
    ("native.moduleArchive", "组件压缩包无法解压：{detail}", "The component archive could not be unpacked: {detail}"),
    ("native.downloadInsecure", "下载地址不安全：{url}", "The download address is not secure: {url}"),
    ("native.downloadRedirects", "下载地址重定向次数过多", "The download was redirected too many times."),
    ("native.downloadNetwork", "下载失败（{detail}），请检查网络后重试", "The download failed ({detail}). Check your connection and try again."),
    ("native.downloadStalled", "下载在 {seconds} 秒内没有收到数据，请检查网络后重试", "No data arrived for {seconds} seconds. Check your connection and try again."),
    ("native.downloadInterrupted", "下载连接中断，请检查网络后重试", "The download connection was interrupted. Check your connection and try again."),
    ("native.downloadHttp", "下载失败（HTTP {status}），请检查网络后重试", "The download failed (HTTP {status}). Check your connection and try again."),
    ("native.downloadResume", "下载续传响应无效", "The server sent an invalid response when resuming the download."),
    ("native.downloadResponse", "下载响应无效", "The server sent an invalid download response."),
    ("native.downloadTooLarge", "下载内容超出清单大小", "The download is larger than expected."),
    ("native.downloadMismatch", "下载校验失败，请重试", "The download failed verification. Try again."),
    ("native.downloadFileMismatch", "下载文件校验失败，请重试", "The downloaded file failed verification. Try again."),
    ("native.downloadCache", "下载缓存无效，请清理下载缓存后重试", "The download cache is invalid. Clear the download cache and try again."),
    ("native.downloadLink", "安装路径包含链接或重解析目录：{path}", "The installation path contains a link or junction: {path}"),
    (
        "native.downloadCancelled",
        "工具安装已取消。已校验的暂存文件会在重试时复用。",
        "The tool installation was cancelled. Verified staged files will be reused when you retry.",
    ),
    ("native.downloadIo", "文件读写失败：{detail}", "A file could not be read or written: {detail}"),
    // Updates (`store/update.rs`)
    ("native.updateTitle", "Rainy 更新", "Rainy updates"),
    ("native.updateCurrent", "已是最新版本：", "You are up to date:"),
    ("native.updateUnavailable", "自动更新仅用于已安装的发行版。", "Automatic updates require an installed release."),
    (
        "native.updateFailed",
        "更新未完成，应用和已有数据保持可用。请稍后通过“检查更新”重试。",
        "The update did not complete. Your application and data remain available. Retry with Check for updates.",
    ),
    ("native.updateReady", "更新已下载：", "Update downloaded:"),
    (
        "native.updateRestartDetail",
        "重启前会保存草稿并关闭执行环境。选择“稍后”可继续工作，之后通过“检查更新”安装。",
        "Rainy will save drafts and close the execution environment before restarting. Choose Later to keep working, then use Check for updates to install.",
    ),
    ("native.updateLater", "稍后", "Later"),
    ("native.updateInstall", "重启并安装", "Restart and install"),
    (
        "native.updateBusy",
        "请先结束运行中的任务、程序、调试、终端及环境准备，再安装更新。",
        "Finish running tasks, programs, debugging, terminals, and environment setup before installing the update.",
    ),
    ("native.checkUpdates", "检查更新…", "Check for updates…"),
    ("native.updateDownloading", "正在下载更新 {version}：{percent}%", "Downloading update {version}: {percent}%"),
    ("native.updateInvalid", "更新信息无效：{detail}", "The update information is not valid: {detail}"),
    ("native.updateWebInstaller", "更新信息要求网页安装程序，Rainy 不使用这种安装方式", "The update information describes a web installer, which Rainy does not use."),
    ("native.updateSignature", "更新信息的签名无效", "The signature of the update information is not valid."),
    ("native.updateLaunchFailed", "无法启动安装程序：{detail}", "The installer could not be started: {detail}"),
    (
        "native.updateNoFolder",
        "找不到存放更新的文件夹：这次登录没有设置 LOCALAPPDATA 环境变量。",
        "There is no folder to keep updates in: LOCALAPPDATA is not set for this session.",
    ),
    (
        "native.updateCleanupFailed",
        "环境组件的操作未能及时停止，安装程序没有启动。",
        "An environment component operation did not stop in time, so the installer was not started.",
    ),
    // Quitting with drafts that could not be saved
    ("native.draftsUnsaved", "草稿尚未保存", "Your drafts have not been saved."),
    ("native.draftsUnsavedDetail", "保存失败，窗口和草稿已保留。", "Saving failed. The window and your drafts are kept."),
    ("native.retrySaveQuit", "重试保存并退出", "Retry saving and quit"),
    ("native.dontQuit", "取消退出", "Do not quit"),
    // The diagnostics export (`store/diagnostics.rs`)
    ("native.diagnosticsSaveTitle", "导出诊断信息", "Export diagnostics"),
    ("native.diagnosticsSaving", "无法保存 {path}", "Could not save {path}"),
    // Settings → Skills & MCP → Import JSON (`carrier/src/mcp_json.rs` names the keys)
    ("native.mcpImport", "从 JSON 导入", "Import JSON"),
    ("native.mcpImportTitle", "从 JSON 导入 MCP 服务器", "Import MCP servers from JSON"),
    (
        "native.mcpImportHint",
        "粘贴 MCP 服务器说明页里的 JSON，或 Claude Desktop、Cursor、VS Code、Cherry Studio、Trae 等应用的 MCP 配置，也可以选择一个配置文件。注释、多余的逗号、中文引号和 Markdown 代码块都能识别。已有的服务器不会被覆盖，重名的会自动改名。",
        "Paste the JSON from an MCP server's page, or the MCP configuration of Claude Desktop, Cursor, VS Code, Cherry Studio, Trae or a similar app, or choose a configuration file. Comments, extra commas, curly quotes and Markdown code blocks are understood. Existing servers are never replaced; a name already in use gets a new one.",
    ),
    ("native.mcpImportLabel", "MCP 配置 JSON", "MCP configuration JSON"),
    ("native.mcpImportFile", "选择文件…", "Choose file…"),
    ("native.mcpImportFileError", "无法读取文件：{error}", "The file could not be read: {error}"),
    ("native.mcpImportFound", "找到 {count} 个服务器，勾选要导入的：", "Found {count} servers. Check the ones to import:"),
    ("native.mcpImportInclude", "导入 {name}", "Import {name}"),
    ("native.mcpImportSubmit", "导入 {count} 个服务器", "Import {count} servers"),
    ("native.mcpImportDone", "已导入 {count} 个服务器", "Imported {count} servers"),
    ("native.mcpImportFailed", "导入 {name} 时出错：{error}。在它之前的 {count} 个已导入。", "Importing {name} failed: {error}. The {count} servers before it were imported."),
    ("native.mcpImportSkipped", "以下条目无法导入：", "These entries cannot be imported:"),
    ("native.mcpImportEnv", "环境变量：{keys}", "Environment: {keys}"),
    ("native.mcpImportHeaders", "请求头：{keys}", "Headers: {keys}"),
    ("native.mcpJsonEmpty", "粘贴 MCP 配置 JSON，或选择一个文件", "Paste MCP configuration JSON, or choose a file."),
    ("native.mcpJsonTooLarge", "内容超过 512 KB，不像 MCP 配置", "The text is over 512 KB, which is not an MCP configuration."),
    ("native.mcpJsonInvalid", "无法识别为 JSON：{detail}", "This is not JSON Rainy can read: {detail}"),
    (
        "native.mcpJsonNoServers",
        "没有找到 MCP 服务器。需要 mcpServers、servers 这类字段，或带 command / url 的服务器配置。",
        "No MCP server was found. Rainy looks for mcpServers, servers and similar fields, or a server with command or url.",
    ),
    ("native.mcpJsonTooMany", "一次最多导入 {max} 个服务器", "At most {max} servers can be imported at once."),
    (
        "native.mcpJsonToml",
        "这是 Codex 的 TOML 配置，不是 JSON。请粘贴 JSON 格式的配置，或用“添加服务器”手动填写。",
        "This is Codex's TOML configuration, not JSON. Paste the JSON form, or enter the server with Add server.",
    ),
    (
        "native.mcpJsonYaml",
        "这是 YAML 配置，不是 JSON。请粘贴 JSON 格式的配置，或用“添加服务器”手动填写。",
        "This is YAML, not JSON. Paste the JSON form, or enter the server with Add server.",
    ),
    (
        "native.mcpJsonCommandLine",
        "这是一行安装命令，不是 JSON。请粘贴说明页里的 JSON 配置，或用“添加服务器”把命令和参数填进去。",
        "This is an install command, not JSON. Paste the JSON from the server's page, or enter its command and arguments with Add server.",
    ),
    ("native.mcpJsonServerInvalid", "{name}：不是服务器配置", "{name}: not a server configuration."),
    ("native.mcpJsonUnknownType", "{name}：Rainy 不支持 {type} 类型的服务器", "{name}: Rainy does not support {type} servers."),
    ("native.mcpJsonBadUrl", "{name}：URL 需要以 http:// 或 https:// 开头", "{name}: the URL must start with http:// or https://."),
    (
        "native.mcpJsonBadHeader",
        "{name}：请求头 {key} 无效（只能用英文字符，不能换行）或重复",
        "{name}: header {key} is invalid (Latin characters only, no line breaks) or repeated.",
    ),
    ("native.mcpJsonNoCommand", "{name}：缺少启动命令（command）或 URL", "{name}: no command or URL."),
    (
        "native.mcpJsonBadArg",
        "{name}：启动命令或参数里有无法使用的内容（参数只能是文字，且不能换行）",
        "{name}: the command or its arguments hold something unusable (arguments must be text without line breaks).",
    ),
    (
        "native.mcpJsonBadEnv",
        "{name}：环境变量 {key} 无效（名称只能用字母、数字和下划线，值不能换行）",
        "{name}: environment variable {key} is invalid (names use letters, digits and underscores; values cannot break lines).",
    ),
    ("native.mcpJsonRenamed", "名称 {from} 改为 {to}", "Renamed {from} to {to}."),
    (
        "native.mcpJsonPlaceholder",
        "{field} 里的 {value} 是占位符，导入后请点“编辑”换成真实的值",
        "{value} in {field} is a placeholder: after importing, choose Edit and enter the real value.",
    ),
    ("native.mcpJsonCommandSplit", "写在一起的命令行已拆成启动命令和参数", "The command line written as one string was split into the command and its arguments."),
    ("native.mcpJsonUnwrapped", "已改为直接运行 {command}：cmd /c 和 .cmd 只在 Windows 上需要", "Runs {command} directly: cmd /c and .cmd are only needed on Windows."),
    (
        "native.mcpJsonCwd",
        "Rainy 不支持工作目录（cwd），已忽略；参数里的相对路径可能找不到",
        "Rainy has no working directory setting (cwd), so it was left out; relative paths in the arguments may not be found.",
    ),
    ("native.mcpJsonEnvFile", "Rainy 不读取 envFile，请把需要的环境变量直接写进配置", "Rainy does not read envFile: put the variables you need in the configuration itself."),
    (
        "native.mcpJsonExcludeTools",
        "要关闭的工具列表无法导入：连接后可在“编辑”里勾选允许的工具",
        "Lists of tools to turn off cannot be imported: after connecting, choose Edit and check the allowed tools.",
    ),
    (
        "native.mcpJsonNoTools",
        "原配置没有允许这个服务器的任何工具，所以导入为停用；需要时点“编辑”勾选允许的工具",
        "The original configuration allows none of this server's tools, so it is imported disabled: choose Edit to allow tools.",
    ),
    (
        "native.mcpJsonInlineScript",
        "这个服务器会运行写在参数里的一段脚本。导入后它会立即运行，请先看清上面的命令、确认来源可信",
        "This server runs a script written into its arguments, and starts as soon as it is imported. Read the command above and make sure you trust where it came from.",
    ),
    ("native.mcpJsonDuplicate", "{name}：和已有的服务器 {existing} 完全相同，不再重复添加", "{name}: the same as the configured server {existing}, so it is not added again."),
    (
        "native.mcpJsonOAuth",
        "Rainy 不支持 OAuth 登录，需要令牌时请在请求头里填写，例如 Authorization: Bearer …",
        "Rainy does not sign in with OAuth: put a token in the request headers instead, such as Authorization: Bearer ….",
    ),
    (
        "native.mcpJsonForeignPath",
        "{value} 是另一种系统的路径，在 Rainy 运行命令的地方可能不存在，请核对",
        "{value} is a path from another system and may not exist where Rainy runs commands. Check it.",
    ),
    ("native.mcpJsonBackslashes", "Windows 路径里的单个反斜杠已按原样读取，请核对路径", "Single backslashes in Windows paths were read as written. Check the paths."),
];

fn tables() -> &'static (Table, Table) {
    static TABLES: OnceLock<(Table, Table)> = OnceLock::new();
    TABLES.get_or_init(|| {
        let modules: HashMap<String, HashMap<String, HashMap<String, String>>> =
            serde_json::from_str(include_str!("../assets/messages.json")).expect("valid messages.json");
        let (mut zh, mut en) = (Table::new(), Table::new());
        for (module, languages) in modules {
            for (language, target) in [("zh", &mut zh), ("en", &mut en)] {
                for (key, text) in &languages[language] {
                    target.insert(format!("{module}.{key}"), text.clone());
                }
            }
        }
        for (key, chinese, english) in NATIVE {
            zh.insert((*key).to_owned(), (*chinese).to_owned());
            en.insert((*key).to_owned(), (*english).to_owned());
        }
        (zh, en)
    })
}

/// Look up one message; an unknown key shows itself, so a missing string is visible rather than blank.
pub fn text(locale: Locale, key: &str) -> &'static str {
    let (zh, en) = tables();
    let table = if locale == Locale::En { en } else { zh };
    table.get(key).map(String::as_str).unwrap_or_else(|| missing(key))
}

/// An unknown key as a lasting string, made once per key, however often a view asks for it.
fn missing(key: &str) -> &'static str {
    static MISSING: OnceLock<std::sync::Mutex<std::collections::HashSet<&'static str>>> = OnceLock::new();
    let mut known = MISSING.get_or_init(Default::default).lock().unwrap();
    match known.get(key) {
        Some(text) => text,
        None => {
            log::warn!("No message for {key}");
            let text: &'static str = Box::leak(key.to_owned().into_boxed_str());
            known.insert(text);
            text
        }
    }
}

/// Look up one message and fill its `{name}` placeholders.
pub fn format(locale: Locale, key: &str, vars: &[(&str, &dyn std::fmt::Display)]) -> String {
    let template = text(locale, key);
    if vars.is_empty() {
        return template.to_owned();
    }
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        match after.find('}') {
            Some(close) if after[..close].chars().all(|c| c.is_alphanumeric() || c == '_') => {
                let name = &after[..close];
                match vars.iter().find(|(var, _)| *var == name) {
                    Some((_, value)) => out.push_str(&value.to_string()),
                    None => out.push_str(&rest[open..open + close + 2]),
                }
                rest = &after[close + 1..];
            }
            _ => {
                out.push('{');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// The meaning of the Host's failure codes, for messages it could not write in the interface language (3.x
/// `describeIdeError`).
const ERROR_KEYS: &[(&str, &str)] = &[
    ("not-found", "ide.ideMissingPath"),
    ("invalid-path", "ide.ideInvalidPath"),
    ("outside-workspace", "ide.ideOutsideRoot"),
    ("workspace-unavailable", "ide.ideUnavailableRoot"),
    ("unsaved-root", "ide.ideUnsavedRoot"),
    ("root-overlap", "ide.ideRootOverlap"),
    ("already-exists", "ide.ideAlreadyExists"),
    ("not-file", "ide.ideNotFile"),
    ("not-directory", "ide.ideNotDirectory"),
    ("read-only", "ide.ideReadOnly"),
    ("too-large", "ide.ideTooLarge"),
    ("version-conflict", "ide.ideVersionConflict"),
    ("permission-denied", "ide.idePermissionDenied"),
    ("workspace-not-found", "ide.ideWorkspaceNotFound"),
    ("revision-conflict", "ide.ideRecoveryConflict"),
    ("state-too-large", "ide.ideStateTooLarge"),
    ("confirmation-required", "ide.ideConfirmationRequired"),
    ("aborted", "ide.ideAborted"),
    ("closed", "ide.ideClosed"),
    ("diff-unavailable", "ide.ideDiffUnavailable"),
    ("io-error", "ide.ideIoError"),
];

fn is_cjk(c: char) -> bool {
    matches!(c, '\u{3000}'..='\u{303f}' | '\u{3400}'..='\u{9fff}' | '\u{ff00}'..='\u{ffef}')
}

/// A Host failure in the interface language: what to show, and the Host's own words when they are kept as detail.
/// The Host writes most messages in the chosen language; one taken from the system ("No such file or directory (os
/// error 2)", on WSL and SSH always English) or from code without a Chinese text is replaced by the meaning of its
/// code. A lost connection always reads the same.
pub fn describe_error(locale: Locale, error: &rainy_protocol::RpcError) -> (String, Option<String>) {
    if error.code == "disconnected" {
        return (text(locale, "app.connectionLost").to_owned(), None);
    }
    let raw = error.message.trim();
    let untranslated = raw.contains("(os error") || (locale == Locale::Zh && !raw.chars().any(is_cjk));
    match ERROR_KEYS.iter().find(|(code, _)| *code == error.code) {
        Some((_, key)) if untranslated || raw.is_empty() => (text(locale, key).to_owned(), (!raw.is_empty()).then(|| raw.to_owned())),
        _ if raw.is_empty() => (text(locale, "chat.errorTitle").to_owned(), None),
        _ => (raw.to_owned(), None),
    }
}

/// [`describe_error`] as one text, the detail on its own line.
pub fn error_text(locale: Locale, error: &rainy_protocol::RpcError) -> String {
    match describe_error(locale, error) {
        (text, Some(detail)) => format!("{text}\n{detail}"),
        (text, None) => text,
    }
}

#[cfg(test)]
mod tests {
    use ::core::prelude::v1::test;

    use super::*;

    #[test]
    fn looks_up_and_formats() {
        assert_eq!(text(Locale::Zh, "chat.send"), "发送");
        assert_eq!(text(Locale::En, "chat.send"), "Send");
        assert_eq!(format(Locale::Zh, "chat.exitCode", &[("code", &3)]), "退出码 3");
        assert_eq!(format(Locale::Zh, "chat.toolRead", &[]), "读取 {path}");
        assert_eq!(text(Locale::Zh, "chat.noSuchKey"), "chat.noSuchKey");
    }

    #[test]
    fn describes_host_failures() {
        let error = |code: &str, message: &str| rainy_protocol::RpcError { code: code.into(), message: message.into(), data: None };
        let raw = error("not-found", "No such file or directory (os error 2)");
        assert_eq!(describe_error(Locale::Zh, &raw), (text(Locale::Zh, "ide.ideMissingPath").to_owned(), Some(raw.message.clone())));
        assert_eq!(describe_error(Locale::En, &raw).0, text(Locale::En, "ide.ideMissingPath"));
        // The Host's own Chinese text stays.
        assert_eq!(describe_error(Locale::Zh, &error("not-found", "对话不存在。")), ("对话不存在。".to_owned(), None));
        assert_eq!(describe_error(Locale::En, &error("not-found", "This chat does not exist.")).0, "This chat does not exist.");
        assert_eq!(describe_error(Locale::Zh, &error("disconnected", "The execution backend is not connected.")).0, text(Locale::Zh, "app.connectionLost"));
        assert_eq!(
            error_text(Locale::Zh, &error("permission-denied", "Permission denied (os error 13)")),
            format!("{}\nPermission denied (os error 13)", text(Locale::Zh, "ide.idePermissionDenied"))
        );
        assert_eq!(describe_error(Locale::Zh, &error("execution-error", "boom")).0, "boom");
    }

    #[test]
    fn has_every_mcp_import_message_in_both_languages() {
        let keys: Vec<&str> = NATIVE.iter().map(|(key, _, _)| *key).filter(|key| key.starts_with("native.mcpJson") || key.starts_with("native.mcpImport")).collect();
        assert!(keys.len() > 30);
        for key in rainy_carrier::mcp_json::MESSAGE_KEYS {
            let key = format!("native.{key}");
            assert!(keys.contains(&key.as_str()), "{key}");
            assert_ne!(text(Locale::Zh, &key), text(Locale::En, &key), "{key}");
        }
        assert_eq!(
            format(Locale::Zh, "native.mcpJsonPlaceholder", &[("field", &"args"), ("value", &"${HOME}")]),
            "args 里的 ${HOME} 是占位符，导入后请点“编辑”换成真实的值"
        );
    }
}
