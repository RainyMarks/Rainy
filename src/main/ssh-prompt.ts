/** Rainy's own windows for the questions `ssh` asks: trusting a new host key, and typing a password or passphrase. */
import { BrowserWindow, dialog, nativeTheme } from 'electron'
import { currentLocale, localized } from '../shared/locale.ts'
import type { AskpassQuestion } from './ssh-askpass.ts'
import { WINDOW_BACKGROUND } from './startup-splash.ts'

function escapeHtml(text: string): string {
  return text.replace(/[&<>"']/gu, char => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', '\'': '&#39;' })[char]!)
}

/**
 * Self-contained page with one password field; `window.rainyAnswer` settles with the typed text or `null`.
 * @param prompt - ssh's question.
 * @param dark - theme.
 * @returns a data URL.
 */
export function secretPage(prompt: string, dark: boolean): string {
  const html = `<!doctype html><html lang="${currentLocale()}"><head><meta charset="utf-8"><title>Rainy</title>
<meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'unsafe-inline'; script-src 'unsafe-inline'"><style>
:root { color-scheme: ${dark ? 'dark' : 'light'}; }
* { box-sizing: border-box; }
html, body { margin: 0; height: 100%; background: ${dark ? WINDOW_BACKGROUND.dark : WINDOW_BACKGROUND.light}; color: ${dark ? '#ebebeb' : '#171717'};
  font: 13px/1.5 "Segoe UI Variable Text", "Segoe UI", "Microsoft YaHei UI", system-ui, sans-serif; }
form { height: 100%; display: flex; flex-direction: column; gap: 10px; padding: 18px 20px; }
h1 { margin: 0; font-size: 14px; font-weight: 600; }
p { margin: 0; color: ${dark ? '#a3a3a3' : '#5c5c5c'}; overflow-wrap: anywhere; white-space: pre-wrap; max-height: 60px; overflow: auto; }
input { width: 100%; height: 32px; padding: 0 10px; border-radius: 4px; border: 1px solid ${dark ? '#3d3d3d' : '#cfcfcf'};
  background: ${dark ? '#0f0f0f' : '#ffffff'}; color: inherit; font: inherit; outline: none; }
input:focus { border-color: ${dark ? '#8c8c8c' : '#525252'}; }
.actions { margin-top: auto; display: flex; justify-content: flex-end; gap: 8px; }
button { min-width: 76px; height: 30px; border-radius: 4px; font: inherit; cursor: pointer; border: 1px solid ${dark ? '#3d3d3d' : '#cfcfcf'};
  background: transparent; color: inherit; }
button[type=submit] { background: ${dark ? '#ebebeb' : '#171717'}; color: ${dark ? '#171717' : '#ffffff'}; border-color: transparent; }
</style></head><body><form id="form" autocomplete="off">
<h1>${escapeHtml(localized({ zh: 'SSH 需要验证', en: 'SSH needs to verify you' }))}</h1>
<p>${escapeHtml(prompt.trim())}</p>
<input id="secret" type="password" spellcheck="false" autofocus aria-label="${escapeHtml(prompt.trim())}">
<div class="actions"><button type="button" id="cancel">${escapeHtml(localized({ zh: '取消', en: 'Cancel' }))}</button>
<button type="submit">${escapeHtml(localized({ zh: '继续', en: 'Continue' }))}</button></div></form>
<script>
window.rainyAnswer = new Promise((resolve) => {
  const input = document.getElementById('secret')
  document.getElementById('form').addEventListener('submit', (event) => { event.preventDefault(); resolve(input.value) })
  document.getElementById('cancel').addEventListener('click', () => { resolve(null) })
  document.addEventListener('keydown', (event) => { if (event.key === 'Escape') resolve(null) })
  input.focus()
})
</script></body></html>`
  return `data:text/html;charset=utf-8,${encodeURIComponent(html)}`
}

/**
 * Ask for a password, passphrase or code in a small modal window.
 * @param parent - owning window, if one is open.
 * @param prompt - ssh's question.
 * @returns the typed text, or `null` when the user cancelled or closed the window.
 */
export async function askSecret(parent: BrowserWindow | undefined, prompt: string): Promise<string | null> {
  const dark = nativeTheme.shouldUseDarkColors
  const window = new BrowserWindow({ ...(parent && !parent.isDestroyed() ? { parent, modal: true } : {}), width: 460, height: 230,
    resizable: false, minimizable: false, maximizable: false, fullscreenable: false, show: false, title: 'Rainy',
    backgroundColor: dark ? WINDOW_BACKGROUND.dark : WINDOW_BACKGROUND.light,
    webPreferences: { sandbox: true, contextIsolation: true, nodeIntegration: false, spellcheck: false } })
  window.setMenuBarVisibility(false)
  window.webContents.setWindowOpenHandler(() => ({ action: 'deny' }))
  window.webContents.on('will-navigate', (event) => { event.preventDefault() })
  const closed = new Promise<null>((resolve) => { window.once('closed', () => { resolve(null) }) })
  try {
    await window.loadURL(secretPage(prompt, dark))
    window.show()
    const answered = (window.webContents.executeJavaScript('window.rainyAnswer') as Promise<unknown>)
      .then(value => typeof value === 'string' ? value : null, () => null)
    return await Promise.race([answered, closed])
  } finally {
    if (!window.isDestroyed()) window.destroy()
  }
}

/**
 * Ask whether to trust a server seen for the first time, showing the fingerprint ssh reported.
 * @param parent - owning window, if one is open.
 * @param prompt - ssh's question, which names the host and its key fingerprint.
 * @returns `yes` to trust it, or `null`.
 */
export async function askHostKey(parent: BrowserWindow | undefined, prompt: string): Promise<string | null> {
  const details = prompt.split('\n').filter(line => !/continue connecting/u.test(line)).join('\n').trim()
  const options = { type: 'warning' as const, title: 'Rainy', noLink: true, defaultId: 1, cancelId: 1,
    message: localized({ zh: '第一次连接这台服务器', en: 'Rainy has not connected to this server before' }),
    detail: localized({ zh: `${details}\n\n请向服务器管理员核对上面的指纹。信任后会记入 ~/.ssh/known_hosts，以后密钥变化时 SSH 会拒绝连接。`,
      en: `${details}\n\nCheck the fingerprint above with the server's administrator. Once trusted it is recorded in ~/.ssh/known_hosts, and SSH refuses to connect if the key ever changes.` }),
    buttons: [localized({ zh: '信任并连接', en: 'Trust and connect' }), localized({ zh: '取消', en: 'Cancel' })] }
  const answer = parent && !parent.isDestroyed() ? await dialog.showMessageBox(parent, options) : await dialog.showMessageBox(options)
  return answer.response === 0 ? 'yes' : null
}

/**
 * @param parent - returns the window that owns the questions.
 * @returns the askpass question handler.
 */
export function sshQuestions(parent: () => BrowserWindow | undefined): (question: AskpassQuestion) => Promise<string | null> {
  return question => question.kind === 'host-key' ? askHostKey(parent(), question.prompt) : askSecret(parent(), question.prompt)
}
