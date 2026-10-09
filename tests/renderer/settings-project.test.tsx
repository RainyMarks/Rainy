// @vitest-environment happy-dom
/** Runtime, memory and optional components stay attached to the current project and the desktop bridges. */
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { act } from 'react'
import type { WorkspaceId } from '../../src/shared/ide-files-protocol.ts'
import type { OptionalModuleStatus, OptionalModulesState } from '../../src/shared/modules-protocol.ts'
import type { ProjectMemoryStatus } from '../../src/shared/rpc.ts'
import type { RuntimeCandidate, RuntimeEnvironmentId, RuntimeSnapshot } from '../../src/shared/runtime-protocol.ts'
import { MemorySection } from '../../src/renderer/settings/MemorySection.tsx'
import { OptionalModules } from '../../src/renderer/settings/OptionalModules.tsx'
import { RuntimeSection } from '../../src/renderer/settings/RuntimeSection.tsx'
import type { RuntimeNativeHost } from '../../src/renderer/settings/native.ts'
import { settingsMessages } from '../../src/renderer/settings/messages.ts'
import {
  all, button, choose, cleanup, click, control, deferred, emit, fakeHost, findButton, handle, hasText, listenerCount, render, setBridge, toast, type,
  waitFor,
} from './settings-harness.tsx'

vi.mock('../../src/renderer/rpc.ts', async () => (await import('./settings-harness.tsx')).rpcModule)
vi.mock('../../src/renderer/ui/toasts.tsx', async () => (await import('./settings-harness.tsx')).toastsModule)

const zh = settingsMessages.zh
const projectA = { workspaceId: 'a' as WorkspaceId, path: '/project-a', title: 'A' }
const projectB = { workspaceId: 'b' as WorkspaceId, path: '/project-b', title: 'B' }
const calls = (method: string) => fakeHost.call.mock.calls.filter(([name]) => name === method).map(([, params]) => params)
const memory = (change: Partial<ProjectMemoryStatus> = {}): ProjectMemoryStatus => ({ enabled: true, generationEnabled: true, revision: 4, items: [], ...change })

beforeEach(async () => {
  await emit('prefs.changed', { locale: 'zh', theme: 'system', uiFontSize: 14, codeFontSize: 13, busyEnter: 'queue', stepDetail: 'standard', showUsage: true })
})
afterEach(async () => {
  await cleanup()
  setBridge('__RAINY_RUNTIME_NATIVE__', undefined)
  setBridge('__RAINY_MODULES__', undefined)
})

it('keeps memory recall and automatic organization independent and submits the edited revision', async () => {
  let current = memory({ items: [{ id: 'note', text: 'Use the project interpreter.', editedByUser: true,
    sources: [{ sessionId: 's1', seq: 3, executionTargetId: 'windows-local', file: { path: 'src/main.py', version: '10:20' } }] }] })
  handle('memory.status', () => current)
  handle('memory.setEnabled', ({ enabled, generationEnabled }) => {
    current = { ...current, ...enabled === undefined ? {} : { enabled }, ...generationEnabled === undefined ? {} : { generationEnabled } }
    return current
  })
  handle('memory.edit', ({ id, text }) => { current = { ...current, revision: 5, items: current.items.map(item => item.id === id ? { ...item, text } : item) }; return current })
  await render(<MemorySection workspace={projectA} />)
  await waitFor(() => { expect(hasText('Use the project interpreter.')).toBe(true) })
  expect(hasText('来源会话：s1 · 位置：3 · 环境：windows-local · src/main.py')).toBe(true)
  expect(hasText(zh.settingsMemoryEdited)).toBe(true)
  await click(button(zh.settingsMemoryUse))
  expect(calls('memory.setEnabled')).toEqual([{ workspaceId: 'a', enabled: false }])
  await waitFor(() => { expect(button(zh.settingsMemoryUse).getAttribute('aria-checked')).toBe('false') })
  expect(button(zh.settingsMemoryGenerate).getAttribute('aria-checked')).toBe('true')
  await click(button(zh.settingsMemoryEdit))
  await type(control<HTMLTextAreaElement>(zh.settingsMemoryText), 'Use Python from the selected environment.')
  await click(button(zh.settingsSave))
  await waitFor(() => { expect(calls('memory.edit')).toEqual([{ workspaceId: 'a', id: 'note', text: 'Use Python from the selected environment.', expectedRevision: 4 }]) })
  await waitFor(() => { expect(hasText('Use Python from the selected environment.')).toBe(true) })
  expect(toast).toHaveBeenCalledWith(zh.settingsSaved, { tone: 'success' })
})

it('deletes one note and clears all notes only after confirmation', async () => {
  let current = memory({ items: [{ id: 'one', text: 'First', sources: [] }, { id: 'two', text: 'Second', sources: [] }] })
  handle('memory.status', () => current)
  handle('memory.delete', ({ id }) => { current = { ...current, items: current.items.filter(item => item.id !== id) }; return current })
  handle('memory.clear', () => { current = { ...current, items: [] }; return current })
  await render(<MemorySection workspace={projectA} />)
  await waitFor(() => { expect(hasText('First')).toBe(true) })
  await click(button(zh.settingsMemoryDelete, document.querySelector('[data-memory="one"]')!))
  expect(calls('memory.delete')).toEqual([{ workspaceId: 'a', id: 'one' }])
  await waitFor(() => { expect(hasText('First')).toBe(false) })
  await click(button(zh.settingsMemoryClear))
  expect(calls('memory.clear')).toEqual([])
  await click(button(zh.settingsCancel))
  await click(button(zh.settingsMemoryClear))
  await click(button(zh.settingsMemoryClearConfirm))
  await waitFor(() => { expect(hasText(zh.settingsMemoryEmpty)).toBe(true) })
  expect(calls('memory.clear')).toEqual([{ workspaceId: 'a' }])
})

it('does not show a delayed memory response after another project becomes current', async () => {
  const pending = deferred<ProjectMemoryStatus>()
  handle('memory.status', ({ workspaceId }) => workspaceId === 'a' ? pending.promise : memory({ items: [{ id: 'b', text: 'Project B fact', sources: [] }] }))
  const view = await render(<MemorySection workspace={projectA} />)
  await view.rerender(<MemorySection workspace={projectB} />)
  await waitFor(() => { expect(hasText('Project B fact')).toBe(true) })
  await act(async () => { pending.resolve(memory({ items: [{ id: 'a', text: 'Project A fact', sources: [] }] })); await pending.promise })
  expect(hasText('Project A fact')).toBe(false)
  expect(hasText('Project B fact')).toBe(true)
})

it('reads memory again when the Host reports a change to the current project', async () => {
  let reads = 0
  handle('memory.status', () => { reads++; return memory({ generating: reads > 1 }) })
  const view = await render(<MemorySection workspace={projectA} />)
  await waitFor(() => { expect(reads).toBe(1) })
  await emit('memory.changed', { workspaceId: 'b' as WorkspaceId })
  expect(reads).toBe(1)
  await emit('memory.changed', { workspaceId: 'a' as WorkspaceId })
  await waitFor(() => { expect(hasText(zh.settingsMemoryGenerating)).toBe(true) })
  await view.unmount()
  expect(listenerCount('memory.changed')).toBe(0)
})

it('asks for a project folder before showing runtime or memory settings', async () => {
  await render(<><RuntimeSection workspace={null} /><MemorySection workspace={null} /></>)
  expect(all('[data-settings-section]').map(section => section.textContent?.includes(zh.settingsOpenProject))).toEqual([true, true])
  expect(fakeHost.call).not.toHaveBeenCalled()
  expect(findButton(zh.settingsExecutionTarget)).toBeUndefined()
  expect(findButton(zh.settingsPrepareEnvironments)).toBeUndefined()
})

function runtimeSnapshot(candidates: RuntimeCandidate[] = [], selected: RuntimeSnapshot['selected'] = {}): RuntimeSnapshot {
  return { targetId: 'windows-local' as RuntimeSnapshot['targetId'], workspaceId: 'a' as WorkspaceId, platform: 'windows', candidates, selected }
}
const python = (path: string, ready = true): RuntimeCandidate => ({ id: path as RuntimeEnvironmentId, language: 'python', path, source: 'system',
  platform: 'windows', version: ready ? '3.13.1' : null, ready, capabilities: [{ name: 'venv', ready, detail: ready ? undefined : 'missing ensurepip' }],
  ...ready ? {} : { error: 'python.exe exited with 1' } })

it('discovers, probes and selects interpreters for the current project', async () => {
  const system = python('C:\\Python313\\python.exe')
  const broken = python('C:\\broken\\python.exe', false)
  const manual = { ...python('D:\\tools\\python.exe'), source: 'manual' as const }
  let snapshot = runtimeSnapshot([system, broken])
  handle('runtime', (request) => {
    if (request.op === 'select') snapshot = { ...snapshot, selected: request.path === null ? {} : { python: snapshot.candidates.find(item => item.path === request.path)! } }
    if (request.op === 'probe') snapshot = { ...snapshot, candidates: [...snapshot.candidates, manual] }
    return snapshot
  })
  await render(<RuntimeSection workspace={projectA} />)
  await waitFor(() => { expect(document.querySelector('[data-candidate="C:\\\\Python313\\\\python.exe"]')).not.toBeNull() })
  expect(calls('runtime')).toEqual([{ op: 'status', workspaceId: 'a' }])
  const brokenCard = document.querySelector('[data-candidate="C:\\\\broken\\\\python.exe"]')!
  expect(brokenCard.textContent).toContain('python.exe exited with 1')
  expect(brokenCard.textContent).toContain('venv · 不可用 · missing ensurepip')
  await click(button(zh.settingsRuntime))
  expect(all<HTMLButtonElement>('[role="menuitem"]').map(item => `${item.textContent}:${String(item.disabled)}`))
    .toEqual([`${zh.settingsAutomatic}:false`, 'C:\\Python313\\python.exe · 3.13.1:false', 'C:\\broken\\python.exe:true'])
  await click(all<HTMLButtonElement>('[role="menuitem"]')[1]!)
  await waitFor(() => { expect(button(zh.settingsRuntime).textContent).toBe('C:\\Python313\\python.exe · 3.13.1') })
  expect(calls('runtime')[1]).toEqual({ op: 'select', workspaceId: 'a', language: 'python', path: 'C:\\Python313\\python.exe' })
  expect(toast).toHaveBeenCalledWith(zh.settingsApplied, { tone: 'success' })
  await choose(zh.settingsRuntime, zh.settingsAutomatic)
  expect(calls('runtime')[2]).toEqual({ op: 'select', workspaceId: 'a', language: 'python', path: null })
  expect(button(zh.settingsProbeEnvironment).disabled).toBe(true)
  await type(control(zh.settingsEnvironmentPath), '  D:\\tools\\python.exe ')
  await click(button(zh.settingsProbeEnvironment))
  expect(calls('runtime')[3]).toEqual({ op: 'probe', workspaceId: 'a', language: 'python', path: 'D:\\tools\\python.exe' })
  await waitFor(() => { expect(document.querySelector('[data-candidate="D:\\\\tools\\\\python.exe"]')).not.toBeNull() })
  await click(button(zh.settingsDiscoverEnvironments))
  expect(calls('runtime')[4]).toEqual({ op: 'discover', workspaceId: 'a' })
  await choose(zh.settingsLanguage, 'node')
  expect(all('[data-candidate]')).toEqual([])
})

function runtimeNative() {
  const windows = { id: 'windows-local', kind: 'windows' as const, label: 'Windows' }
  const ubuntu = { id: 'wsl:Ubuntu', kind: 'wsl' as const, label: 'Ubuntu' }
  let progress: ((message: string) => void) | undefined
  const stop = vi.fn()
  const native = {
    targets: vi.fn<RuntimeNativeHost['targets']>(async () => ({ current: windows, targets: [windows] })),
    switchTarget: vi.fn<RuntimeNativeHost['switchTarget']>(async () => ({ ok: true })),
    prepare: vi.fn<RuntimeNativeHost['prepare']>(async () => undefined),
    onProgress: vi.fn<RuntimeNativeHost['onProgress']>((listener) => { progress = listener; return stop }),
  }
  setBridge('__RAINY_RUNTIME_NATIVE__', native)
  return { native, windows, ubuntu, stop, progress: (message: string) => { progress?.(message) } }
}

it('shows component preparation progress and refreshes targets without changing the current project', async () => {
  handle('runtime', () => runtimeSnapshot())
  const h = runtimeNative()
  const pending = deferred<undefined>()
  h.native.targets.mockResolvedValueOnce({ current: h.windows, targets: [h.windows] }).mockResolvedValueOnce({ current: h.windows, targets: [h.windows, h.ubuntu] })
  h.native.prepare.mockReturnValueOnce(pending.promise)
  const view = await render(<RuntimeSection workspace={projectA} />)
  await waitFor(() => { expect(button(zh.settingsExecutionTarget).textContent).toBe('Windows') })
  await click(button(zh.settingsPrepareEnvironments))
  await waitFor(() => { expect(hasText(zh.settingsPreparingEnvironments)).toBe(true) })
  await act(async () => { h.progress('正在校验离线组件 2 / 3') })
  expect(hasText('正在校验离线组件 2 / 3')).toBe(true)
  await act(async () => { pending.resolve(undefined); await pending.promise })
  await waitFor(() => { expect(h.native.targets).toHaveBeenCalledTimes(2) })
  await waitFor(() => { expect(hasText('正在校验离线组件 2 / 3')).toBe(false) })
  await choose(zh.settingsExecutionTarget, 'Ubuntu')
  await waitFor(() => { expect(h.native.switchTarget).toHaveBeenCalledWith({ targetId: 'wsl:Ubuntu', workspaceId: 'a' }) })
  await view.unmount()
  expect(h.stop).toHaveBeenCalledOnce()
})

it('clears the waiting message and reports a failed preparation or a refused target switch', async () => {
  handle('runtime', () => runtimeSnapshot())
  const h = runtimeNative()
  h.native.targets.mockResolvedValue({ current: h.windows, targets: [h.windows, h.ubuntu] })
  h.native.prepare.mockRejectedValueOnce(new Error('离线包校验失败'))
  h.native.switchTarget.mockResolvedValueOnce({ ok: false, error: '请先结束运行中的 AI 任务' })
  await render(<RuntimeSection workspace={projectA} />)
  await click(button(zh.settingsPrepareEnvironments))
  await waitFor(() => { expect(toast).toHaveBeenCalledWith('离线包校验失败') })
  expect(hasText(zh.settingsPreparingEnvironments)).toBe(false)
  expect(button(zh.settingsPrepareEnvironments).disabled).toBe(false)
  await choose(zh.settingsExecutionTarget, 'Ubuntu')
  await waitFor(() => { expect(toast).toHaveBeenCalledWith('请先结束运行中的 AI 任务') })
})

function modules(installed: boolean) {
  let list: OptionalModuleStatus[] = [
    { id: 'strata', installed: false, downloadBytes: 500 * 1024 ** 2, unpackedBytes: 800 * 1024 ** 2 },
    { id: 'php', installed, downloadBytes: 36 * 1024 ** 2, unpackedBytes: 96 * 1024 ** 2 },
  ]
  let receive: ((state: OptionalModulesState) => void) | undefined
  const bridge = {
    list: vi.fn(async () => list),
    state: vi.fn(async (): Promise<OptionalModulesState> => ({ phase: 'idle', completedBytes: 0, totalBytes: 0, error: '' })),
    install: vi.fn(async () => { list = list.map(module => module.id === 'php' ? { ...module, installed: true } : module) }),
    remove: vi.fn(async () => { list = list.map(module => module.id === 'php' ? { ...module, installed: false } : module) }),
    cancel: vi.fn(async () => undefined),
    onProgress: vi.fn((listener: (state: OptionalModulesState) => void) => { receive = listener; return () => { receive = undefined } }),
  }
  setBridge('__RAINY_MODULES__', bridge)
  return { bridge, progress: async (state: OptionalModulesState) => { await act(async () => { receive?.(state) }) } }
}

it('lists components with their download size and downloads one on request', async () => {
  const h = modules(false)
  await render(<OptionalModules />)
  await waitFor(() => { expect(findButton('下载（36MB）')).toBeDefined() })
  expect(hasText(zh.modulesTitle)).toBe(true)
  expect(findButton('下载（500MB）')).toBeDefined()
  await h.progress({ phase: 'downloading', module: 'php', completedBytes: 18, totalBytes: 36, error: '' })
  expect(document.querySelector('[data-module="php"]')?.textContent).toContain('正在下载 50%')
  await click(button(zh.modulesCancel))
  expect(h.bridge.cancel).toHaveBeenCalledOnce()
  await h.progress({ phase: 'idle', completedBytes: 0, totalBytes: 0, error: '' })
  await click(button('下载（36MB）'))
  expect(h.bridge.install).toHaveBeenCalledExactlyOnceWith('php')
  await waitFor(() => { expect(findButton(zh.modulesRemove)).toBeDefined() })
  expect(document.querySelector('[data-module="php"]')?.textContent).toContain('已下载，占用 96MB')
})

it('removes an installed component and shows only the requested one inline', async () => {
  const h = modules(true)
  const changed = vi.fn()
  await render(<OptionalModules only="php" onChange={changed} />)
  await waitFor(() => { expect(findButton(zh.modulesRemove)).toBeDefined() })
  await click(button(zh.modulesRemove))
  expect(h.bridge.remove).toHaveBeenCalledExactlyOnceWith('php')
  await waitFor(() => { expect(changed).toHaveBeenCalledOnce() })
  expect(hasText(zh.modulesTitle)).toBe(false)
  expect(findButton('下载（500MB）')).toBeUndefined()
})

it('renders no components outside the desktop app', async () => {
  const { container } = await render(<OptionalModules />)
  expect(container.innerHTML).toBe('')
})
