/** Local progress presentation; the native owner performs cancellation and window lifecycle. */
const api = window.__RAINY_TOOLPACK__
const { say, setupLocale } = window
const byId = id => document.getElementById(id)
const labels = {
  prepare: say('准备安装', 'Preparing'), preparing: say('准备安装', 'Preparing'),
  'checking-media': say('校验安装文件', 'Checking installation files'), 'checking-space': say('检查磁盘空间', 'Checking disk space'),
  extracting: say('解包工具', 'Unpacking tools'), verifying: say('校验工具文件', 'Checking tool files'), preserving: say('保留个人设置', 'Keeping your settings'),
  switching: say('切换工具版本', 'Switching tool versions'), 'rolling-back': say('恢复原有工具', 'Restoring the previous tools'),
  complete: say('安装完成', 'Installed'), cancelled: say('已取消安装', 'Installation cancelled'),
  error: say('安装未完成', 'Installation incomplete'), failed: say('安装未完成', 'Installation incomplete'),
}
const measuredPhases = new Set(['checking-media', 'extracting', 'verifying', 'complete'])
const terminalPhases = new Set(['complete', 'cancelled', 'error', 'failed'])
const number = new Intl.NumberFormat(setupLocale, { maximumFractionDigits: 1 })
let latest
let revision = 0
let cancelRequested = false
let disposed = false
let unsubscribe = () => {}

function bytes(value) {
  const units = ['B', 'KiB', 'MiB', 'GiB', 'TiB']
  let unit = 0
  while (value >= 1024 && unit < units.length - 1) { value /= 1024; unit++ }
  return `${number.format(value)} ${units[unit]}`
}

function render(snapshot) {
  if (disposed) return
  latest = snapshot
  const terminal = terminalPhases.has(snapshot.phase)
  if (terminal) cancelRequested = false
  const cancelling = !terminal && (cancelRequested || snapshot.cancelling)
  byId('phase').textContent = Object.hasOwn(labels, snapshot.phase) ? labels[snapshot.phase] : say('工具安装', 'Tool installation')
  byId('message').textContent = snapshot.message
  byId('current-path').textContent = snapshot.currentPath ?? ''
  byId('current-path').hidden = !snapshot.currentPath
  const progress = byId('progress')
  const measured = measuredPhases.has(snapshot.phase) && snapshot.totalBytes > 0
  if (measured) {
    const completed = Math.min(snapshot.totalBytes, Math.max(0, snapshot.completedBytes))
    const percent = number.format(completed / snapshot.totalBytes * 100)
    progress.max = snapshot.totalBytes
    progress.value = completed
    progress.setAttribute('aria-valuetext', say(`当前阶段 ${percent}%，${bytes(completed)} / ${bytes(snapshot.totalBytes)}`, `This step ${percent}%, ${bytes(completed)} of ${bytes(snapshot.totalBytes)}`))
    byId('percent').textContent = `${percent}%`
    byId('bytes').textContent = `${bytes(completed)} / ${bytes(snapshot.totalBytes)}`
  } else {
    progress.removeAttribute('value')
    progress.removeAttribute('aria-valuetext')
    byId('percent').textContent = ''
    byId('bytes').textContent = ''
  }
  progress.hidden = terminal && !measured
  byId('cancel').disabled = terminal || cancelling
  byId('cancel').textContent = cancelling ? say('正在取消…', 'Cancelling…') : snapshot.phase === 'cancelled' ? say('已取消', 'Cancelled') : say('取消安装', 'Cancel installation')
  byId('cancel-state').textContent = cancelling ? say('正在安全停止，请稍候', 'Stopping safely, please wait') : ''
  byId('cancel-state').removeAttribute('data-error')
}

async function cancel() {
  if (disposed || api === undefined || latest === undefined || cancelRequested || latest.cancelling || terminalPhases.has(latest.phase)) return
  cancelRequested = true
  render(latest)
  try { await api.cancel() }
  catch (error) {
    if (disposed || terminalPhases.has(latest.phase)) return
    cancelRequested = false
    render(latest)
    byId('cancel-state').textContent = error instanceof Error && error.message ? error.message : say('取消请求未能发送，请重试', 'The cancel request was not sent. Try again.')
    byId('cancel-state').setAttribute('data-error', '')
  }
}

byId('cancel').addEventListener('click', () => { void cancel() })

function disconnect() {
  if (disposed) return
  disposed = true
  unsubscribe()
}

window.addEventListener('pagehide', disconnect, { once: true })
window.addEventListener('unload', disconnect, { once: true })

if (api === undefined) {
  byId('phase').textContent = say('安装服务未连接', 'Installer not connected')
  byId('message').textContent = say('请关闭此窗口后重新运行安装程序', 'Close this window and run the installer again.')
  byId('progress').hidden = true
} else {
  unsubscribe = api.onProgress(snapshot => { revision++; render(snapshot) })
  const requestedAt = revision
  api.getProgress().then(snapshot => { if (revision === requestedAt) render(snapshot) }).catch(error => {
    if (disposed || revision !== requestedAt) return
    byId('phase').textContent = say('无法读取安装进度', 'Cannot read the installation progress')
    byId('message').textContent = error instanceof Error && error.message ? error.message : say('请关闭此窗口后重新运行安装程序', 'Close this window and run the installer again.')
    byId('progress').hidden = true
  })
}
