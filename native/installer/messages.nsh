; What the Rainy setup and uninstaller say, in Simplified Chinese and English (the language follows Windows). Texts 3.x
; had in resources/installer.nsh are kept as they were.

LangString rainyRunApp ${LANG_SIMPCHINESE} "运行 ${PRODUCT_NAME}"
LangString rainyRunApp ${LANG_ENGLISH} "Run ${PRODUCT_NAME}"

LangString rainyWindowsRequired ${LANG_SIMPCHINESE} "${PRODUCT_NAME} 需要 64 位的 Windows 10 或更高版本。"
LangString rainyWindowsRequired ${LANG_ENGLISH} "${PRODUCT_NAME} needs 64-bit Windows 10 or later."

LangString rainySetupRunning ${LANG_SIMPCHINESE} "另一个 Rainy 安装程序正在运行，请等它结束后再试。"
LangString rainySetupRunning ${LANG_ENGLISH} "Another Rainy setup is running. Try again when it has finished."

LangString rainyAppRunning ${LANG_SIMPCHINESE} "请先保存工作并手动关闭 Rainy 及其命令行窗口，再重新运行安装程序。安装程序不会自动结束这些进程。"
LangString rainyAppRunning ${LANG_ENGLISH} "Save your work and close Rainy and its command windows yourself, then run setup again. Setup does not end these processes."

LangString rainyAppRunningRetry ${LANG_SIMPCHINESE} "Rainy 或它启动的程序仍在运行。请先保存工作并手动关闭 Rainy 及其命令行窗口，然后选择“重试”。安装程序不会自动结束这些进程。"
LangString rainyAppRunningRetry ${LANG_ENGLISH} "Rainy or a program it started is still running. Save your work and close Rainy and its command windows yourself, then choose Retry. Setup does not end these processes."

LangString rainyFolderInvalid ${LANG_SIMPCHINESE} "应用文件目录校验失败，已保留原有文件。"
LangString rainyFolderInvalid ${LANG_ENGLISH} "The application folder could not be verified; the existing files were kept."

LangString rainyReparseInstall ${LANG_SIMPCHINESE} "应用文件目录包含重解析点，已停止安装并保留所有数据："
LangString rainyReparseInstall ${LANG_ENGLISH} "The application folder contains a link or another reparse point. Setup stopped and kept all data: "

LangString rainyReparseUninstall ${LANG_SIMPCHINESE} "应用文件目录包含重解析点，已停止卸载并保留所有数据："
LangString rainyReparseUninstall ${LANG_ENGLISH} "The application folder contains a link or another reparse point. Uninstalling stopped and kept all data: "

LangString rainyFileBusy ${LANG_SIMPCHINESE} "应用文件仍被占用或无法删除，请关闭 Rainy 后重试："
LangString rainyFileBusy ${LANG_ENGLISH} "An application file is in use or cannot be deleted. Close Rainy and try again: "

LangString rainyDataKept ${LANG_SIMPCHINESE} "本地 Linux 环境和用户数据已保留。"
LangString rainyDataKept ${LANG_ENGLISH} "The local Linux environment and user data were kept."

LangString rainyElevationFailed ${LANG_SIMPCHINESE} "这份 Rainy 是为这台电脑的所有用户安装的，更改它需要管理员权限，但没有获得授权。没有做任何更改。"
LangString rainyElevationFailed ${LANG_ENGLISH} "This Rainy is installed for all users of this computer, so changing it needs administrator rights, which were not granted. Nothing was changed."

LangString rainyWebView2Missing ${LANG_SIMPCHINESE} "Rainy 的网页、PDF 和文档预览需要 Microsoft Edge WebView2 运行时，这台电脑上没有找到它。Rainy 的其他功能不受影响。$\r$\n$\r$\n可以从 ${WEBVIEW2_URL} 免费下载安装。现在打开下载页面吗？"
LangString rainyWebView2Missing ${LANG_ENGLISH} "Rainy's web, PDF and document previews need the Microsoft Edge WebView2 Runtime, which was not found on this computer. Everything else in Rainy works without it.$\r$\n$\r$\nIt is a free download at ${WEBVIEW2_URL}. Open the download page now?"
