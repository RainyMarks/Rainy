/** Local development setup controls; installation is sent only by the explicit install button. */
const api = window.__RAINY_IDE_ENVIRONMENT__
const byId = id => document.getElementById(id)
const labels = { checking: '正在检查', ready: '开发工具已就绪', 'needs-install': '需要准备开发工具', unsupported: '需要手动准备', 'missing-media': '安装文件不可用', installing: '正在安装', error: '准备未完成' }
let working = false
let revision = 0
let disposed = false
let unsubscribe = () => {}

function render(snapshot) {
  if (disposed) return
  working = snapshot.busy
  byId('state').textContent = labels[snapshot.status] || '开发工具准备'
  byId('distribution').textContent = snapshot.distro
  byId('message').textContent = snapshot.message
  byId('error-code').hidden = !snapshot.code
  byId('error-code').textContent = snapshot.code || ''
  byId('install').hidden = snapshot.status !== 'needs-install'
  byId('tools').replaceChildren(...snapshot.tools.map(tool => {
    const row = document.createElement('li')
    const name = document.createElement('span')
    name.className = 'tool-name'
    name.textContent = tool.name
    const status = document.createElement('span')
    status.className = tool.ready ? 'tool-status' : 'tool-status missing'
    status.textContent = tool.ready ? '已就绪' : '缺失'
    row.append(name, status)
    return row
  }))
  byId('incomplete').hidden = snapshot.incompletePackages.length === 0
  byId('incomplete').textContent = snapshot.incompletePackages.length ? `尚有 ${snapshot.incompletePackages.length} 个组件未完成安装或配置。` : ''
  byId('diagnostics').hidden = snapshot.log.length === 0
  byId('log').textContent = snapshot.log.join('\n')
  for (const button of document.querySelectorAll('button')) button.disabled = working
}

async function act(type) {
  if (working || disposed || !api) return
  working = true
  for (const button of document.querySelectorAll('button')) button.disabled = true
  try { render(await api.act({ type })) }
  catch (error) {
    if (disposed) return
    working = false
    byId('message').textContent = error instanceof Error ? error.message : '开发工具准备失败，请重新检查。'
    for (const button of document.querySelectorAll('button')) button.disabled = false
  }
}

byId('install').addEventListener('click', () => { void act('install') })
byId('retry').addEventListener('click', () => { void act('retry') })
byId('close').addEventListener('click', () => {
  if (!working && api) void api.close().catch(error => { if (!disposed) byId('message').textContent = error.message })
})

function disconnect() {
  if (disposed) return
  disposed = true
  unsubscribe()
}
window.addEventListener('pagehide', disconnect, { once: true })
window.addEventListener('unload', disconnect, { once: true })
if (api) {
  unsubscribe = api.onProgress(snapshot => { revision++; render(snapshot) })
  const requestedAt = revision
  api.inspect().then(snapshot => { if (revision === requestedAt) render(snapshot) }).catch(error => {
    if (!disposed) byId('message').textContent = error.message
  })
} else {
  byId('message').textContent = '开发工具准备服务未连接，请关闭窗口后从工作区重新打开。'
  for (const button of document.querySelectorAll('button')) button.disabled = true
}
