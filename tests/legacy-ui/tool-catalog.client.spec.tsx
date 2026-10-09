// @vitest-environment jsdom
/** Directory discovery, launch availability and lazy retained IceSky navigation. */
import { afterEach, describe, expect, it, vi } from 'vitest'
import { cleanup, fireEvent, render, screen, within } from '@testing-library/react'
import type { ComponentProps } from 'react'
import { useState } from 'react'
import type { TranslateNS } from '@deepseek-ai/dsh-client-locale/client'
import { ToolCatalog } from '../src/client/ToolCatalog.tsx'
import { CtfWorkbench } from '../src/client/ctf.tsx'
import type { NativeToolsState } from '../src/client/native-tools.ts'
import { zh } from '../src/client/locales.ts'
import { globalProps } from './global-props.client.ts'

afterEach(cleanup)
const t = ((key: keyof typeof zh, values?: Record<string, string>) => zh[key].replace(/\{(\w+)\}/g,
  (match, name: string) => values?.[name] ?? match)) as TranslateNS<'rainy'>

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
  const view = render(<ToolCatalog t={t} state={next} {...actions} />)
  return { ...actions, ...view }
}

describe('common tool catalog', () => {
  it('updates only outdated downloaded tools from a checked channel', () => {
    const h = fixture({ ...state, tools: state.tools.map(tool => tool.id === 'x64dbg' ? { ...tool, outdated: true, downloadBytes: 3 * 1024 ** 2 } : tool),
      updateBytes: 3 * 1024 ** 2, update: { phase: 'available', version: '1.0.6', error: '' } })
    expect(screen.getByText('有新工具或更新：1.0.6')).toBeTruthy()
    expect(within(screen.getByRole('heading', { name: 'x64dbg' }).closest('li')!).getByText('有更新')).toBeTruthy()
    fireEvent.click(screen.getByRole('button', { name: '更新已下载的工具（3.0MB）' }))
    expect(h.operateTools).toHaveBeenCalledExactlyOnceWith('update')
    fireEvent.click(screen.getByRole('button', { name: '检查工具更新' }))
    expect(h.checkToolUpdates).toHaveBeenCalledOnce()
  })
  it('downloads one tool on demand, shows its progress, and hides download actions once nothing is missing', () => {
    const h = fixture({ ...state, tools: [...state.tools, pending] })
    expect(screen.getByText('已下载 3 / 4 款工具，用到哪款就下载哪款')).toBeTruthy()
    const card = screen.getByRole('heading', { name: 'jq' }).closest('li')!
    expect(within(card).queryByRole('button', { name: '打开 jq' })).toBeNull()
    fireEvent.click(within(card).getByRole('button', { name: '下载 jq' }))
    expect(h.operateTools).toHaveBeenLastCalledWith('install', ['jq'])
    fireEvent.click(screen.getByRole('button', { name: '全部下载（1.0MB）' }))
    expect(h.operateTools).toHaveBeenLastCalledWith('install', ['jq'])
    h.rerender(<ToolCatalog t={t} state={{ ...state, tools: [...state.tools, pending],
      download: { phase: 'downloading', completedBytes: 5, totalBytes: 10, error: '', operation: 'install', tools: ['jq'] } }} {...h} />)
    expect(screen.getAllByText('正在下载 50%')).toHaveLength(2)
    expect(screen.getByRole('progressbar').getAttribute('value')).toBe('50')
    expect(screen.getByRole('button', { name: '下载 jq' }).hasAttribute('disabled')).toBe(true)
    fireEvent.click(screen.getByRole('button', { name: '取消' }))
    expect(h.cancelDownload).toHaveBeenCalledOnce()
    h.rerender(<ToolCatalog t={t} state={state} {...h} />)
    expect(screen.queryByRole('button', { name: /^全部下载/ })).toBeNull()
    fireEvent.click(screen.getByRole('button', { name: '已下载' }))
    expect(screen.getAllByRole('listitem')).toHaveLength(3)
  })
  it('removes a downloaded tool only after a second confirming click', () => {
    const h = fixture()
    fireEvent.click(screen.getByRole('button', { name: '移除 CyberChef' }))
    expect(h.operateTools).not.toHaveBeenCalled()
    fireEvent.click(screen.getByRole('button', { name: '确认移除 CyberChef' }))
    expect(h.operateTools).toHaveBeenCalledExactlyOnceWith('remove', ['cyberchef'])
  })
  it('groups the complete list by task and offers only groups that contain tools', () => {
    fixture()
    expect(screen.getAllByRole('heading', { level: 2 }).map(heading => heading.firstChild?.textContent)).toEqual(['逆向调试', '取证与文件', '编码与数据'])
    const reverse = screen.getByRole('region', { name: '逆向调试' })
    expect(within(reverse).getByRole('heading', { name: 'x64dbg' })).toBeTruthy()
    expect(within(reverse).queryByText('逆向调试', { selector: 'span' })).toBeNull()
    expect(screen.queryByRole('button', { name: '音频与信号' })).toBeNull()
    fireEvent.change(screen.getByRole('textbox'), { target: { value: '7' } })
    expect(screen.queryAllByRole('heading', { level: 2 })).toHaveLength(0)
    expect(within(screen.getByRole('list', { name: '常用工具' })).getByText('取证与文件')).toBeTruthy()
  })
  it('filters by category, localized use, favorites and recent order', () => {
    const h = fixture()
    fireEvent.click(screen.getByRole('button', { name: '逆向调试' }))
    expect(screen.getAllByRole('listitem')).toHaveLength(1)
    expect(screen.getByRole('heading', { name: 'x64dbg' })).toBeTruthy()
    fireEvent.click(screen.getByRole('button', { name: '全部' }))
    fireEvent.change(screen.getByRole('textbox'), { target: { value: '编码' } })
    expect(screen.getAllByRole('listitem')).toHaveLength(1)
    expect(screen.getByRole('heading', { name: 'CyberChef' })).toBeTruthy()
    fireEvent.change(screen.getByRole('textbox'), { target: { value: '' } })
    fireEvent.click(screen.getByRole('button', { name: '收藏' }))
    expect(screen.getAllByRole('listitem')).toHaveLength(1)
    fireEvent.click(screen.getByRole('button', { name: '取消收藏 CyberChef' }))
    expect(h.toggleFavorite).toHaveBeenCalledExactlyOnceWith('cyberchef')
    fireEvent.click(screen.getByRole('button', { name: '最近使用' }))
    expect(screen.getAllByRole('heading').map(row => row.textContent)).toEqual(['x64dbg', 'CyberChef'])
  })

  it('distinguishes unverified from missing and disables overlapping launches', () => {
    const h = fixture()
    const cyberchef = screen.getByRole('heading', { name: 'CyberChef' }).closest('li')!
    expect(within(cyberchef).getByText('待验证')).toBeTruthy()
    fireEvent.click(screen.getByRole('button', { name: '打开 CyberChef' }))
    expect(h.launchTool).toHaveBeenLastCalledWith('cyberchef')
    fireEvent.click(screen.getByRole('button', { name: '打开 x32dbg' }))
    expect(h.launchTool).toHaveBeenLastCalledWith('x64dbg', 'x32')
    expect(screen.getByRole('button', { name: '打开 7-Zip' }).hasAttribute('disabled')).toBe(true)
    expect(screen.getByText('7zFM.exe')).toBeTruthy()
    h.rerender(<ToolCatalog t={t} state={{ ...state, pending: ['x64dbg'] }} {...h} />)
    expect(screen.getByRole('button', { name: '打开 x64dbg' }).hasAttribute('disabled')).toBe(true)
    expect(screen.getByRole('button', { name: '打开 x32dbg' }).hasAttribute('disabled')).toBe(true)
    expect(screen.getByRole('button', { name: '刷新工具状态' }).hasAttribute('disabled')).toBe(true)
  })

  it('retains rows and offers retry after a directory refresh fails', () => {
    const h = fixture({ ...state, phase: 'error', error: 'Directory unavailable' })
    expect(screen.getAllByRole('listitem')).toHaveLength(3)
    expect(screen.getByRole('alert').textContent).toContain('Directory unavailable')
    fireEvent.click(screen.getByRole('button', { name: '重试' }))
    expect(h.loadTools).toHaveBeenCalledOnce()
  })

  it('repairs a tool with missing files by downloading its damaged parts again', () => {
    const h = fixture()
    const missing = screen.getByRole('heading', { name: '7-Zip' }).closest('li')!
    expect(within(missing).getByText('重新下载缺失或损坏的文件')).toBeTruthy()
    fireEvent.click(within(missing).getByRole('button', { name: '修复' }))
    expect(h.operateTools).toHaveBeenCalledExactlyOnceWith('repair')
    expect(h.launchTool).not.toHaveBeenCalled()
  })

  it('offers repair when only the alternate executable is missing', () => {
    fixture({ ...state, tools: [{ ...current, id: 'x64dbg', name: 'x64dbg', category: 'reverse', version: '2026',
      launchKind: 'desktop', status: 'ready', verified: false, missing: [],
      variants: [{ id: 'x32', name: 'x32dbg', status: 'missing' }] }] })
    expect(screen.getByRole('button', { name: '打开 x64dbg' }).hasAttribute('disabled')).toBe(false)
    expect(screen.getByRole('button', { name: '打开 x32dbg' }).hasAttribute('disabled')).toBe(true)
    expect(screen.getByRole('button', { name: '修复' })).toBeTruthy()
  })

  it('offers no native launch controls in a browser without the desktop bridge', () => {
    fixture({ ...state, phase: 'desktop-only' })
    expect(screen.getByText('在 RainyAgent 桌面应用中使用常用工具')).toBeTruthy()
    expect(screen.queryByRole('button')).toBeNull()
  })
})

describe('CTF directory and IceSky tabs', () => {
  it('mounts one IceSky frame on first selection and preserves it across directory visits', () => {
    const attach = vi.fn()
    const loadTools = vi.fn(async () => {})
    function Harness() {
      const [view, selectView] = useState<'catalog' | 'icesky'>('catalog')
      const props: ComponentProps<typeof CtfWorkbench> = {
        ...globalProps,
        width: 550, close: vi.fn(), t,
        useWorkbench: selector => selector({ phase: 'ready', saving: 'idle', error: undefined, message: '' }),
        useTools: selector => selector(state), useView: selector => selector(view), selectView,
        attach, loadTools, launchTool: vi.fn(async () => {}), toggleFavorite: vi.fn(async () => {}),
        operateTools: vi.fn(async () => {}), cancelDownload: vi.fn(async () => {}), checkToolUpdates: vi.fn(async () => {}),
        retry: vi.fn(async () => true), loadFailed: vi.fn(),
      }
      return <CtfWorkbench {...props} />
    }
    const h = render(<Harness />)
    expect(h.container.querySelector('iframe')).toBeNull()
    expect(loadTools).toHaveBeenCalledOnce()
    fireEvent.click(screen.getByRole('tab', { name: 'IceSky' }))
    const frame = h.container.querySelector('iframe')
    expect(frame).not.toBeNull()
    fireEvent.click(screen.getByRole('tab', { name: '常用工具' }))
    expect(h.container.querySelector('iframe')).toBe(frame)
    expect(frame?.parentElement?.hidden).toBe(true)
    fireEvent.click(screen.getByRole('tab', { name: 'IceSky' }))
    expect(h.container.querySelectorAll('iframe')).toHaveLength(1)
    expect(frame?.parentElement?.hidden).toBe(false)
  })
})
