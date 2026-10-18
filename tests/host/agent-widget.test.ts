/** Interactive answers on the Host: the show_widget tool, its place in a chat, and the open route serving the sandboxed frame. */
import { join, resolve } from 'node:path'
import { afterEach, describe, expect, it } from 'vitest'
import { widgetTool } from '../../src/host/agent/tools/widget.ts'
import { HostServer } from '../../src/host/server.ts'
import { framePolicy, installWidget } from '../../src/host/widget.ts'
import { WIDGET_FRAME_PATH } from '../../src/shared/widget-protocol.ts'
import { FakeOpenAI, harness, idle } from './agent-fixtures.ts'

const resources = join(resolve(import.meta.dirname, '../..'), 'resources')
const text = (result: { content: { type: string; text?: string }[] }): string => result.content.map(block => block.text ?? '').join('')

describe('show_widget', () => {
  it('accepts a widget and refuses empty or oversized ones', async () => {
    const tool = widgetTool()
    const result = await tool.execute('1', { title: '计算器', height: 300, css: '', html: '<p>hi</p>' }, undefined)
    expect(text(result)).toContain('shown to the user')
    await expect(tool.execute('1', { title: 'x', height: 100, css: 'p{}', html: '  ' }, undefined)).rejects.toThrow('needs markup')
    await expect(tool.execute('1', { title: 'x', height: 100, css: '', html: 'x'.repeat(200_001) }, undefined)).rejects.toThrow('under 200000')
    // The order the model writes the fields in is the order the user sees them stream.
    expect(Object.keys(tool.parameters.properties)).toEqual(['title', 'height', 'css', 'html', 'script'])
  })
})

describe('show_widget in a chat', () => {
  const cleanups: (() => Promise<void>)[] = []
  let server: FakeOpenAI | undefined
  afterEach(async () => {
    for (const cleanup of cleanups.splice(0).reverse()) await cleanup()
    await server?.stop()
  })
  const toolNames = (request: Record<string, unknown> | undefined): string[] =>
    ((request?.tools ?? []) as { function?: { name?: string } }[]).map(tool => tool.function?.name ?? '')

  it('is offered by default, runs without approval even in read-only mode, and goes away with the switch', async () => {
    server = new FakeOpenAI([
      { toolCalls: [{ name: 'show_widget', arguments: { title: '滑块', height: 200, css: '', html: '<input type="range">', script: '' } }] },
      { text: '拖动滑块试试。' },
      { text: 'off' },
    ])
    await server.start()
    const h = await harness(server, cleanups)
    const chat = await h.call('sessions.create', { workspaceId: h.workspaceId, permission: 'read-only' })
    await h.call('chat.send', { sessionId: chat.id, text: '做个滑块' })
    await idle(h, chat.id)
    expect(toolNames(server.requests[0])).toContain('show_widget')
    expect(JSON.stringify(server.requests[0])).toContain('answer with show_widget instead of describing it in text')
    const snapshot = await h.call('sessions.get', { sessionId: chat.id })
    const result = snapshot.entries.find(entry => entry.kind === 'toolResult')
    expect(result).toMatchObject({ toolName: 'show_widget', isError: false })
    await h.settings.setPrefs({ widgets: false })
    await h.call('chat.send', { sessionId: chat.id, text: 'again' })
    await idle(h, chat.id)
    expect(toolNames(server.requests.at(-1))).not.toContain('show_widget')
  })
})

describe('widget route', () => {
  let server: HostServer | undefined
  afterEach(async () => { await server?.close() })

  it('serves the frame and its libraries without the session cookie, and nothing else', async () => {
    server = new HostServer({ port: 0, rendererRoot: resources, injectedGlobals: () => ({}), log: () => undefined })
    installWidget(server, resources)
    server.route('/rainy/private', () => { throw new Error('must not be reached') })
    await server.listen()
    const base = `http://127.0.0.1:${server.port}`
    const frame = await fetch(`${base}${WIDGET_FRAME_PATH}`)
    expect(frame.status).toBe(200)
    expect(frame.headers.get('content-security-policy')).toBe(framePolicy(server.port))
    expect(framePolicy(server.port)).toMatch(/^sandbox allow-scripts; default-src 'none'; .*connect-src 'none'/u)
    const html = await frame.text()
    expect(html).toContain('<script type="importmap">{"imports":{"three":"/rainy/widget/lib/three.module.min.js"')
    expect(html).toContain("var TAG = 'rainy-widget'")
    expect(html).toContain('var Idiomorph=')
    for (const name of ['lib/chart.umd.min.js', 'lib/d3.min.js', 'lib/three.module.min.js', 'lib/three-addons/controls/OrbitControls.js', 'fonts/hanken-grotesk-latin.woff2']) {
      const file = await fetch(`${base}/rainy/widget/${name}`)
      expect(file.status, name).toBe(200)
      expect(file.headers.get('access-control-allow-origin'), name).toBe('*')
    }
    for (const name of ['runtime.js', 'base.css', 'versions.json', 'licenses/d3.txt', 'lib/..%2F..%2Fpackage.json', 'lib/sub/x.js', 'fonts/x.ttf']) {
      expect((await fetch(`${base}/rainy/widget/${name}`)).status, name).toBe(404)
    }
    expect((await fetch(`${base}${WIDGET_FRAME_PATH}`, { method: 'POST' })).status).toBe(405)
    // Other routes still need the cookie.
    expect((await fetch(`${base}/rainy/private`)).status).toBe(403)
  })
})
