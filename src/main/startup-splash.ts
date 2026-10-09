/** First paint of the main window while the carrier admits resources and starts its Host. */
import type { BrowserWindow } from 'electron'
import { CAPTION_HEIGHT } from './window-chrome.ts'

/** Window background colors shared by the native window and the startup page, so neither flashes. */
export const WINDOW_BACKGROUND = { dark: '#16191e', light: '#f6f7f9' } as const

/**
 * Render the self-contained startup page; it loads no files and runs no remote content.
 * @param dark - native theme at launch.
 * @returns a data URL for the main window's first load.
 */
export function startupPage(dark: boolean): string {
  const background = dark ? WINDOW_BACKGROUND.dark : WINDOW_BACKGROUND.light
  const html = `<!doctype html><html lang="zh"><head><meta charset="utf-8"><title>RainyAgent</title><style>
:root { color-scheme: ${dark ? 'dark' : 'light'}; --fg: ${dark ? '#e8eaed' : '#1d2129'}; --muted: ${dark ? '#9aa0a6' : '#6b7280'};
  --track: ${dark ? 'rgba(255,255,255,.08)' : 'rgba(17,24,39,.08)'}; --accent: #4d6bfe; }
* { box-sizing: border-box; }
html, body { margin: 0; height: 100%; background: ${background}; color: var(--fg); font: 13px/1.5 "Segoe UI Variable Text", "Segoe UI",
  "Microsoft YaHei UI", system-ui, sans-serif; -webkit-user-select: none; user-select: none; cursor: default; }
.drag { position: fixed; inset: 0 0 auto; height: ${CAPTION_HEIGHT}px; -webkit-app-region: drag; }
main { height: 100%; display: grid; place-content: center; justify-items: center; gap: 18px; }
h1 { margin: 0; font-size: 22px; font-weight: 600; letter-spacing: .2px; }
.bar { position: relative; width: 240px; height: 3px; overflow: hidden; border-radius: 3px; background: var(--track); }
.fill { position: absolute; inset: 0 auto 0 0; width: 0; border-radius: 3px; background: var(--accent); transition: width .2s ease; }
.bar[data-indeterminate] .fill { width: 36%; animation: slide 1.15s ease-in-out infinite; }
@keyframes slide { from { transform: translateX(-100%) } to { transform: translateX(280%) } }
p { margin: 0; min-height: 20px; color: var(--muted); text-align: center; font-variant-numeric: tabular-nums; }
@media (prefers-reduced-motion: reduce) { .bar[data-indeterminate] .fill { animation: none; width: 100%; opacity: .35; } }
</style></head><body><div class="drag"></div><main><h1>RainyAgent</h1>
<div class="bar" data-indeterminate><div class="fill"></div></div><p id="step">正在启动…</p></main>
<script>
window.__rainyStartup = (state) => {
  const bar = document.querySelector('.bar'), fill = document.querySelector('.fill')
  document.getElementById('step').textContent = state.text
  if (typeof state.ratio === 'number') { bar.removeAttribute('data-indeterminate'); fill.style.width = Math.round(state.ratio * 100) + '%' }
  else { bar.setAttribute('data-indeterminate', ''); fill.style.width = '' }
}
</script></body></html>`
  return 'data:text/html;charset=utf-8,' + encodeURIComponent(html)
}

/** Progress reporting into the startup page; updates after the workbench has loaded are ignored. */
export interface StartupProgress {
  /** Show one startup step. @param text - user-facing description. @param ratio - determinate progress from 0 to 1. */
  step(text: string, ratio?: number): void
}

/**
 * Bind progress updates to the startup page of one window.
 * @param window - main window while it still shows the startup page.
 * @returns a reporter that coalesces rapid updates into at most one page update per animation frame interval.
 */
export function startupProgress(window: BrowserWindow): StartupProgress {
  let pending: { text: string; ratio?: number } | undefined
  let timer: ReturnType<typeof setTimeout> | undefined
  const flush = (): void => {
    timer = undefined
    const state = pending
    pending = undefined
    if (state === undefined || window.isDestroyed() || !window.webContents.getURL().startsWith('data:')) return
    window.webContents.executeJavaScript(`window.__rainyStartup?.(${JSON.stringify(state)})`)
      .catch((_navigated: unknown) => { /* The workbench replaced the startup page. */ })
  }
  return {
    step(text, ratio) {
      pending = ratio === undefined ? { text } : { text, ratio }
      timer ??= setTimeout(flush, 16)
    },
  }
}
