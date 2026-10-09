/** Place native Windows controls alongside the shared application's existing header rows. */
import { ipcRenderer } from 'electron'
import { CAPTION_HEIGHT, CAPTION_FALLBACK_WIDTH } from './window-chrome.ts'

interface ControlsOverlay extends EventTarget {
  visible: boolean
  getTitlebarAreaRect(): DOMRect
}

/** Reuse DSH's drag markers, theme tokens and settings panel without adding a caption row. */
export function installWindowChrome(): void {
  const mount = () => {
    const root = document.documentElement
    root.dataset.rainyFrame = ''
    const style = document.createElement('style')
    style.textContent = `
      html[data-rainy-frame] { --rainy-caption-width: ${CAPTION_FALLBACK_WIDTH}px; }
      html[data-rainy-frame] [data-window-drag] { -webkit-app-region: drag; }
      html[data-rainy-frame] [data-rainy-caption-edge] {
        padding-right: calc(var(--rainy-caption-width) + var(--rainy-row-end, 6px)) !important;
      }
      html[data-rainy-frame] header[data-window-drag] { min-height: ${CAPTION_HEIGHT}px; }
      html[data-rainy-frame] [data-conversation-header-corner],
      html[data-rainy-frame] [data-conversation-header-leading],
      html[data-rainy-frame] body > :not(#root):not([data-rainy-topbar]),
      html[data-rainy-frame] :is(button,a,input,select,textarea,summary,[contenteditable='true'],[tabindex],
        [role='dialog'],[role='alertdialog'],[role='menu'],[role='listbox'],[role='tooltip'],[role='button'],
        [role='link'],[role='tab'],[role='menuitem'],[role='option'],[role='checkbox'],[role='radio'],[role='switch']) {
        -webkit-app-region: no-drag;
      }
      html[data-rainy-frame] [data-rainy-topbar] { -webkit-app-region: drag; }
    `
    document.head.append(style)
    const showMenu = (event: Event) => {
      if (!(event instanceof CustomEvent)) return
      const data: unknown = event.detail
      if (data !== null && typeof data === 'object' && 'menu' in data && 'locale' in data
        && (data.menu === 'edit' || data.menu === 'help') && (data.locale === 'zh' || data.locale === 'en')) {
        ipcRenderer.send('rainy:native-menu', { menu: data.menu, locale: data.locale })
      }
    }
    window.addEventListener('rainy:native-menu', showMenu)
    const overlay = (navigator as Navigator & { windowControlsOverlay?: ControlsOverlay }).windowControlsOverlay
    const rows = new Set<HTMLElement>()
    let frame = 0
    let disposed = false
    let palette = ''
    const probe = document.createElement('span')
    probe.style.cssText = 'position:fixed;visibility:hidden;pointer-events:none;background:var(--dsw-alias-bg-base);color:var(--dsw-alias-label-primary)'
    document.body.append(probe)
    const canvas = document.createElement('canvas')
    canvas.width = canvas.height = 1
    const context = canvas.getContext('2d', { willReadFrequently: true })
    const rgb = (color: string) => {
      if (!context) return undefined
      context.clearRect(0, 0, 1, 1)
      context.fillStyle = color
      context.fillRect(0, 0, 1, 1)
      return '#' + Array.from(context.getImageData(0, 0, 1, 1).data.slice(0, 3), byte => byte.toString(16).padStart(2, '0')).join('')
    }
    const update = () => {
      frame = 0
      if (disposed) return
      const area = overlay?.visible ? overlay.getTitlebarAreaRect() : undefined
      const width = area ? Math.max(0, innerWidth - area.right) : CAPTION_FALLBACK_WIDTH
      const height = area?.height ?? CAPTION_HEIGHT
      const value = `${width}px`
      if (root.style.getPropertyValue('--rainy-caption-width') !== value) root.style.setProperty('--rainy-caption-width', value)
      for (const row of rows) {
        if (!row.isConnected) { resize.unobserve(row); rows.delete(row); continue }
        const rect = row.getBoundingClientRect()
        const frameRight = document.querySelector('[data-app-frame]')?.getBoundingClientRect().right ?? innerWidth
        const edge = rect.top < height && rect.bottom > 0 && rect.right >= frameRight - 2 && rect.width > width
        if (edge && !row.hasAttribute('data-rainy-caption-edge')) {
          const padding = getComputedStyle(row).paddingRight
          row.style.setProperty('--rainy-row-end', padding)
          row.setAttribute('data-rainy-caption-edge', '')
        } else if (!edge && row.hasAttribute('data-rainy-caption-edge')) row.removeAttribute('data-rainy-caption-edge')
      }
      const computed = getComputedStyle(probe)
      const color = rgb(computed.backgroundColor)
      const symbolColor = rgb(computed.color)
      const theme = root.getAttribute('data-ds-theme-source')
      const locale = root.lang.toLowerCase().startsWith('zh') ? 'zh' : 'en'
      const next = JSON.stringify({ color, symbolColor, theme, locale })
      if (color && symbolColor && next !== palette) {
        palette = next
        ipcRenderer.send('rainy:caption-colors', { color, symbolColor, theme, locale })
      }
    }
    const schedule = () => { if (!frame && !disposed) frame = requestAnimationFrame(update) }
    const resize = new ResizeObserver(schedule)
    const collect = () => {
      for (const row of document.querySelectorAll<HTMLElement>('[data-window-drag]')) {
        if (!rows.has(row)) { rows.add(row); resize.observe(row) }
      }
      schedule()
    }
    const changes = new MutationObserver(collect)
    changes.observe(document.body, { childList: true, subtree: true, attributes: true, attributeFilter: ['data-ds-dark-theme', 'data-rightbar-collapsed', 'data-sidebar-right-panel'] })
    changes.observe(root, { attributes: true, attributeFilter: ['data-ds-theme-source', 'lang'] })
    changes.observe(document.head, { childList: true, subtree: true, characterData: true })
    resize.observe(document.body)
    overlay?.addEventListener('geometrychange', schedule)
    window.addEventListener('resize', schedule)
    collect()
    window.addEventListener('pagehide', () => {
      disposed = true
      cancelAnimationFrame(frame)
      resize.disconnect(); changes.disconnect()
      overlay?.removeEventListener('geometrychange', schedule)
      window.removeEventListener('resize', schedule)
      window.removeEventListener('rainy:native-menu', showMenu)
      rows.clear(); probe.remove(); style.remove()
    }, { once: true })
  }
  if (document.readyState === 'loading') window.addEventListener('DOMContentLoaded', mount, { once: true })
  else mount()
}
