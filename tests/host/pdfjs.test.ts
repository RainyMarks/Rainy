/** pdf.js for the PDF viewer: only its two browser files are served. */
import { join, resolve } from 'node:path'
import { expect, it } from 'vitest'
import type { HostServer } from '../../src/host/server.ts'
import { installPdfJs } from '../../src/host/pdfjs.ts'

it('serves the pdf.js files from the resources and nothing else under its route', async () => {
  let handler: ((request: never, response: never, url: URL) => Promise<void>) | undefined
  installPdfJs({ route: (path: string, route: typeof handler) => { expect(path).toBe('/rainy/pdfjs/'); handler = route } } as unknown as HostServer,
    join(resolve(import.meta.dirname, '../..'), 'resources'))
  const call = async (method: string, path: string) => {
    const result: { status?: number; headers?: Record<string, unknown>; body?: Buffer } = {}
    await handler!({ method } as never, {
      writeHead: (status: number, headers?: Record<string, unknown>) => { result.status = status; result.headers = headers },
      end: (body?: Buffer) => { result.body = body },
    } as never, new URL(`http://127.0.0.1${path}`))
    return result
  }
  const script = await call('GET', '/rainy/pdfjs/pdf.min.js')
  expect(script.status).toBe(200)
  expect(script.headers?.['Content-Type']).toBe('text/javascript; charset=utf-8')
  expect(script.body!.length).toBeGreaterThan(100_000)
  expect((await call('GET', '/rainy/pdfjs/pdf.worker.min.js')).status).toBe(200)
  expect((await call('GET', '/rainy/pdfjs/LICENSE')).status).toBe(404)
  expect((await call('GET', '/rainy/pdfjs/../package.json')).status).toBe(404)
  expect((await call('POST', '/rainy/pdfjs/pdf.min.js')).status).toBe(404)
})
