// @vitest-environment happy-dom
/** Editor tabs pick a viewer by file type, switch Markdown between source, preview and split, and page binary files in hex. */
import { act } from 'react'
import { afterEach, describe, expect, it, vi } from 'vitest'
import type { IdeFileDocument, IdeFileVersion } from '../../src/shared/ide-files-protocol.ts'
import { byRole, cleanup, click, keyDown, waitFor } from './ide-dom.tsx'
import { resetChatState } from './app-chat-mock.tsx'
import { disposeWindows, mountWindow, workspace } from './app-fixture.tsx'

vi.mock('../../src/renderer/rpc.ts', () => import('./ide-host-mock.ts'))
vi.mock('../../src/renderer/chat/ChatPane.tsx', () => import('./app-chat-mock.tsx'))
vi.mock('../../src/renderer/chat/History.tsx', () => import('./app-chat-mock.tsx'))
vi.mock('../../src/renderer/settings/SettingsDialog.tsx', () => import('./app-chat-mock.tsx'))
vi.mock('../../src/renderer/ide/editor-loader.ts', () => ({ loadEditorAssets: async () => (await import('./app-fixture.tsx')).assets }))

afterEach(async () => {
  cleanup()
  await disposeWindows()
  resetChatState()
  vi.unstubAllGlobals()
  vi.clearAllMocks()
})

function file(path: string, change: Partial<IdeFileDocument>): IdeFileDocument {
  return { workspaceId: workspace.workspaceId, path, version: 'v2' as IdeFileVersion, bytes: 4, content: null, bom: false, eol: 'lf', readOnlyReason: null, ...change }
}

async function open(path: string, document: IdeFileDocument) {
  const h = await mountWindow()
  h.request.mockImplementation(async (request) => {
    if (request.op === 'files.read') return document as never
    if (request.op === 'state.save') return { version: 1, revision: 1, data: h.workbench.model.state.getSnapshot().data } as never
    throw new Error(`Unexpected request ${request.op}`)
  })
  await act(async () => { await h.workbench.model.openFile(path) })
  return h
}

const editorHidden = (): boolean => window.document.querySelector<HTMLElement>('[data-rainy-editor]')?.hidden !== false

describe('file viewers', () => {
  it('switches a Markdown tab between source, preview and split, also with Ctrl+Shift+V', async () => {
    const h = await open('notes/README.md', file('notes/README.md', { content: '# 题解\n\n![flag](flag.png)\n', bytes: 24 }))
    expect(byRole('button', '源码').getAttribute('aria-pressed')).toBe('true')
    expect(editorHidden()).toBe(false)
    click(byRole('button', '预览'))
    // The Markdown renderer loads on first use, which can take seconds while the whole suite runs in parallel.
    await waitFor(() => { expect(window.document.querySelector('[data-viewer="markdown"] h1')?.textContent).toBe('题解') }, 10000)
    expect(editorHidden()).toBe(true)
    expect(window.document.querySelector('[data-viewer="markdown"] img')?.getAttribute('src')).toBe('/rainy/ide/raw?w=a&p=notes%2Fflag.png&v=v2-0')
    click(byRole('button', '分栏'))
    await waitFor(() => { expect(editorHidden()).toBe(false) })
    expect(window.document.querySelector('[data-viewer="markdown"]')).not.toBeNull()
    expect(h.workbench.model.state.getSnapshot().data.tabs.find(tab => tab.path === 'notes/README.md')?.view).toBe('split')
    keyDown(window, { key: 'V', code: 'KeyV', ctrlKey: true, shiftKey: true })
    await waitFor(() => { expect(editorHidden()).toBe(true) })
    keyDown(window, { key: 'V', code: 'KeyV', ctrlKey: true, shiftKey: true })
    await waitFor(() => { expect(window.document.querySelector('[data-viewer="markdown"]')).toBeNull() })
    expect(h.workbench.model.state.getSnapshot().data.tabs.find(tab => tab.path === 'notes/README.md')?.view).toBeUndefined()
  })

  it('shows images through the byte route with their size in the status bar', async () => {
    await open('logo.png', file('logo.png', { readOnlyReason: 'media', bytes: 2048 }))
    await waitFor(() => { expect(window.document.querySelector('[data-viewer="image"] img')?.getAttribute('src')).toBe('/rainy/ide/raw?w=a&p=logo.png&v=v2') }, 10000)
    expect(editorHidden()).toBe(true)
    expect(window.document.querySelector('[data-rainy-status]')?.textContent).toContain('2.0KB')
  })

  it('pages binary files into the hex view and names their format', async () => {
    const bytes = Uint8Array.from([0x7f, 0x45, 0x4c, 0x46, 2, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 3, 0, 0x3e, 0])
    const fetch = vi.fn(async (_input: RequestInfo | URL, init?: RequestInit) => {
      const range = /bytes=(\d+)-(\d+)/u.exec(new Headers(init?.headers).get('range') ?? '')
      return new Response(bytes.slice(Number(range?.[1] ?? 0), Number(range?.[2] ?? bytes.length - 1) + 1), { status: 206, headers: { etag: '"v2"' } })
    })
    vi.stubGlobal('fetch', fetch)
    await open('chall', file('chall', { readOnlyReason: 'binary', bytes: bytes.length }))
    await waitFor(() => { expect(window.document.querySelector('[data-viewer="hex"]')?.textContent).toContain('ELF 64-bit LSB shared object, x86-64') })
    const cells = [...window.document.querySelectorAll('[data-viewer="hex"] [data-offset]')].slice(0, 4).map(cell => cell.textContent)
    expect(cells).toEqual(['7f', '45', '4c', '46'])
    expect(String(fetch.mock.calls[0]?.[0])).toBe('/rainy/ide/raw?w=a&p=chall&v=v2')
  })
})
