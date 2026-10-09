// @vitest-environment jsdom
/** Optional components are listed with their sizes and downloaded, cancelled or removed through the desktop bridge. */
import { afterEach, expect, it, vi } from 'vitest'
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import type { TranslateNS } from '@deepseek-ai/dsh-client-locale/client'
import { OptionalModules } from '../src/client/OptionalModules.tsx'
import type { OptionalModuleStatus, OptionalModulesBridge, OptionalModulesState } from '../src/modules-protocol.ts'
import { zh } from '../src/client/locales.ts'

const t = ((key: keyof typeof zh, values?: Record<string, string>) => zh[key].replace(/\{(\w+)\}/g,
  (match, name: string) => values?.[name] ?? match)) as TranslateNS<'rainy'>
const host = globalThis as typeof globalThis & { __RAINY_MODULES__?: OptionalModulesBridge }
afterEach(() => { cleanup(); delete host.__RAINY_MODULES__ })

function bridge(installed: boolean) {
  let modules: OptionalModuleStatus[] = [
    { id: 'strata', installed: false, downloadBytes: 500 * 1024 ** 2, unpackedBytes: 800 * 1024 ** 2 },
    { id: 'php', installed, downloadBytes: 36 * 1024 ** 2, unpackedBytes: 96 * 1024 ** 2 },
  ]
  let receive: ((state: OptionalModulesState) => void) | undefined
  const fake = {
    list: vi.fn(async () => modules),
    state: vi.fn(async (): Promise<OptionalModulesState> => ({ phase: 'idle', completedBytes: 0, totalBytes: 0, error: '' })),
    install: vi.fn(async () => { modules = modules.map(module => module.id === 'php' ? { ...module, installed: true } : module) }),
    remove: vi.fn(async () => { modules = modules.map(module => module.id === 'php' ? { ...module, installed: false } : module) }),
    cancel: vi.fn(async () => {}),
    onProgress: vi.fn((listener: (state: OptionalModulesState) => void) => { receive = listener; return () => { receive = undefined } }),
  }
  host.__RAINY_MODULES__ = fake
  return { fake, progress: (state: OptionalModulesState) => { receive?.(state) } }
}

it('lists components with their download size and downloads one on request', async () => {
  const h = bridge(false)
  render(<OptionalModules t={t} />)
  const download = await screen.findByRole('button', { name: '下载（36MB）' })
  expect(screen.getByText('可选组件')).toBeTruthy()
  expect(screen.getByRole('button', { name: '下载（500MB）' })).toBeTruthy()
  act(() => { h.progress({ phase: 'downloading', module: 'php', completedBytes: 18, totalBytes: 36, error: '' }) })
  expect(screen.getByText(/正在下载 50%/)).toBeTruthy()
  fireEvent.click(screen.getByRole('button', { name: '取消' }))
  expect(h.fake.cancel).toHaveBeenCalledOnce()
  act(() => { h.progress({ phase: 'idle', completedBytes: 0, totalBytes: 0, error: '' }) })
  fireEvent.click(download)
  expect(h.fake.install).toHaveBeenCalledExactlyOnceWith('php')
  await waitFor(() => { expect(screen.getByRole('button', { name: '删除' })).toBeTruthy() })
  expect(screen.getByText(/已下载，占用 96MB/)).toBeTruthy()
})

it('removes an installed component and shows only the requested one inline', async () => {
  const h = bridge(true)
  const changed = vi.fn()
  render(<OptionalModules t={t} only="php" onChange={changed} />)
  fireEvent.click(await screen.findByRole('button', { name: '删除' }))
  expect(h.fake.remove).toHaveBeenCalledExactlyOnceWith('php')
  await waitFor(() => { expect(changed).toHaveBeenCalledOnce() })
  expect(screen.queryByText('可选组件')).toBeNull()
  expect(screen.queryByRole('button', { name: '下载（500MB）' })).toBeNull()
})

it('renders nothing outside the desktop app', () => {
  const { container } = render(<OptionalModules t={t} />)
  expect(container.innerHTML).toBe('')
})
