import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { mkdtemp, mkdir, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join, relative } from 'node:path'
import { once } from 'node:events'
import { serveNativeTool, type NativeToolWebPage } from '../../src/main/native-tool-web.ts'

const control = vi.hoisted(() => ({
  path: '', phase: '' as 'realpath' | 'stat' | 'destroy' | '',
  entered: Promise.withResolvers<undefined>(), release: Promise.withResolvers<undefined>(),
  streams: 0, server: undefined as import('node:http').Server | undefined,
}))

vi.mock('node:http', async (importOriginal) => {
  const actual = await importOriginal<typeof import('node:http')>()
  return { ...actual, createServer: (listener: import('node:http').RequestListener) => {
    control.server = actual.createServer(listener)
    return control.server
  } }
})

vi.mock('../../src/main/toolpack-fs.ts', async () => {
  const actual = await vi.importActual<typeof import('node:fs')>('node:fs')
  return { toolPackFileSystem: { ...actual, promises: { ...actual.promises,
    realpath: async (path: Parameters<typeof actual.promises.realpath>[0]) => {
      if (control.phase === 'realpath' && String(path) === control.path) {
        control.entered.resolve(undefined); await control.release.promise
      }
      return actual.promises.realpath(path)
    },
    stat: async (path: Parameters<typeof actual.promises.stat>[0]) => {
      if (control.phase === 'stat' && String(path) === control.path) {
        control.entered.resolve(undefined); await control.release.promise
      }
      return actual.promises.stat(path)
    },
  }, createReadStream: (...args: Parameters<typeof actual.createReadStream>) => {
    const stream = actual.createReadStream(...args)
    control.streams++
    if (control.phase === 'destroy') {
      const destroy = stream._destroy.bind(stream)
      stream._destroy = (error, callback) => {
        control.entered.resolve(undefined)
        void control.release.promise.then(() => { destroy(error, callback) })
      }
    }
    return stream
  },
  } }
})

let fixture: string
let page: NativeToolWebPage | undefined

beforeEach(async () => {
  fixture = await mkdtemp(join(tmpdir(), 'rainy-web-close-'))
  control.path = join(fixture, 'assets', 'index.html')
  control.phase = ''
  control.streams = 0
  control.entered = Promise.withResolvers<undefined>()
  control.release = Promise.withResolvers<undefined>()
  await mkdir(join(fixture, 'assets'))
  await writeFile(control.path, '<html>offline</html>')
  page = await serveNativeTool({ id: 'cyberchef', name: 'CyberChef', kind: 'web', target: control.path,
    executable: control.path, cwd: join(fixture, 'assets'), args: [], roots: [fixture], userData: fixture })
})

afterEach(async () => {
  control.release.resolve(undefined)
  await page?.close()
  page = undefined
  if (relative(tmpdir(), fixture).startsWith('..')) throw new Error('Fixture cleanup escaped the temporary directory')
  await rm(fixture, { recursive: true, force: true })
})

describe('offline tool shutdown', () => {
  it.each(['realpath', 'stat'] as const)('waits for a pending %s and does not open its file after closing', async (phase) => {
    if (!page || !control.server) throw new Error('Missing server fixture')
    control.phase = phase
    const request = fetch(page.url).then(response => response.text()).catch((error: unknown) => error)
    await control.entered.promise
    const serverClosed = once(control.server, 'close')
    let completed = false
    const closing = page.close().then(() => { completed = true })
    await serverClosed
    await new Promise<undefined>((resolve) => { setImmediate(() => { resolve(undefined) }) })
    expect(completed).toBe(false)
    control.release.resolve(undefined)
    await closing
    await request
    expect(control.streams).toBe(0)
  })

  it('waits for the file descriptor to close after the response completes', async () => {
    if (!page || !control.server) throw new Error('Missing server fixture')
    control.phase = 'destroy'
    const request = fetch(page.url).then(response => response.text()).catch((error: unknown) => error)
    await control.entered.promise
    const serverClosed = once(control.server, 'close')
    let completed = false
    const closing = page.close().then(() => { completed = true })
    await serverClosed
    await new Promise<undefined>((resolve) => { setImmediate(() => { resolve(undefined) }) })
    expect(completed).toBe(false)
    control.release.resolve(undefined)
    await closing
    await request
    expect(control.streams).toBe(1)
  })
})
