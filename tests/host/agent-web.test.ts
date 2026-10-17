/** web_search and web_fetch through DeepSeek: requests, results, failures, approvals and which key pays. */
import { mkdtemp, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { afterEach, describe, expect, it } from 'vitest'
import { DEEPSEEK_FLASH } from '../../src/host/agent/models.ts'
import { decide, type PermissionInput } from '../../src/host/agent/permissions.ts'
import { Observations, type ToolContext } from '../../src/host/agent/tools/common.ts'
import { deepSeekKey, fetchableUrl, webFetchTool, webSearchTool, type WebAccess } from '../../src/host/agent/tools/web.ts'
import { LOCAL_NO_KEY, type Settings } from '../../src/host/settings.ts'
import { FakeOpenAI, harness, idle } from './agent-fixtures.ts'

interface Sent { url: string; headers: Record<string, string>; body: Record<string, unknown> }

/** A DeepSeek stand-in answering each request with the next scripted response. */
function deepSeek(responses: { status: number; body: unknown }[], key: string | null = 'sk-test'): { web: WebAccess; sent: Sent[] } {
  const sent: Sent[] = []
  const web: WebAccess = {
    apiKey: () => key ?? undefined,
    fetch: (async (input: string | URL | Request, init?: RequestInit) => {
      sent.push({ url: String(input), headers: init?.headers as Record<string, string>, body: JSON.parse(String(init?.body)) as Record<string, unknown> })
      const next = responses.shift() ?? { status: 500, body: {} }
      return new Response(JSON.stringify(next.body), { status: next.status, headers: { 'Content-Type': 'application/json' } })
    }) as typeof fetch,
  }
  return { web, sent }
}

const text = (result: { content: { type: string; text?: string }[] }): string => result.content.map(block => block.text ?? '').join('')
const cleanups: string[] = []
afterEach(async () => { for (const dir of cleanups.splice(0)) await rm(dir, { recursive: true, force: true }) })

async function context(): Promise<ToolContext> {
  const dir = await mkdtemp(join(tmpdir(), 'rainy-web-'))
  cleanups.push(dir)
  return { cwd: dir, sessionId: 's', spillDir: join(dir, 'spill'), toolTokens: () => 4000, observations: new Observations(),
    runtimeEnvironment: () => { throw new Error('not used') } }
}

describe('web_search', () => {
  it('posts the query with the DeepSeek key and lists titles, URLs and summaries', async () => {
    const { web, sent } = deepSeek([{ status: 200, body: { results: [
      { title: 'DeepSeek V4', url: 'https://example.com/v4', site_name: 'Example', date: '2026-06-01T00:00:00+00:00', snippet: 'short', summary: 'long summary' },
      { title: '', url: 'https://example.org/x', snippet: 'only snippet' },
    ] } }])
    const result = await webSearchTool(web).execute('1', { query: ' DeepSeek V4 site:example.com ', count: 99 }, undefined)
    expect(sent[0]).toMatchObject({ url: 'https://api.deepseek.com/v1/tools/search', body: { query: 'DeepSeek V4 site:example.com', count: 20 } })
    expect(sent[0]!.headers.Authorization).toBe('Bearer sk-test')
    expect(text(result)).toContain('[1] DeepSeek V4\nURL: https://example.com/v4\nSite: Example\nDate: 2026-06-01\nlong summary')
    expect(text(result)).toContain('[2] https://example.org/x')
    expect(result.details.results).toEqual([
      { title: 'DeepSeek V4', url: 'https://example.com/v4', site: 'Example', date: '2026-06-01', snippet: 'short' },
      { title: 'https://example.org/x', url: 'https://example.org/x', site: undefined, date: undefined, snippet: 'only snippet' },
    ])
  })

  it('reports an empty search, rejects bad queries and explains account problems', async () => {
    expect(text(await webSearchTool(deepSeek([{ status: 200, body: { results: [] } }]).web).execute('1', { query: 'nothing' }, undefined))).toBe('No web results for "nothing".')
    await expect(webSearchTool(deepSeek([]).web).execute('1', { query: '  ' }, undefined)).rejects.toThrow('expected search terms')
    await expect(webSearchTool(deepSeek([]).web).execute('1', { query: 'x'.repeat(129) }, undefined)).rejects.toThrow('at most 128')
    await expect(webSearchTool(deepSeek([{ status: 402, body: { error: { message: 'Insufficient Balance' } } }]).web).execute('1', { query: 'a' }, undefined))
      .rejects.toThrow(/HTTP 402.*insufficient balance.*Insufficient Balance/u)
    await expect(webSearchTool(deepSeek([], null).web).execute('1', { query: 'a' }, undefined)).rejects.toThrow('needs a DeepSeek API key')
  })

  it('retries once after the rate limit', async () => {
    const { web, sent } = deepSeek([{ status: 429, body: {} }, { status: 200, body: { results: [{ title: 't', url: 'https://a.example', snippet: 's' }] } }])
    expect(text(await webSearchTool(web).execute('1', { query: 'a' }, undefined))).toContain('https://a.example')
    expect(sent).toHaveLength(2)
  })
})

describe('web_fetch', () => {
  it('returns the page as Markdown with its title', async () => {
    const { web, sent } = deepSeek([{ status: 200, body: { title: 'Article', content: '# Article\n\nBody.' } }])
    const result = await webFetchTool(web, await context()).execute('1', { url: 'https://example.com/a?b=1' }, undefined)
    expect(sent[0]).toMatchObject({ url: 'https://api.deepseek.com/v1/tools/fetch', body: { url: 'https://example.com/a?b=1' } })
    expect(text(result)).toBe('# Article\nURL: https://example.com/a?b=1\n\n# Article\n\nBody.')
    expect(result.details).toEqual({ url: 'https://example.com/a?b=1', title: 'Article', characters: 16 })
  })

  it('accepts only plain http(s) URLs and explains pages DeepSeek cannot read', async () => {
    expect(() => fetchableUrl('file:///etc/passwd')).toThrow('only http and https')
    expect(() => fetchableUrl('https://user:pw@example.com')).toThrow('user name or password')
    expect(() => fetchableUrl('example.com')).toThrow('absolute http(s) URL')
    await expect(webFetchTool(deepSeek([{ status: 404, body: {} }]).web, await context()).execute('1', { url: 'https://example.com' }, undefined))
      .rejects.toThrow('no readable page content')
    // A host that does not exist comes back as 500, after one retry.
    await expect(webFetchTool(deepSeek([{ status: 500, body: {} }, { status: 500, body: {} }]).web, await context()).execute('1', { url: 'https://missing.example' }, undefined))
      .rejects.toThrow('it may be unreachable')
  })
})

describe('web approvals', () => {
  const base: PermissionInput = { mode: 'ask', toolName: 'web_fetch', args: { url: 'https://Docs.Example.com/page' }, cwd: '.', roots: async () => [],
    rules: { commands: [], tools: [] }, readOnlyTools: new Set(), canRemember: true, windows: false }

  it('searches without asking in every mode', async () => {
    for (const mode of ['read-only', 'ask', 'auto'] as const) expect(await decide({ ...base, mode, toolName: 'web_search', args: { query: 'x' } })).toEqual({ kind: 'allow' })
  })

  it('asks before reading a new site, remembers the site, and does not ask in auto mode', async () => {
    expect(await decide(base)).toEqual({ kind: 'ask', approval: { reason: 'web', path: 'https://Docs.Example.com/page', rule: 'web_fetch:docs.example.com' } })
    expect(await decide({ ...base, mode: 'read-only' })).toMatchObject({ kind: 'ask' })
    expect(await decide({ ...base, canRemember: false })).toEqual({ kind: 'ask', approval: { reason: 'web', path: 'https://Docs.Example.com/page' } })
    expect(await decide({ ...base, rules: { commands: [], tools: ['web_fetch:docs.example.com'] } })).toEqual({ kind: 'allow' })
    expect(await decide({ ...base, rules: { commands: [], tools: ['web_fetch:example.com'] } })).toMatchObject({ kind: 'ask' })
    expect(await decide({ ...base, mode: 'auto' })).toEqual({ kind: 'allow' })
  })
})

describe('DeepSeek key', () => {
  const settings = (models: { provider: string; baseURL: string }[], keys: Record<string, string>, selected?: string): Settings => ({
    get: () => ({ models, selected: selected === undefined ? null : { provider: selected, model: 'm' } }),
    apiKey: (provider: string) => keys[provider],
  }) as unknown as Settings

  it('uses the selected DeepSeek model first, then any DeepSeek model with a real key', () => {
    const models = [{ provider: 'other', baseURL: 'https://api.example.com' }, { provider: 'ds1', baseURL: 'https://api.deepseek.com' },
      { provider: 'ds2', baseURL: 'https://API.deepseek.com/v1' }]
    expect(deepSeekKey(settings(models, { other: 'sk-other', ds1: 'sk-1', ds2: 'sk-2' }))).toBe('sk-1')
    expect(deepSeekKey(settings(models, { ds1: 'sk-1', ds2: 'sk-2' }, 'ds2'))).toBe('sk-2')
    expect(deepSeekKey(settings(models, { ds1: LOCAL_NO_KEY, ds2: 'sk-2' }))).toBe('sk-2')
    expect(deepSeekKey(settings(models, { other: 'sk-other' }))).toBeUndefined()
  })
})

describe('web tools in a chat', () => {
  const cleanups: (() => Promise<void>)[] = []
  let server: FakeOpenAI | undefined
  afterEach(async () => {
    for (const cleanup of cleanups.splice(0).reverse()) await cleanup()
    await server?.stop()
  })
  const toolNames = (request: Record<string, unknown> | undefined): string[] =>
    ((request?.tools ?? []) as { function?: { name?: string }; name?: string }[]).map(tool => tool.function?.name ?? tool.name ?? '')

  it('offers web_search and web_fetch only while a DeepSeek model has a key and the switch is on', async () => {
    server = new FakeOpenAI([{ text: 'one' }, { text: 'two' }, { text: 'three' }])
    await server.start()
    const h = await harness(server, cleanups)
    const chat = await h.call('sessions.create', { workspaceId: h.workspaceId })
    const ask = async (text: string): Promise<string[]> => {
      await h.call('chat.send', { sessionId: chat.id, text })
      await idle(h, chat.id)
      return toolNames(server!.requests.at(-1))
    }
    expect(await ask('no key yet')).not.toContain('web_search')
    await h.settings.update((data) => { data.models.push({ ...DEEPSEEK_FLASH }) })
    await h.settings.setApiKey(DEEPSEEK_FLASH.provider, 'sk-test')
    const names = await ask('with a key')
    expect(names).toEqual(expect.arrayContaining(['web_search', 'web_fetch']))
    expect(JSON.stringify(server.requests.at(-1))).toContain('Use web_search when the answer depends on current or external information')
    await h.settings.setPrefs({ webSearch: false })
    expect(await ask('switched off')).not.toContain('web_fetch')
  })
})
