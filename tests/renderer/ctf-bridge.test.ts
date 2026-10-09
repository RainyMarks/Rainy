// @vitest-environment happy-dom
/** Handshake, context changes and durable-save settlement through real Window messages. */
import { afterEach, describe, expect, it, vi } from 'vitest'
import { CtfWorkbenchBridge } from '../../src/renderer/ctf/ctf-bridge.ts'
import type { CtfColors, CtfConfiguration } from '../../src/renderer/ctf/ctf-protocol.ts'
import { readCtfColors } from '../../src/renderer/ctf/ctf-colors.ts'

const owners: CtfWorkbenchBridge[] = []
afterEach(() => {
  for (const owner of owners.splice(0)) owner.dispose()
  document.body.replaceChildren()
  document.body.removeAttribute('style')
  vi.restoreAllMocks()
  vi.useRealTimers()
})

const first: CtfConfiguration = { context: { kind: 'session', id: 'a' }, visible: true,
  appearance: { dark: true, fontSize: 14, codeFontSize: 13, locale: 'zh' } }

function fixture() {
  const frame = document.createElement('iframe')
  frame.src = '/rainy/icesky/index.html?embed=rainy'
  document.body.append(frame)
  if (frame.contentWindow === null) throw new Error('expected a Window proxy')
  const send = vi.spyOn(frame.contentWindow, 'postMessage').mockImplementation(() => {})
  const toast = vi.fn()
  const bridge = new CtfWorkbenchBridge({ origin: location.origin, readyTimeoutMs: 15000, flushTimeoutMs: 15000,
    toast, flushFailureMessage: () => 'save failed' })
  owners.push(bridge)
  bridge.attach(frame)
  bridge.configure(first)
  const message = (data: unknown, origin = location.origin, source: MessageEventSource | null = frame.contentWindow): void => {
    bridge.receive(new MessageEvent('message', { data, origin, source }))
  }
  const flushId = (): string => {
    const data: unknown = send.mock.lastCall?.[0]
    if (data === null || typeof data !== 'object' || !('id' in data) || typeof data.id !== 'string') throw new Error('expected a flush request')
    return data.id
  }
  return { bridge, frame, send, toast, message, flushId }
}

describe('CTF frame bridge', () => {
  it('sends parsed host semantic colors and omits the palette before tokens are available', () => {
    assertNoPalette()
    const tokens = {
      '--dsw-alias-bg-base': '#111111', '--dsw-alias-bg-layer-1': '#222222',
      '--dsw-alias-interactive-bg-hover': 'rgba(255, 255, 255, 0.08)',
      '--dsw-alias-label-primary': '#eeeeee', '--dsw-alias-label-secondary': '#bbbbbb',
      '--dsw-alias-border-l1': 'rgba(255, 255, 255, 0.06)', '--dsw-alias-button-primary-fill': '#5588ee',
      '--dsw-alias-label-primary-foreground': '#ffffff', '--dsw-alias-state-error-primary': '#ee4444',
      '--dsw-alias-state-warn-primary': '#eebb44', '--dsw-alias-state-success-primary': '#44bb66',
    }
    for (const [token, value] of Object.entries(tokens)) document.body.style.setProperty(token, value)
    const colors = readCtfColors(document)
    expect(normalized(colors)).toEqual({ background: 'rgb(17, 17, 17)', panel: 'rgb(34, 34, 34)', input: 'rgb(34, 34, 34)',
      hover: 'rgba(255, 255, 255, 0.08)', text: 'rgb(238, 238, 238)', secondary: 'rgb(187, 187, 187)',
      border: 'rgba(255, 255, 255, 0.06)', accent: 'rgb(85, 136, 238)', accentText: 'rgb(255, 255, 255)',
      error: 'rgb(238, 68, 68)', warning: 'rgb(238, 187, 68)', success: 'rgb(68, 187, 102)' })
    const h = fixture()
    h.bridge.configure({ ...first, appearance: { ...first.appearance, ...colors === undefined ? {} : { colors } } })
    h.message({ type: 'rainy:ready' })
    expect(h.send).toHaveBeenLastCalledWith(expect.objectContaining({ appearance: { ...first.appearance, colors } }), location.origin)
    expect(document.querySelector('[aria-hidden="true"]')).toBeNull()
  })
  it('waits for the application acknowledgement and ignores forged or superseded messages', () => {
    const h = fixture()
    h.message({ type: 'rainy:ready' }, 'https://unrelated.test')
    h.message({ type: 'rainy:ready' }, location.origin, window)
    expect(h.send).not.toHaveBeenCalled()
    h.message({ type: 'rainy:ready' })
    expect(h.send).toHaveBeenLastCalledWith({ type: 'rainy:configure', revision: 1, ...first }, location.origin)
    expect(h.bridge.state.getSnapshot().phase).toBe('loading')
    h.bridge.configure({ ...first, context: { kind: 'session', id: 'b' } })
    h.message({ type: 'rainy:loaded', revision: 1 })
    expect(h.bridge.state.getSnapshot().phase).toBe('loading')
    h.message({ type: 'rainy:loaded', revision: 2 })
    expect(h.bridge.state.getSnapshot().phase).toBe('ready')
    expect(h.frame.src).toContain('index.html?embed=rainy')
    h.message({ type: 'rainy:toast', message: 'copied', kind: 'success' })
    expect(h.toast).toHaveBeenCalledExactlyOnceWith('copied', 'success')
  })

  it('keeps the document and unsaved data on a refused retry, then retries saving without reloading', async () => {
    const h = fixture()
    h.message({ type: 'rainy:ready' })
    h.message({ type: 'rainy:loaded', revision: 1 })
    const replace = vi.spyOn(h.frame, 'src', 'set')
    h.message({ type: 'rainy:status', state: 'error', message: 'disk full' })
    const retry = h.bridge.retry()
    expect(h.send.mock.lastCall?.[0]).toMatchObject({ type: 'rainy:flush' })
    h.message({ type: 'rainy:flushed', id: h.flushId(), ok: false, error: 'disk full' })
    expect(await retry).toBe(false)
    expect(replace).not.toHaveBeenCalled()
    expect(h.bridge.state.getSnapshot()).toMatchObject({ phase: 'error', message: 'disk full' })
    const success = h.bridge.retry()
    h.message({ type: 'rainy:flushed', id: h.flushId(), ok: true })
    expect(await success).toBe(true)
    expect(replace).not.toHaveBeenCalled()
    expect(h.bridge.state.getSnapshot()).toMatchObject({ phase: 'ready', saving: 'saved' })
  })

  it('reports a root startup error without replacing the document or overwriting it with a timeout', () => {
    vi.useFakeTimers()
    const h = fixture()
    const replace = vi.spyOn(h.frame, 'src', 'set')
    h.message({ type: 'rainy:ready' })
    h.message({ type: 'rainy:error', message: 'Draft could not be read' }, 'https://unrelated.test')
    h.message({ type: 'rainy:error', message: 7 })
    expect(h.bridge.state.getSnapshot().phase).toBe('loading')
    h.message({ type: 'rainy:error', message: 'Draft could not be read' })
    expect(h.bridge.state.getSnapshot()).toMatchObject({ phase: 'error', error: 'load', message: 'Draft could not be read' })
    vi.advanceTimersByTime(15000)
    expect(h.bridge.state.getSnapshot().message).toBe('Draft could not be read')
    expect(replace).not.toHaveBeenCalled()
  })

  it('bounds failed boot and save waits and settles every pending request at disposal', async () => {
    vi.useFakeTimers()
    const h = fixture()
    vi.advanceTimersByTime(15000)
    expect(h.bridge.state.getSnapshot()).toMatchObject({ phase: 'error', error: 'timeout' })
    h.message({ type: 'rainy:ready' })
    const save = h.bridge.flush()
    h.message({ type: 'rainy:flushed', id: h.flushId(), ok: true }, 'https://unrelated.test')
    vi.advanceTimersByTime(15000)
    expect(await save).toEqual({ ok: false, error: 'save failed' })
    const pending = h.bridge.flush()
    h.bridge.dispose()
    expect(await pending).toEqual({ ok: false, error: 'save failed' })
    expect(vi.getTimerCount()).toBe(0)
    h.message({ type: 'rainy:toast', message: 'late', kind: 'success' })
    expect(h.toast).not.toHaveBeenCalled()
  })
})

function assertNoPalette(): void {
  expect(readCtfColors(document)).toBeUndefined()
  expect(document.querySelector('[aria-hidden="true"]')).toBeNull()
}

/** Browsers report parsed colors as `rgb()`; the test DOM may keep the hex spelling. */
function normalized(colors: CtfColors | undefined): Record<string, string> | undefined {
  if (colors === undefined) return undefined
  const rgb = (value: string): string => {
    const hex = /^#([0-9a-f]{2})([0-9a-f]{2})([0-9a-f]{2})$/iu.exec(value)
    return hex === null ? value : `rgb(${hex.slice(1).map(part => parseInt(part, 16)).join(', ')})`
  }
  return Object.fromEntries(Object.entries(colors).map(([key, value]: [string, string]) => [key, rgb(value)]))
}
