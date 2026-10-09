/** Desktop tool processes start with a visible window. */
import { EventEmitter } from 'node:events'
import { describe, expect, it, vi } from 'vitest'
import type { NativeInvocation } from '../src/native-tools.ts'

const { spawn } = vi.hoisted(() => ({ spawn: vi.fn() }))
vi.mock('node:child_process', async importOriginal => ({ ...await importOriginal<typeof import('node:child_process')>(), spawn }))
const { startNativeProcess } = await import('../src/native-tool-process.ts')

describe('native tool process', () => {
  it('does not request SW_HIDE for a desktop program, which many tools honor and never show their window', async () => {
    const child = Object.assign(new EventEmitter(), { pid: 4242, unref: vi.fn() })
    spawn.mockImplementation(() => { queueMicrotask(() => child.emit('spawn')); return child })
    const invocation: NativeInvocation = { id: 'winmerge', name: 'WinMerge', kind: 'gui', target: 'C:\\tools\\winmerge\\WinMergeU.exe',
      executable: 'C:\\tools\\winmerge\\WinMergeU.exe', cwd: 'C:\\tools\\winmerge', args: [], roots: [], userData: 'C:\\data\\winmerge' }
    await startNativeProcess(invocation, { SystemRoot: 'C:\\Windows' })
    expect(spawn).toHaveBeenCalledOnce()
    expect(spawn.mock.calls[0]?.[2]).toMatchObject({ windowsHide: false, detached: true, shell: false, cwd: 'C:\\tools\\winmerge' })
    expect(child.unref).toHaveBeenCalledOnce()
  })
})
