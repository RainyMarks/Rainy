/** The IceSky workbench iframe: static files, model relay and durable drafts. */
import { stat } from 'node:fs/promises'
import { join } from 'node:path'
import type { HostEnvironment } from '../env.ts'
import type { HostServer } from '../server.ts'
import { createIceSkyProxyHandler } from './proxy.ts'
import { createIceSkyDraftHandler, IceSkyDraftStore } from './state.ts'
import { IceSkyStaticAssets } from './static.ts'

/** Largest complete draft request, including every tool's fields. */
export const ICESKY_MAX_DRAFT_BYTES = 8 * 1024 * 1024
/** Private cache lifetime of resources addressed by their content version. */
export const ICESKY_ASSET_MAX_AGE_SECONDS = 31536000

/** Dependencies of {@link installIceSky}. */
export interface IceSkyDependencies {
  readonly env: HostEnvironment
  readonly server: HostServer
  readonly log: (message: string) => void
}

/**
 * Whether a chat log exists under `<home>/chats`.
 * @param home Host data root.
 * @param sessionId Chat identity from a draft scope.
 * @returns Whether `chats/<sessionId>.jsonl` is a file; identities that are not plain file names never match.
 */
export async function chatExists(home: string, sessionId: string): Promise<boolean> {
  if (!/^[A-Za-z0-9_-][A-Za-z0-9_.-]{0,255}$/u.test(sessionId)) return false
  try { return (await stat(join(home, 'chats', `${sessionId}.jsonl`))).isFile() } catch (_missingChat) { return false }
}

/**
 * Register `/rainy/icesky/`, `/rainy/icesky/state` and the four model relay routes.
 * @param deps Host environment, server and diagnostics.
 * @throws Error when the built resource manifest is missing or does not match `index.html`.
 */
export async function installIceSky(deps: IceSkyDependencies): Promise<void> {
  const { env, server, log } = deps
  const assets = await IceSkyStaticAssets.open(join(env.resources, 'icesky'))
  let store: IceSkyDraftStore | undefined
  const statePath = join(env.home, 'icesky', 'state.json')
  try {
    store = await IceSkyDraftStore.open(statePath)
  } catch (error) {
    log(`IceSky drafts are unavailable; ${statePath} was left unchanged: ${error instanceof Error ? error.message : String(error)}`)
  }
  const serveStatic = (request: Parameters<IceSkyStaticAssets['handle']>[0], response: Parameters<IceSkyStaticAssets['handle']>[1]): Promise<void> =>
    assets.handle(request, response, { maxAgeSeconds: ICESKY_ASSET_MAX_AGE_SECONDS })
  server.route('/rainy/icesky', serveStatic, true)
  server.route('/rainy/icesky/', serveStatic)
  server.route('/rainy/icesky/state', createIceSkyDraftHandler(store, {
    maxDraftBytes: ICESKY_MAX_DRAFT_BYTES,
    sessionExists: sessionId => chatExists(env.home, sessionId),
  }))
  for (const provider of ['openai', 'anthropic'] as const) {
    for (const operation of ['chat', 'models'] as const) server.route(`/api/${provider}/${operation}`, createIceSkyProxyHandler(provider, operation))
  }
}
