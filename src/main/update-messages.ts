/** Native update dialogs follow the language selected by the workbench. */
export const updateMessages = {
  zh: {
    title: 'RainyAgent 更新', current: '已是最新版本：', unavailable: '自动更新仅用于已安装的发行版。',
    failed: '更新未完成，应用和已有数据保持可用。请稍后通过“检查更新”重试。', ready: '更新已下载：',
    restartDetail: '重启前会保存草稿并关闭执行环境。选择“稍后”可继续工作，之后通过“检查更新”安装。',
    later: '稍后', install: '重启并安装', busy: '请先结束运行中的任务、程序、调试、终端及环境准备，再安装更新。',
  },
  en: {
    title: 'RainyAgent updates', current: 'You are up to date:', unavailable: 'Automatic updates require an installed release.',
    failed: 'The update did not complete. Your application and data remain available. Retry with Check for updates.', ready: 'Update downloaded:',
    restartDetail: 'RainyAgent will save drafts and close the execution environment before restarting. Choose Later to keep working, then use Check for updates to install.',
    later: 'Later', install: 'Restart and install', busy: 'Finish running tasks, programs, debugging, terminals, and environment setup before installing the update.',
  },
} as const
