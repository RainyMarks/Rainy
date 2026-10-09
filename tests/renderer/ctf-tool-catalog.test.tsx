// @vitest-environment happy-dom
/** Tool discovery, launch availability and the retained IceSky frame. */
import { afterEach, describe, expect, it, vi } from 'vitest'
import { ToolCatalog } from '../../src/renderer/ctf/ToolCatalog.tsx'
import { CtfWorkbench } from '../../src/renderer/ctf/CtfWorkbench.tsx'
import { createCtfWorkbench, type CtfWorkbench as CtfWorkbenchOwner } from '../../src/renderer/ctf/ctf-workbench.ts'
import type { NativeToolsState } from '../../src/renderer/ctf/native-tools.ts'
import { allByRole, allByText, byRole, change, cleanup, click, queryByRole, render } from './ide-dom.tsx'

vi.mock('../../src/renderer/rpc.ts', () => import('./ide-host-mock.ts'))

const owners: CtfWorkbenchOwner[] = []
afterEach(() => {
  cleanup()
  for (const owner of owners.splice(0)) owner.dispose()
})

const current = { outdated: false, downloadBytes: 0 }
const state: NativeToolsState = { phase: 'ready', error: '', pending: [], savingFavorites: false, catalogOutdated: false, updateBytes: 0,
  update: { phase: 'unchecked', version: '', error: '' },
  download: { phase: 'idle', completedBytes: 0, totalBytes: 0, error: '' }, tools: [
    { ...current, id: 'cyberchef', name: 'CyberChef', category: 'web', version: '10', launchKind: 'web', status: 'ready', verified: false, missing: [] },
    { ...current, id: 'x64dbg', name: 'x64dbg', category: 'reverse', version: '2026', launchKind: 'desktop', status: 'ready', verified: true,
      missing: [], variants: [{ id: 'x32', name: 'x32dbg', status: 'ready' }] },
    { ...current, id: '7zip', name: '7-Zip', category: 'misc', version: '25', launchKind: 'desktop', status: 'missing', verified: false,
      missing: ['7zFM.exe'] },
  ], preferences: { favorites: ['cyberchef'], recent: ['x64dbg', 'cyberchef'] } }
const pending: NativeToolsState['tools'][number] = { id: 'jq', name: 'jq', category: 'misc', version: '1.8', launchKind: 'terminal',
  status: 'available', verified: false, missing: [], outdated: false, downloadBytes: 1024 ** 2 }

function fixture(next = state) {
  const actions = { loadTools: vi.fn(async () => {}), launchTool: vi.fn(async () => {}), toggleFavorite: vi.fn(async () => {}),
    operateTools: vi.fn(async () => {}), cancelDownload: vi.fn(async () => {}), checkToolUpdates: vi.fn(async () => {}) }
  const view = render(<ToolCatalog state={next} {...actions} />)
  return { ...actions, ...view, actions }
}

function card(name: string): HTMLElement {
  const item = byRole('heading', name).closest('li')
  if (!(item instanceof HTMLElement)) throw new Error(`No card for ${name}`)
  return item
}

describe('common tool catalog', () => {
  it('updates only outdated downloaded tools from a checked channel', () => {
    const h = fixture({ ...state, tools: state.tools.map(tool => tool.id === 'x64dbg' ? { ...tool, outdated: true, downloadBytes: 3 * 1024 ** 2 } : tool),
      updateBytes: 3 * 1024 ** 2, update: { phase: 'available', version: '1.0.6', error: '' } })
    expect(allByText('有新工具或更新：1.0.6')).toHaveLength(1)
    expect(allByText('有更新', card('x64dbg'))).toHaveLength(1)
    click(byRole('button', '更新已下载的工具（3.0MB）'))
    expect(h.operateTools).toHaveBeenCalledExactlyOnceWith('update')
    click(byRole('button', '检查工具更新'))
    expect(h.checkToolUpdates).toHaveBeenCalledOnce()
  })

  it('downloads one tool on demand, shows its progress, and hides download actions once nothing is missing', () => {
    const h = fixture({ ...state, tools: [...state.tools, pending] })
    expect(allByText('已下载 3 / 4 款工具，用到哪款就下载哪款')).toHaveLength(1)
    expect(queryByRole('button', '打开 jq', card('jq'))).toBeNull()
    click(byRole('button', '下载 jq', card('jq')))
    expect(h.operateTools).toHaveBeenLastCalledWith('install', ['jq'])
    click(byRole('button', '全部下载（1.0MB）'))
    expect(h.operateTools).toHaveBeenLastCalledWith('install', ['jq'])
    h.rerender(<ToolCatalog state={{ ...state, tools: [...state.tools, pending],
      download: { phase: 'downloading', completedBytes: 5, totalBytes: 10, error: '', operation: 'install', tools: ['jq'] } }} {...h.actions} />)
    expect(allByText('正在下载 50%')).toHaveLength(2)
    expect(byRole('progressbar').getAttribute('value')).toBe('50')
    expect(byRole('button', '下载 jq').hasAttribute('disabled')).toBe(true)
    click(byRole('button', '取消'))
    expect(h.cancelDownload).toHaveBeenCalledOnce()
    h.rerender(<ToolCatalog state={state} {...h.actions} />)
    expect(allByRole('button', /^全部下载/u)).toHaveLength(0)
    click(byRole('button', '已下载'))
    expect(allByRole('listitem')).toHaveLength(3)
  })

  it('removes a downloaded tool only after a second confirming click', () => {
    const h = fixture()
    click(byRole('button', '移除 CyberChef'))
    expect(h.operateTools).not.toHaveBeenCalled()
    click(byRole('button', '确认移除 CyberChef'))
    expect(h.operateTools).toHaveBeenCalledExactlyOnceWith('remove', ['cyberchef'])
  })

  it('groups the complete list by task and offers only groups that contain tools', () => {
    fixture()
    expect(allByRole('heading').filter(heading => heading.tagName === 'H2').map(heading => heading.firstChild?.textContent))
      .toEqual(['逆向调试', '取证与文件', '编码与数据'])
    const reverse = byRole('region', '逆向调试')
    expect(byRole('heading', 'x64dbg', reverse)).toBeTruthy()
    expect(allByText('逆向调试', reverse).filter(element => element.tagName === 'SPAN')).toHaveLength(0)
    expect(queryByRole('button', '音频与信号')).toBeNull()
    change(byRole('textbox'), '7')
    expect(allByRole('heading').filter(heading => heading.tagName === 'H2')).toHaveLength(0)
    expect(allByText('取证与文件', byRole('list', '常用工具')).length).toBeGreaterThan(0)
  })

  it('filters by category, localized use, favorites and recent order', () => {
    const h = fixture()
    click(byRole('button', '逆向调试'))
    expect(allByRole('listitem')).toHaveLength(1)
    expect(byRole('heading', 'x64dbg')).toBeTruthy()
    click(byRole('button', '全部'))
    change(byRole('textbox'), '编码')
    expect(allByRole('listitem')).toHaveLength(1)
    expect(byRole('heading', 'CyberChef')).toBeTruthy()
    change(byRole('textbox'), '')
    click(byRole('button', '收藏'))
    expect(allByRole('listitem')).toHaveLength(1)
    click(byRole('button', '取消收藏 CyberChef'))
    expect(h.toggleFavorite).toHaveBeenCalledExactlyOnceWith('cyberchef')
    click(byRole('button', '最近使用'))
    expect(allByRole('heading').map(row => row.textContent)).toEqual(['x64dbg', 'CyberChef'])
  })

  it('distinguishes unverified from missing and disables overlapping launches', () => {
    const h = fixture()
    expect(allByText('待验证', card('CyberChef'))).toHaveLength(1)
    click(byRole('button', '打开 CyberChef'))
    expect(h.launchTool).toHaveBeenLastCalledWith('cyberchef')
    click(byRole('button', '打开 x32dbg'))
    expect(h.launchTool).toHaveBeenLastCalledWith('x64dbg', 'x32')
    expect(byRole('button', '打开 7-Zip').hasAttribute('disabled')).toBe(true)
    expect(allByText('7zFM.exe')).toHaveLength(1)
    h.rerender(<ToolCatalog state={{ ...state, pending: ['x64dbg'] }} {...h.actions} />)
    expect(byRole('button', '打开 x64dbg').hasAttribute('disabled')).toBe(true)
    expect(byRole('button', '打开 x32dbg').hasAttribute('disabled')).toBe(true)
    expect(byRole('button', '刷新工具状态').hasAttribute('disabled')).toBe(true)
  })

  it('retains rows and offers retry after a directory refresh fails', () => {
    const h = fixture({ ...state, phase: 'error', error: 'Directory unavailable' })
    expect(allByRole('listitem')).toHaveLength(3)
    expect(byRole('alert').textContent).toContain('Directory unavailable')
    click(byRole('button', '重试'))
    expect(h.loadTools).toHaveBeenCalledOnce()
  })

  it('repairs a tool with missing files by downloading its damaged parts again', () => {
    const h = fixture()
    const missing = card('7-Zip')
    expect(allByText('重新下载缺失或损坏的文件', missing)).toHaveLength(1)
    click(byRole('button', '修复', missing))
    expect(h.operateTools).toHaveBeenCalledExactlyOnceWith('repair')
    expect(h.launchTool).not.toHaveBeenCalled()
  })

  it('offers repair when only the alternate executable is missing', () => {
    fixture({ ...state, tools: [{ ...current, id: 'x64dbg', name: 'x64dbg', category: 'reverse', version: '2026',
      launchKind: 'desktop', status: 'ready', verified: false, missing: [],
      variants: [{ id: 'x32', name: 'x32dbg', status: 'missing' }] }] })
    expect(byRole('button', '打开 x64dbg').hasAttribute('disabled')).toBe(false)
    expect(byRole('button', '打开 x32dbg').hasAttribute('disabled')).toBe(true)
    expect(byRole('button', '修复')).toBeTruthy()
  })

  it('offers no native launch controls in a browser without the desktop bridge', () => {
    fixture({ ...state, phase: 'desktop-only' })
    expect(allByText('在 RainyAgent 桌面应用中使用常用工具')).toHaveLength(1)
    expect(allByRole('button')).toHaveLength(0)
  })
})

describe('CTF workbench tabs', () => {
  it('mounts one IceSky frame on first selection and keeps it across catalog visits', () => {
    const owner = createCtfWorkbench({ readyTimeoutMs: 15000, flushTimeoutMs: 15000 })
    owners.push(owner)
    const load = vi.spyOn(owner.tools, 'load')
    const configure = vi.spyOn(owner.bridge, 'configure')
    const view = render(<CtfWorkbench owner={owner} sessionId={null} active dark={false} />)
    expect(view.container.querySelector('iframe')).toBeNull()
    expect(load).toHaveBeenCalledOnce()
    expect(configure).toHaveBeenLastCalledWith(expect.objectContaining({ context: { kind: 'standalone' }, visible: false }))
    click(byRole('tab', 'IceSky'))
    const frame = view.container.querySelector('iframe')
    expect(frame).not.toBeNull()
    expect(configure).toHaveBeenLastCalledWith(expect.objectContaining({ visible: true,
      appearance: expect.objectContaining({ dark: false, locale: 'zh' }) }))
    click(byRole('tab', '常用工具'))
    expect(view.container.querySelector('iframe')).toBe(frame)
    expect(frame?.parentElement?.hidden).toBe(true)
    click(byRole('tab', 'IceSky'))
    expect(view.container.querySelectorAll('iframe')).toHaveLength(1)
    expect(frame?.parentElement?.hidden).toBe(false)
    view.rerender(<CtfWorkbench owner={owner} sessionId="chat-1" active dark />)
    expect(configure).toHaveBeenLastCalledWith(expect.objectContaining({ context: { kind: 'session', id: 'chat-1' },
      appearance: expect.objectContaining({ dark: true }) }))
  })
})
