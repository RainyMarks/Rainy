/**
 * Outbound HTTP for model services and MCP servers: direct, or through the proxy the user or the
 * system chose. Commands and terminals are not affected; the hosts they reach often sit on networks a proxy cannot reach.
 */
import { isIP } from 'node:net'
import { fetch as undiciFetch, ProxyAgent, type Dispatcher } from 'undici'
import type { ProxySettings, ProxyStatus } from '../shared/rpc.ts'

/** Shown instead of a proxy password; sending it back keeps the stored password. */
export const PASSWORD_MASK = '***'
/** How long the carrier's answer for one origin is reused; system proxy changes apply after at most this long. */
const SYSTEM_ANSWER_MS = 30_000
const MAX_SYSTEM_ANSWERS = 64
const MAX_AGENTS = 4
/** Stands for "any remote service" when Settings shows the system proxy. */
const REFERENCE_URL = 'https://example.com/'

function privateV4(address: string): boolean {
  const [a = 0, b = 0] = address.split('.').map(Number)
  return a === 127 || a === 10 || a === 0 || (a === 172 && b >= 16 && b <= 31) || (a === 192 && b === 168) || (a === 169 && b === 254)
}

/**
 * Hosts that never go through a proxy: this machine, private networks, `.local` names and single-label names (as
 * Windows' `<local>` rule), where local model servers live.
 * @param hostname URL host name, IPv6 in brackets or not.
 * @returns Whether requests to it connect directly.
 */
export function bypassesProxy(hostname: string): boolean {
  const host = hostname.replace(/^\[|\]$/gu, '').replace(/\.$/u, '').toLowerCase()
  if (host === 'localhost' || host.endsWith('.localhost') || host.endsWith('.local')) return true
  const family = isIP(host)
  if (family === 0) return !host.includes('.')
  if (family === 4) return privateV4(host)
  const mapped = /^::ffff:(?:(\d+\.\d+\.\d+\.\d+)|([\da-f]{1,4}):([\da-f]{1,4}))$/u.exec(host)
  if (mapped !== null) {
    if (mapped[1] !== undefined) return privateV4(mapped[1])
    const high = Number.parseInt(mapped[2] ?? '0', 16)
    const low = Number.parseInt(mapped[3] ?? '0', 16)
    return privateV4(`${high >> 8}.${high & 255}.${low >> 8}.${low & 255}`)
  }
  return host === '::1' || host === '::' || /^f[cd]/u.test(host) || /^fe[89ab]/u.test(host)
}

/**
 * Normalize a proxy address typed by the user.
 * @param text `host:port` or an `http(s)://` URL, optionally with `user:password@`.
 * @returns The URL, or `undefined` when it is not an HTTP proxy address.
 */
export function proxyUrl(text: string | null | undefined): string | undefined {
  const value = text?.trim() ?? ''
  if (value === '') return undefined
  try {
    const url = new URL(/^[a-z][a-z0-9+.-]*:\/\//iu.test(value) ? value : `http://${value}`)
    if ((url.protocol !== 'http:' && url.protocol !== 'https:') || url.hostname === '') return undefined
    const credentials = url.username === '' ? '' : `${url.username}${url.password === '' ? '' : `:${url.password}`}@`
    return `${url.protocol}//${credentials}${url.host}`
  } catch (_invalid) {
    return undefined
  }
}

/** @returns The address with its password replaced by {@link PASSWORD_MASK}. */
function masked(url: string): string {
  return url.replace(/^(\w+:\/\/[^:@/]+):[^@/]*@/u, `$1:${PASSWORD_MASK}@`)
}

/** Whether a `NO_PROXY` entry (a host, a domain with or without a leading dot, `*`) covers a host. */
function noProxyCovers(entry: string, host: string): boolean {
  const name = entry.toLowerCase().replace(/^\*?\./u, '').replace(/:\d+$/u, '')
  return entry === '*' || host === name || host.endsWith(`.${name}`)
}

/**
 * The proxy from `HTTPS_PROXY`/`HTTP_PROXY` for one URL, honouring `NO_PROXY`.
 * @param env Process environment.
 * @param url Request URL.
 * @returns Proxy URL, or `undefined` for direct.
 */
export function environmentProxy(env: NodeJS.ProcessEnv, url: URL): string | undefined {
  const host = url.hostname.replace(/^\[|\]$/gu, '').toLowerCase()
  const noProxy = (env.NO_PROXY ?? env.no_proxy ?? '').split(/[\s,]+/u).filter(Boolean)
  if (noProxy.some(entry => noProxyCovers(entry, host))) return undefined
  const https = env.HTTPS_PROXY ?? env.https_proxy
  const http = env.HTTP_PROXY ?? env.http_proxy
  return proxyUrl(url.protocol === 'https:' ? https ?? http : http ?? https)
}

/** Asks the Windows carrier which proxy the system uses for a URL: `http://host:port`, or `null` for direct. */
export type SystemProxyResolver = (url: string) => Promise<string | null>

/** The proxy settings and the request function every outbound model request uses. */
export class Network {
  private settings: ProxySettings = { mode: 'system' }
  private readonly answers = new Map<string, { at: number; proxy: Promise<string | undefined> }>()
  private readonly agents = new Map<string, Dispatcher>()
  private readonly listeners = new Set<() => void>()

  /**
   * @param platform Host platform; only the Windows Host asks the carrier, the WSL Host's network differs.
   * @param env Process environment, for `HTTPS_PROXY`, `HTTP_PROXY` and `NO_PROXY`.
   * @param resolveSystem The carrier's system proxy lookup; absent without the Electron carrier.
   */
  constructor(private readonly platform: 'win32' | 'linux', private readonly env: NodeJS.ProcessEnv = process.env,
    private readonly resolveSystem?: SystemProxyResolver) {}

  /** @param settings The user's choice from Settings. */
  configure(settings: ProxySettings | undefined): void {
    this.settings = settings ?? { mode: 'system' }
    for (const listener of this.listeners) listener()
  }

  /**
   * @param listener Called after the settings change.
   * @returns A function that removes the listener.
   */
  onChange(listener: () => void): () => void {
    this.listeners.add(listener)
    return () => { this.listeners.delete(listener) }
  }

  /**
   * Settings as typed in Settings, where a masked password stands for the stored one.
   * @param settings Settings from the page.
   * @returns Settings with the stored password restored when the rest of the address is unchanged.
   */
  withStoredPassword(settings: ProxySettings): ProxySettings {
    if (settings.mode !== 'manual' || this.settings.mode !== 'manual') return settings
    const typed = proxyUrl(settings.url)
    const stored = proxyUrl(this.settings.url)
    if (typed === undefined || stored === undefined) return settings
    const next = new URL(typed)
    const previous = new URL(stored)
    if (next.password !== encodeURIComponent(PASSWORD_MASK) && next.password !== PASSWORD_MASK) return settings
    if (next.protocol !== previous.protocol || next.host !== previous.host || next.username !== previous.username) return settings
    return { mode: 'manual', url: stored }
  }

  /**
   * The system proxy for one URL: the carrier's answer on Windows (proxy settings, bypass list and PAC), the
   * environment otherwise.
   * @param url Request URL.
   * @returns Proxy URL, or `undefined` for direct.
   */
  systemProxy(url: URL): Promise<string | undefined> {
    if (this.platform !== 'win32' || this.resolveSystem === undefined) return Promise.resolve(environmentProxy(this.env, url))
    const now = Date.now()
    const cached = this.answers.get(url.origin)
    if (cached !== undefined && now - cached.at < SYSTEM_ANSWER_MS) return cached.proxy
    const proxy = this.resolveSystem(`${url.origin}/`).then(
      answer => proxyUrl(answer),
      // Without an answer the environment decides, which on Windows usually means direct.
      () => environmentProxy(this.env, url),
    )
    this.answers.delete(url.origin)
    this.answers.set(url.origin, { at: now, proxy })
    if (this.answers.size > MAX_SYSTEM_ANSWERS) this.answers.delete(this.answers.keys().next().value ?? '')
    return proxy
  }

  /**
   * @param settings Proxy settings.
   * @param url Request URL.
   * @returns The proxy a request to the URL goes through, or `undefined` for direct.
   */
  async proxyFor(settings: ProxySettings, url: URL): Promise<string | undefined> {
    if (settings.mode === 'off' || bypassesProxy(url.hostname)) return undefined
    if (settings.mode === 'manual') return proxyUrl(settings.url)
    return this.systemProxy(url)
  }

  /**
   * What Settings shows; proxy passwords are masked.
   * @param target URL of the selected model, for the proxy in effect; a remote example when there is none.
   * @returns The stored settings, the proxy the target uses and the system proxy for remote services.
   */
  async status(target?: string): Promise<ProxyStatus> {
    let url: URL
    try { url = new URL(target ?? REFERENCE_URL) } catch (_invalid) { url = new URL(REFERENCE_URL) }
    const [effective, system] = await Promise.all([this.proxyFor(this.settings, url), this.systemProxy(new URL(REFERENCE_URL))])
    const stored = this.settings.mode === 'manual' ? proxyUrl(this.settings.url) : undefined
    const settings = stored !== undefined && new URL(stored).password !== '' ? { mode: 'manual' as const, url: masked(stored) } : this.settings
    return { settings, effective: effective === undefined ? null : masked(effective), system: system === undefined ? null : masked(system) }
  }

  /**
   * `fetch` for model services and MCP servers.
   * @param input Request URL or object.
   * @param init Request options.
   * @returns The response.
   */
  readonly fetch: typeof globalThis.fetch = (input, init) => this.fetchWith(this.settings, input, init)

  /**
   * `fetch` through given settings, for testing a choice before it is saved.
   * @param settings Proxy settings.
   * @param input Request URL or object.
   * @param init Request options.
   * @returns The response.
   */
  async fetchWith(settings: ProxySettings, input: Parameters<typeof globalThis.fetch>[0], init?: RequestInit): Promise<Response> {
    const target = new URL(typeof input === 'string' ? input : input instanceof URL ? input.href : input.url)
    const proxy = await this.proxyFor(settings, target)
    if (proxy === undefined) return globalThis.fetch(input, init)
    // undici's fetch accepts the standard arguments; its types differ from the DOM library's only in name.
    return undiciFetch(input as never, { ...init as object, dispatcher: this.agent(proxy) } as never) as unknown as Promise<Response>
  }

  /** @returns The dispatcher for one proxy; credentials in its address become a `Proxy-Authorization` header. */
  private agent(proxy: string): Dispatcher {
    const existing = this.agents.get(proxy)
    if (existing !== undefined) {
      this.agents.delete(proxy)
      this.agents.set(proxy, existing)
      return existing
    }
    const url = new URL(proxy)
    const token = url.username === '' ? undefined
      : `Basic ${Buffer.from(`${decodeURIComponent(url.username)}:${decodeURIComponent(url.password)}`).toString('base64')}`
    url.username = ''
    url.password = ''
    // Plain-HTTP requests are forwarded rather than tunnelled: many proxies allow CONNECT only to port 443.
    const agent = new ProxyAgent({ uri: url.href, proxyTunnel: false, ...(token === undefined ? {} : { token }) })
    this.agents.set(proxy, agent)
    if (this.agents.size > MAX_AGENTS) {
      const [oldest, dispatcher] = this.agents.entries().next().value ?? []
      if (oldest !== undefined) this.agents.delete(oldest)
      // Closing waits for requests still using it.
      void dispatcher?.close().catch(() => undefined)
    }
    return agent
  }
}
