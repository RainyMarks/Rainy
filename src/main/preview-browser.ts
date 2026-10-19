/**
 * The agent's preview window: one hidden Chromium window it opens its own running app in, to see what it built.
 *
 * Only loopback addresses are opened — the development server of the project at hand — so the window never reaches a
 * site the user is signed in to. It has its own in-memory session, so it shares no cookies or storage with anything.
 */
import { BrowserWindow, session } from 'electron'
import { localized } from '../shared/locale.ts'

/** The window renders at this size; a page that reacts to width sees a desktop. */
const WIDTH = 1280
const HEIGHT = 800
/** Longest a navigation may take before the command answers with what it has. */
const LOAD_TIMEOUT_MS = 20_000
/** Console messages and failed requests kept per page. */
const EVENTS_KEPT = 40
/** Characters of page text one read returns. */
const TEXT_LIMIT = 8000

/** One command from the Host. */
export interface PreviewCommand {
  action: 'open' | 'read' | 'click' | 'type' | 'press' | 'screenshot' | 'close'
  url?: string | undefined
  /** Visible text or CSS selector of the element to act on. */
  target?: string | undefined
  text?: string | undefined
  key?: string | undefined
  /** Wait this long after acting before reading the page again. */
  settleMs?: number | undefined
}

/** What the Host gets back. */
export interface PreviewAnswer {
  url?: string | undefined
  title?: string | undefined
  text?: string | undefined
  /** Interactive elements, as `role "name"` lines. */
  elements?: string[] | undefined
  /** Console errors and warnings, newest last. */
  console?: string[] | undefined
  /** Requests that failed, newest last. */
  failed?: string[] | undefined
  /** PNG, base64, for a screenshot. */
  image?: string | undefined
  /** Set when the command could not be carried out. */
  error?: string | undefined
}

/** @param url Address from the model. @returns Whether it is a loopback http(s) address. */
export function isLoopbackUrl(url: string): boolean {
  let parsed: URL
  try { parsed = new URL(url) } catch { return false }
  if (parsed.protocol !== 'http:' && parsed.protocol !== 'https:') return false
  const host = parsed.hostname.toLowerCase().replace(/^\[|\]$/gu, '')
  return host === 'localhost' || host === '127.0.0.1' || host === '::1' || host.endsWith('.localhost')
}

/** Reads the page the way a text-only model can act on: its text and the elements it can click or type into. */
const READ_SCRIPT = `(() => {
  const visible = (element) => {
    const rect = element.getBoundingClientRect()
    if (rect.width < 1 || rect.height < 1) return false
    const style = getComputedStyle(element)
    return style.visibility !== 'hidden' && style.display !== 'none' && Number(style.opacity) !== 0
  }
  const label = (element) => (
    element.getAttribute('aria-label') || element.title || element.placeholder || element.value
    || (element.innerText || element.textContent || '').trim()
  ).replace(/\\s+/g, ' ').slice(0, 80)
  const role = (element) => {
    const explicit = element.getAttribute('role')
    if (explicit) return explicit
    const tag = element.tagName.toLowerCase()
    if (tag === 'a') return 'link'
    if (tag === 'button') return 'button'
    if (tag === 'select') return 'select'
    if (tag === 'textarea') return 'textbox'
    if (tag === 'input') return (element.type === 'checkbox' || element.type === 'radio' || element.type === 'submit' || element.type === 'button') ? element.type : 'textbox'
    return 'control'
  }
  const nodes = [...document.querySelectorAll('a[href], button, input, select, textarea, [role="button"], [role="link"], [role="tab"], [role="checkbox"], [onclick]')]
  const elements = []
  for (const node of nodes) {
    if (!visible(node)) continue
    const name = label(node)
    const line = role(node) + (name ? ' "' + name + '"' : '') + (node.disabled ? ' [disabled]' : '')
    if (!elements.includes(line)) elements.push(line)
    if (elements.length >= 60) break
  }
  return { url: location.href, title: document.title, text: (document.body ? document.body.innerText : '').replace(/\\n{3,}/g, '\\n\\n').trim().slice(0, ${TEXT_LIMIT}), elements }
})()`

/** Finds an element by visible text, falling back to a CSS selector, and acts on it in the page. */
function actScript(action: 'click' | 'type' | 'press', target: string, value: string): string {
  const wanted = JSON.stringify(target)
  const text = JSON.stringify(value)
  return `(() => {
    const wanted = ${wanted}
    const lower = wanted.toLowerCase()
    const named = (element) => ((element.getAttribute('aria-label') || element.title || element.placeholder || element.name || element.innerText || element.textContent || '').trim().toLowerCase())
    const all = [...document.querySelectorAll('a[href], button, input, select, textarea, [role="button"], [role="link"], [role="tab"], [role="checkbox"], [onclick], label')]
    let found = all.find(element => named(element) === lower) ?? all.find(element => named(element).includes(lower))
    if (!found) { try { found = document.querySelector(wanted) ?? undefined } catch (_invalidSelector) { found = undefined } }
    if (!found) return { error: 'no element matches ' + wanted }
    if (found.tagName === 'LABEL' && found.control) found = found.control
    found.scrollIntoView({ block: 'center' })
    if (${JSON.stringify(action)} === 'click') { found.focus?.(); found.click() }
    else if (${JSON.stringify(action)} === 'type') {
      found.focus?.()
      const setter = Object.getOwnPropertyDescriptor(found.constructor.prototype, 'value')?.set
      if (setter) setter.call(found, ${text}); else found.value = ${text}
      found.dispatchEvent(new Event('input', { bubbles: true }))
      found.dispatchEvent(new Event('change', { bubbles: true }))
    } else {
      found.focus?.()
      for (const type of ['keydown', 'keypress', 'keyup']) {
        found.dispatchEvent(new KeyboardEvent(type, { key: ${text}, bubbles: true, cancelable: true }))
      }
      if (${text} === 'Enter' && found.form) found.form.requestSubmit?.()
    }
    return { ok: true }
  })()`
}

/** The hidden window and what its page reported. */
export class PreviewBrowser {
  private window: BrowserWindow | undefined
  private readonly console: string[] = []
  private readonly failed: string[] = []

  /** Close the window; a later command opens a new one. */
  close(): void {
    const window = this.window
    this.window = undefined
    this.console.length = 0
    this.failed.length = 0
    if (window !== undefined && !window.isDestroyed()) window.destroy()
  }

  /**
   * Carry out one command from the Host.
   * @param command The command.
   * @returns What the page reports, or an error for the model.
   */
  async handle(command: PreviewCommand): Promise<PreviewAnswer> {
    try {
      if (command.action === 'close') { this.close(); return {} }
      if (command.action === 'open') {
        const url = command.url ?? ''
        if (!isLoopbackUrl(url)) {
          return { error: localized({
            zh: '预览只能打开本机地址（localhost 或 127.0.0.1）上的项目自己的服务，其他网址请用 web_fetch。',
            en: 'The preview only opens the project\'s own server on this machine (localhost or 127.0.0.1); use web_fetch for other addresses.',
          }) }
        }
        await this.open(url)
        return await this.report(command.settleMs ?? 400)
      }
      const contents = this.window?.webContents
      if (contents === undefined || this.window?.isDestroyed() === true) {
        return { error: localized({ zh: '预览还没有打开任何页面，请先用 browser_open 打开。', en: 'The preview has no page open yet; open one with browser_open first.' }) }
      }
      if (command.action === 'read') return await this.report(0)
      if (command.action === 'screenshot') {
        const image = await contents.capturePage()
        return { ...await this.report(0), image: image.toPNG().toString('base64') }
      }
      const value = command.action === 'press' ? command.key ?? 'Enter' : command.text ?? ''
      const result = await contents.executeJavaScript(actScript(command.action, command.target ?? '', value), true) as { error?: string }
      if (result.error !== undefined) return { ...await this.report(0), error: result.error }
      return await this.report(command.settleMs ?? 500)
    } catch (error) {
      return { error: error instanceof Error ? error.message : String(error) }
    }
  }

  private async open(url: string): Promise<void> {
    if (this.window === undefined || this.window.isDestroyed()) {
      const partition = `preview-${Date.now()}`
      const preview = session.fromPartition(partition, { cache: false })
      this.window = new BrowserWindow({
        show: false, width: WIDTH, height: HEIGHT,
        webPreferences: { session: preview, nodeIntegration: false, contextIsolation: true, sandbox: true, webSecurity: true, javascript: true },
      })
      this.window.on('closed', () => { this.window = undefined })
      const contents = this.window.webContents
      contents.setWindowOpenHandler(() => ({ action: 'deny' }))
      contents.on('console-message', (_event, level, message, line, source) => {
        if (level < 2) return
        this.push(this.console, `${level === 3 ? 'error' : 'warn'}: ${message}${source === '' ? '' : ` (${source}:${line})`}`)
      })
      contents.on('did-fail-load', (_event, code, description, failedUrl, isMainFrame) => {
        if (isMainFrame) this.push(this.failed, `${failedUrl}: ${description} (${code})`)
      })
      contents.session.webRequest.onErrorOccurred(({ url: failedUrl, error }) => { this.push(this.failed, `${failedUrl}: ${error}`) })
      // A page that asks for a camera, notifications or anything else is refused rather than prompting the user.
      contents.session.setPermissionRequestHandler((_contents, _permission, callback) => { callback(false) })
    }
    this.console.length = 0
    this.failed.length = 0
    const contents = this.window.webContents
    await Promise.race([
      contents.loadURL(url).catch((error: unknown) => { this.push(this.failed, error instanceof Error ? error.message : String(error)) }),
      new Promise<void>(done => { setTimeout(done, LOAD_TIMEOUT_MS) }),
    ])
  }

  private push(list: string[], line: string): void {
    list.push(line)
    while (list.length > EVENTS_KEPT) list.shift()
  }

  private async report(settleMs: number): Promise<PreviewAnswer> {
    if (settleMs > 0) await new Promise<void>(done => { setTimeout(done, settleMs) })
    const contents = this.window?.webContents
    if (contents === undefined) return { error: 'the preview window closed' }
    const page = await contents.executeJavaScript(READ_SCRIPT, true) as { url: string; title: string; text: string; elements: string[] }
    return {
      url: page.url, title: page.title, text: page.text, elements: page.elements,
      console: [...this.console], failed: [...this.failed],
    }
  }
}
