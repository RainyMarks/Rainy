/** The preview tools: what reaches the model, which addresses open, and how a failure is reported. */
import { expect, it } from 'vitest'
import { Observations, type ToolContext } from '../../src/host/agent/tools/common.ts'
import { browserTools, renderPage, type PreviewAnswer } from '../../src/host/agent/tools/browser.ts'
import { isLoopbackUrl } from '../../src/main/preview-browser.ts'

const context: ToolContext = {
  cwd: '/project', sessionId: 'browser-test', spillDir: '/tmp/spill', toolTokens: () => 4000, observations: new Observations(),
  runtimeEnvironment: () => ({ environment: {}, executables: {} }),
}

const text = (result: { content: { type: string; text?: string }[] }): string =>
  result.content.flatMap(block => block.type === 'text' && block.text !== undefined ? [block.text] : []).join('\n')

function tools(answer: PreviewAnswer | ((command: { action: string }) => PreviewAnswer), vision = false) {
  const calls: Record<string, unknown>[] = []
  const list = browserTools(context, (command) => {
    calls.push(command)
    return Promise.resolve(typeof answer === 'function' ? answer(command) : answer)
  }, vision)
  const by = (name: string) => list.find(tool => tool.name === name)!
  return { calls, by, names: list.map(tool => tool.name) }
}

it('reports the page, its elements and its console errors in one result', async () => {
  const { by, calls } = tools({
    url: 'http://localhost:5173/', title: 'Invoice', text: 'Total: 120.00', elements: ['button "Save"', 'textbox "Amount"'],
    console: ['error: Uncaught TypeError: total is not a function (app.js:12)'], failed: ['http://localhost:5173/api/rate: net::ERR_CONNECTION_REFUSED'],
  })
  const result = await by('browser_open').execute('call-1', { url: 'http://localhost:5173/' }, undefined, undefined)
  expect(calls).toEqual([{ action: 'open', url: 'http://localhost:5173/' }])
  const said = text(result)
  expect(said).toContain('URL: http://localhost:5173/')
  expect(said).toContain('button "Save"')
  expect(said).toContain('Uncaught TypeError')
  expect(said).toContain('ERR_CONNECTION_REFUSED')
  expect(said).toContain('Total: 120.00')
  expect(result.details).toEqual({ url: 'http://localhost:5173/', title: 'Invoice', errors: 2 })
})

it('says plainly when a page has no errors, so the model does not guess', () => {
  expect(renderPage({ url: 'http://127.0.0.1:3000/', title: 'App', text: 'Hello', elements: [], console: [], failed: [] }))
    .toContain('Console: no errors or warnings.')
})

it('opens only addresses on this machine', () => {
  for (const url of ['http://localhost:5173/', 'http://127.0.0.1:3000/x', 'http://app.localhost:8080/', 'https://localhost:8443/']) {
    expect(isLoopbackUrl(url)).toBe(true)
  }
  for (const url of ['https://example.com/', 'http://192.168.1.10:3000/', 'file:///c:/secrets.txt', 'not a url', 'http://localhost.example.com/']) {
    expect(isLoopbackUrl(url)).toBe(false)
  }
})

it('passes a click through as the page command and marks a missing element as a failure', async () => {
  const { by, calls } = tools(command => (command.action === 'click'
    ? { error: 'no element matches "Save"', url: 'http://localhost:5173/', text: 'Total', console: [], failed: [] }
    : { url: 'http://localhost:5173/' }))
  const result = await by('browser_act').execute('call-1', { action: 'click', target: 'Save' }, undefined, undefined)
  expect(calls).toEqual([{ action: 'click', target: 'Save', text: '' }])
  expect(result.isError).toBe(true)
  expect(text(result)).toContain('no element matches "Save"')
})

it('sends a key press with its key and defaults to Enter', async () => {
  const { by, calls } = tools({ url: 'http://localhost:5173/' })
  await by('browser_act').execute('call-1', { action: 'press', target: 'Amount' }, undefined, undefined)
  await by('browser_act').execute('call-2', { action: 'type', target: 'Amount', text: '42' }, undefined, undefined)
  expect(calls).toEqual([
    { action: 'press', target: 'Amount', key: 'Enter' },
    { action: 'type', target: 'Amount', text: '42' },
  ])
})

it('offers the screenshot only to a model that accepts images, and returns it as one', async () => {
  expect(tools({}).names).toEqual(['browser_open', 'browser_read', 'browser_act'])
  const { by } = tools({ url: 'http://localhost:5173/', image: 'aGk=' }, true)
  const result = await by('browser_screenshot').execute('call-1', {}, undefined, undefined)
  expect(result.content).toEqual([
    { type: 'text', text: 'Screenshot of http://localhost:5173/' },
    { type: 'image', data: 'aGk=', mimeType: 'image/png' },
  ])
})

it('fails the call when no page is open yet', async () => {
  const { by } = tools({ error: 'The preview has no page open yet; open one with browser_open first.' })
  const result = await by('browser_read').execute('call-1', {}, undefined, undefined)
  expect(result.isError).toBe(true)
  expect(text(result)).toContain('no page open yet')
})
