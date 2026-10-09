/** Local progress presentation; the native owner performs cancellation and window lifecycle. */
const api = window.__RAINY_TOOLPACK__
const byId = id => document.getElementById(id)
const labels = {
  prepare: '准备安装', preparing: '准备安装',
  'checking-media': '校验安装文件', 'checking-space': '检查磁盘空间',
  extracting: '解包工具', verifying: '校验工具文件', preserving: '保留个人设置',
  switching: '切换工具版本', 'rolling-back': '恢复原有工具',
  complete: '安装完成', cancelled: '已取消安装', error: '安装未完成', failed: '安装未完成',
}
const measuredPhases = new Set(['checking-media', 'extracting', 'verifying', 'complete'])
const terminalPhases = new Set(['complete', 'cancelled', 'error', 'failed'])
const number = new Intl.NumberFormat('zh-CN', { maximumFractionDigits: 1 })
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
  byId('phase').textContent = Object.hasOwn(labels, snapshot.phase) ? labels[snapshot.phase] : '工具安装'
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
    progress.setAttribute('aria-valuetext', `当前阶段 ${percent}%，${bytes(completed)} / ${bytes(snapshot.totalBytes)}`)
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
  byId('cancel').textContent = cancelling ? '正在取消…' : snapshot.phase === 'cancelled' ? '已取消' : '取消安装'
  byId('cancel-state').textContent = cancelling ? '正在安全停止，请稍候' : ''
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
    byId('cancel-state').textContent = error instanceof Error && error.message ? error.message : '取消请求未能发送，请重试'
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
  byId('phase').textContent = '安装服务未连接'
  byId('message').textContent = '请关闭此窗口后重新运行安装程序'
  byId('progress').hidden = true
} else {
  unsubscribe = api.onProgress(snapshot => { revision++; render(snapshot) })
  const requestedAt = revision
  api.getProgress().then(snapshot => { if (revision === requestedAt) render(snapshot) }).catch(error => {
    if (disposed || revision !== requestedAt) return
    byId('phase').textContent = '无法读取安装进度'
    byId('message').textContent = error instanceof Error && error.message ? error.message : '请关闭此窗口后重新运行安装程序'
    byId('progress').hidden = true
  })
}
