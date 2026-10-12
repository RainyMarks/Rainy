/** Local setup page. The preload owns all privileged work and validates each request. */
const api = window.__RAINY_ENVIRONMENT__
const { say } = window
const byId = id => document.getElementById(id)
const labels = {
  ready: say('环境已就绪', 'Environment ready'), 'needs-system': say('需要安装系统组件', 'System components needed'),
  'needs-distro': say('需要创建运行环境', 'Environment needed'), 'saved-distro-missing': say('原环境不可用', 'Previous environment unavailable'),
  'reboot-required': say('需要重启 Windows', 'Restart Windows'), blocked: say('需要手动处理', 'Action needed'),
  error: say('准备未完成', 'Setup incomplete'), working: say('正在准备', 'Preparing'),
}
let working = false

function render(snapshot) {
  working = snapshot.busy
  byId('state').textContent = labels[snapshot.status] || say('环境准备', 'Environment setup')
  byId('message').textContent = snapshot.message
  byId('location').textContent = `${snapshot.installRoot}\\runtime\\wsl`
  byId('error-code').hidden = !snapshot.code
  byId('error-code').textContent = snapshot.code || ''
  byId('install').hidden = snapshot.status !== 'needs-system'
  byId('create').hidden = snapshot.canResume || !['needs-distro', 'saved-distro-missing', 'error'].includes(snapshot.status)
  byId('create').textContent = snapshot.savedDistro ? say('创建新环境（不含原数据）', 'Create a new environment (without the old data)') : say('创建专用环境', 'Create a dedicated environment')
  byId('resume').hidden = !snapshot.canResume || !['needs-distro', 'saved-distro-missing', 'error'].includes(snapshot.status)
  const choices = snapshot.distributions.filter(distro => distro.version === 2)
  byId('existing').hidden = !choices.length || ['working', 'ready', 'reboot-required', 'blocked'].includes(snapshot.status)
  const selected = byId('distros').value
  byId('distros').replaceChildren(...choices.map(distro => {
    const option = document.createElement('option')
    option.value = distro.name
    option.textContent = distro.name
    if (distro.name === selected) option.selected = true
    return option
  }))
  for (const control of document.querySelectorAll('button,select')) control.disabled = working
}

async function act(action) {
  if (working) return
  working = true
  for (const control of document.querySelectorAll('button,select')) control.disabled = true
  try { render(await api.act(action)) } catch (error) {
    byId('message').textContent = error instanceof Error ? error.message : say('环境准备失败，请重试。', 'Environment setup failed. Try again.')
    working = false
    for (const control of document.querySelectorAll('button,select')) control.disabled = false
  }
}

byId('install').addEventListener('click', () => act({ type: 'install-system-components' }))
byId('create').addEventListener('click', () => act({ type: 'create-managed-distro' }))
byId('resume').addEventListener('click', () => act({ type: 'resume' }))
byId('retry').addEventListener('click', () => act({ type: 'retry' }))
byId('select').addEventListener('click', () => act({ type: 'select-existing', distroName: byId('distros').value }))
if (api) {
  const unsubscribe = api.onProgress(render)
  window.addEventListener('unload', () => unsubscribe(), { once: true })
  api.inspect().then(render).catch(error => { byId('message').textContent = error.message })
} else {
  byId('message').textContent = say('环境准备服务未加载，请关闭并重新打开 RainyAgent。', 'The setup service did not load. Close and reopen RainyAgent.')
  for (const control of document.querySelectorAll('button,select')) control.disabled = true
}
