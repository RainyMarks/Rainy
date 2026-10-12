/**
 * The Windows system proxy as Chromium resolves it for one address: proxy settings, the bypass list (including
 * `<local>`) and PAC scripts. The Windows Host asks for each service it connects to.
 */
import type { Session } from 'electron'

/**
 * The first HTTP proxy in a PAC-style answer. SOCKS proxies are not supported by the Host's request stack and count as none.
 * @param rules Answer such as `PROXY 127.0.0.1:7890; DIRECT`.
 * @returns `http://host:port`, `https://host:port`, or `null` for direct.
 */
export function proxyFromRules(rules: string): string | null {
  for (const rule of rules.split(';')) {
    const [kind = '', address = ''] = rule.trim().split(/\s+/u)
    const upper = kind.toUpperCase()
    if (upper === 'DIRECT') return null
    if ((upper === 'PROXY' || upper === 'HTTP' || upper === 'HTTPS') && /^[\w.-]+:\d{1,5}$|^\[[\da-f:]+\]:\d{1,5}$/iu.test(address)) {
      return `${upper === 'HTTPS' ? 'https' : 'http'}://${address}`
    }
  }
  return null
}

/**
 * @param session Session whose proxy configuration follows the system.
 * @param url `http(s)` address the Host is about to request.
 * @returns The proxy for it, or `null` for direct.
 */
export async function systemProxyFor(session: Pick<Session, 'resolveProxy'>, url: string): Promise<string | null> {
  return proxyFromRules(await session.resolveProxy(url))
}
