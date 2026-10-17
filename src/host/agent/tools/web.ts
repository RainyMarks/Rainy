/** `web_search` and `web_fetch` through DeepSeek's Search and Fetch APIs, paid for with the user's DeepSeek API key. */
import { Type, type Static } from '@earendil-works/pi-ai'
import type { AgentTool } from '@earendil-works/pi-agent-core'
import { LOCAL_NO_KEY, type Settings } from '../../settings.ts'
import { limitOutput, textResult, type ToolContext } from './common.ts'

/** DeepSeek's API host; only models served from it lend their key to web search. */
export const DEEPSEEK_API = 'https://api.deepseek.com'
const SEARCH_URL = `${DEEPSEEK_API}/v1/tools/search`
const FETCH_URL = `${DEEPSEEK_API}/v1/tools/fetch`
const SEARCH_TIMEOUT_MS = 30_000
const FETCH_TIMEOUT_MS = 60_000
const DEFAULT_COUNT = 8
const MAX_COUNT = 20
/** Fetched pages beyond this are cut before the tool budget applies, so one huge page cannot fill the spill directory. */
const MAX_PAGE_CHARS = 400_000

/** What the web tools need from the Host. */
export interface WebAccess {
  /** The DeepSeek API key, or `undefined` when no DeepSeek model has one. */
  apiKey(): string | undefined
  /** Request function honoring the proxy settings. */
  fetch: typeof globalThis.fetch
}

/**
 * The DeepSeek key web search uses: the key of the selected model when it is a DeepSeek model, else of the first
 * DeepSeek model in Settings.
 * @param settings Settings and credentials.
 * @returns The key, or `undefined`.
 */
export function deepSeekKey(settings: Settings): string | undefined {
  const data = settings.get()
  const providers = data.models.filter(model => isDeepSeek(model.baseURL)).map(model => model.provider)
  const selected = data.selected?.provider
  const ordered = selected !== undefined && providers.includes(selected) ? [selected, ...providers.filter(provider => provider !== selected)] : providers
  for (const provider of ordered) {
    const key = settings.apiKey(provider)
    if (key !== undefined && key !== '' && key !== LOCAL_NO_KEY) return key
  }
  return undefined
}

/** @param baseURL A model's service address. @returns Whether it is DeepSeek's official API. */
export function isDeepSeek(baseURL: string): boolean {
  try { return new URL(baseURL).hostname.toLowerCase() === 'api.deepseek.com' } catch { return false }
}

/** One search result as the chat card shows it. */
export interface WebSearchResult { title: string; url: string; site?: string | undefined; date?: string | undefined; snippet: string }
export interface WebSearchDetails { query: string; results: WebSearchResult[] }
export interface WebFetchDetails { url: string; title: string; characters: number }

/** Why a call failed, in words the model and the user can act on. */
function failure(status: number, body: string, what: 'search' | 'fetch'): Error {
  let message = ''
  try { message = String((JSON.parse(body) as { error?: { message?: unknown } }).error?.message ?? '') } catch { message = body.slice(0, 300) }
  const reason: Record<number, string> = {
    401: 'the DeepSeek API key was rejected; the user can update it in Settings → Models',
    402: 'the DeepSeek account has insufficient balance; the user needs to top it up',
    403: 'DeepSeek refused to fetch this URL under its security policy',
    404: 'no readable page content was found at this URL (unavailable, blocked by the site, or not a web page)',
    429: `the DeepSeek ${what} rate limit was exceeded; wait a moment before retrying`,
  }
  // DeepSeek answers 500 rather than 404 for a page on a host that does not exist, so a fetch does not suggest retrying.
  const server = what === 'fetch' ? 'DeepSeek could not fetch this page; it may be unreachable, or DeepSeek had a temporary error'
    : 'DeepSeek had a temporary error; retry later'
  return new Error(`web ${what} failed (HTTP ${status}): ${reason[status] ?? (status >= 500 ? server : 'invalid request')}`
    + (message !== '' ? ` — ${message}` : ''))
}

async function post(web: WebAccess, url: string, body: Record<string, unknown>, timeoutMs: number, signal: AbortSignal | undefined, what: 'search' | 'fetch'): Promise<unknown> {
  const key = web.apiKey()
  if (key === undefined) throw new Error('web access needs a DeepSeek API key; the user can add a DeepSeek model with its key in Settings → Models')
  const abort = signal === undefined ? AbortSignal.timeout(timeoutMs) : AbortSignal.any([signal, AbortSignal.timeout(timeoutMs)])
  for (let attempt = 0; ; attempt++) {
    let response: Response
    try {
      response = await web.fetch(url, { method: 'POST', headers: { Authorization: `Bearer ${key}`, 'Content-Type': 'application/json' }, body: JSON.stringify(body), signal: abort })
    } catch (error) {
      if (signal?.aborted === true) throw new Error('tool call aborted')
      if (abort.aborted) throw new Error(`web ${what} timed out after ${timeoutMs / 1000} s`)
      throw new Error(`web ${what} could not reach DeepSeek: ${error instanceof Error ? error.message : String(error)}`)
    }
    const text = await response.text()
    if (response.ok) {
      try { return JSON.parse(text) } catch { throw new Error(`web ${what} returned an unreadable answer`) }
    }
    // One quiet retry for the per-second limit and transient server errors.
    if (attempt === 0 && (response.status === 429 || response.status >= 500)) {
      await new Promise(resolve => setTimeout(resolve, 1200))
      continue
    }
    throw failure(response.status, text, what)
  }
}

const string = (value: unknown): string => typeof value === 'string' ? value : ''

const searchParameters = Type.Object({
  query: Type.String({ description: 'Search terms, at most 128 characters. Supports site:, -site:, "exact phrase", -word, lang:zh,en, after:2025-01-01 and before:2026.' }),
  count: Type.Optional(Type.Number({ description: `Number of results, 1–${MAX_COUNT}. Defaults to ${DEFAULT_COUNT}.` })),
})

/**
 * The `web_search` tool.
 * @param web DeepSeek access.
 * @returns Tool definition.
 */
export function webSearchTool(web: WebAccess): AgentTool<typeof searchParameters, WebSearchDetails> {
  return {
    name: 'web_search',
    label: 'Web search',
    description: 'Search the web with the DeepSeek search engine for current or external information: documentation, papers, news, error '
      + 'messages, library versions. Returns titles, URLs, dates and a summary of each result; use web_fetch to read a page in full.',
    parameters: searchParameters,
    executionMode: 'sequential',
    async execute(_id, params: Static<typeof searchParameters>, signal) {
      const query = params.query.trim()
      if (query === '') throw new Error('invalid query: expected search terms')
      if ([...query].length > 128) throw new Error('invalid query: at most 128 characters')
      const count = Math.min(MAX_COUNT, Math.max(1, Math.floor(params.count ?? DEFAULT_COUNT)))
      const answer = await post(web, SEARCH_URL, { query, count }, SEARCH_TIMEOUT_MS, signal, 'search') as { results?: unknown }
      const raw = Array.isArray(answer.results) ? answer.results as Record<string, unknown>[] : []
      const results = raw.map(item => ({
        title: string(item.title) || string(item.url), url: string(item.url), site: string(item.site_name) || undefined,
        date: string(item.date).slice(0, 10) || undefined, snippet: string(item.snippet), summary: string(item.summary) || string(item.snippet),
      })).filter(item => item.url !== '')
      if (results.length === 0) return textResult(`No web results for "${query}".`, { query, results: [] })
      const text = results.map((item, index) => `[${index + 1}] ${item.title}\nURL: ${item.url}${item.site === undefined ? '' : `\nSite: ${item.site}`}`
        + `${item.date === undefined ? '' : `\nDate: ${item.date}`}\n${item.summary}`).join('\n\n')
      return textResult(`${text}\n\nCite the URLs you rely on.`, { query, results: results.map(({ summary: _summary, ...shown }) => shown) })
    },
  }
}

const fetchParameters = Type.Object({
  url: Type.String({ description: 'The http(s) URL of the page to read.' }),
})

/**
 * Check a URL before it leaves for DeepSeek.
 * @param value URL from the model.
 * @returns The normalized URL.
 */
export function fetchableUrl(value: string): URL {
  let url: URL
  try { url = new URL(value.trim()) } catch { throw new Error('invalid url: expected an absolute http(s) URL') }
  if (url.protocol !== 'http:' && url.protocol !== 'https:') throw new Error('invalid url: only http and https pages can be fetched')
  if (url.username !== '' || url.password !== '') throw new Error('invalid url: URLs with a user name or password cannot be fetched')
  if (url.href.length > 8192) throw new Error('invalid url: at most 8192 characters')
  return url
}

/**
 * The `web_fetch` tool.
 * @param web DeepSeek access.
 * @param context Chat tool context, for cutting long pages to the tool budget.
 * @returns Tool definition.
 */
export function webFetchTool(web: WebAccess, context: ToolContext): AgentTool<typeof fetchParameters, WebFetchDetails> {
  return {
    name: 'web_fetch',
    label: 'Web fetch',
    description: 'Read one web page as Markdown through DeepSeek. Use it for URLs from web_search or from the user. '
      + 'Pages are fetched by DeepSeek\'s servers, so pages on this computer or a private network cannot be reached.',
    parameters: fetchParameters,
    executionMode: 'sequential',
    async execute(_id, params: Static<typeof fetchParameters>, signal) {
      const url = fetchableUrl(params.url)
      const answer = await post(web, FETCH_URL, { url: url.href }, FETCH_TIMEOUT_MS, signal, 'fetch') as { title?: unknown; content?: unknown }
      const title = string(answer.title)
      let content = string(answer.content)
      const characters = content.length
      if (content.length > MAX_PAGE_CHARS) content = `${content.slice(0, MAX_PAGE_CHARS)}\n\n[page cut at ${MAX_PAGE_CHARS} characters]`
      const text = await limitOutput(context, 'web_fetch', `${title === '' ? '' : `# ${title}\n`}URL: ${url.href}\n\n${content}`)
      return textResult(text, { url: url.href, title, characters })
    },
  }
}
