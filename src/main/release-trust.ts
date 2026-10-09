/** Public release keys embedded in the carrier; no user or device activation state. */
import { createHash, createPublicKey } from 'node:crypto'
import { z } from 'zod'

declare const __RAINY_RELEASE_PUBLIC_KEYS__: unknown

/** Public trust anchors for the packaged resource inventory. */
export interface ReleaseKeyring { readonly version: 1; readonly keys: Readonly<Record<string, string>> }

/**
 * Identify an Ed25519 release key by its standard public encoding.
 * @param pem - SPKI public key in PEM format.
 * @returns the key fingerprint used by resource manifests.
 */
export function releasePublicKeyId(pem: string): string {
  const key = createPublicKey(pem)
  if (key.asymmetricKeyType !== 'ed25519') throw new Error('发行公钥必须使用 Ed25519')
  return createHash('sha256').update(key.export({ type: 'spki', format: 'der' })).digest('hex').slice(0, 32)
}

/**
 * Validate public release keys and reject private keys or mismatched fingerprints.
 * @param input - untrusted JSON from build resources.
 * @returns a nonempty public keyring.
 */
export function parseReleaseKeyring(input: unknown): ReleaseKeyring {
  const parsed = z.object({ version: z.literal(1), keys: z.record(z.string().regex(/^[a-f0-9]{32}$/), z.string().max(4096)) })
    .strict().parse(input)
  if (Object.keys(parsed.keys).length === 0) throw new Error('发行包缺少资源验证公钥')
  for (const [id, pem] of Object.entries(parsed.keys)) {
    if (!pem.startsWith('-----BEGIN PUBLIC KEY-----')) throw new Error('发行包只能包含发行公钥')
    if (releasePublicKeyId(pem) !== id) throw new Error('发行公钥标识无效')
  }
  return parsed
}

/**
 * Read the trust anchors compiled into this carrier.
 * @returns embedded keys, or undefined in an unpackaged development build.
 */
export function embeddedReleaseKeys(): ReleaseKeyring | undefined {
  if (typeof __RAINY_RELEASE_PUBLIC_KEYS__ === 'undefined' || __RAINY_RELEASE_PUBLIC_KEYS__ === null) return undefined
  return parseReleaseKeyring(__RAINY_RELEASE_PUBLIC_KEYS__)
}
