/**
 * API keys at rest. With the Electron carrier, `.credentials.sealed.json` holds each key encrypted with AES-256-GCM
 * under a key the carrier keeps with Windows DPAPI (`src/main/credential-key.ts`) and sends over the control channel.
 * `.credentials.json` stays a version 1 file: empty once its keys are sealed, so an older RainyAgent starts without
 * keys instead of failing, and any key it stores there is sealed on the next start. Without the carrier (development
 * runs) keys stay in `.credentials.json` as before.
 *
 * A sealed value no key here can open is never deleted: it stays under its name until the user stores or removes that
 * key, and then moves to `unreadable` in the same file, so a restored carrier key could still recover it.
 */
import { createCipheriv, createDecipheriv, randomBytes } from 'node:crypto'
import { join } from 'node:path'
import { z } from 'zod'
import { readJson, writeJson } from './files.ts'

const IV_BYTES = 12
const TAG_BYTES = 16

/** Encrypts one credential value, bound to its name so values cannot be swapped between names. */
export interface CredentialSeal {
  /** @returns `base64(iv | tag | ciphertext)`. */
  seal(name: string, value: string): string
  /** @returns The value, or `undefined` when it was sealed with another key or changed. */
  open(name: string, sealed: string): string | undefined
}

/**
 * @param key 32-byte key from the carrier.
 * @returns Seal for credential values.
 */
export function credentialSeal(key: Buffer): CredentialSeal {
  if (key.length !== 32) throw new Error('The credential key must be 32 bytes')
  return {
    seal(name, value) {
      const iv = randomBytes(IV_BYTES)
      const cipher = createCipheriv('aes-256-gcm', key, iv)
      cipher.setAAD(Buffer.from(name, 'utf8'))
      const data = Buffer.concat([cipher.update(value, 'utf8'), cipher.final()])
      return Buffer.concat([iv, cipher.getAuthTag(), data]).toString('base64')
    },
    open(name, sealed) {
      const bytes = Buffer.from(sealed, 'base64')
      if (bytes.length < IV_BYTES + TAG_BYTES) return undefined
      try {
        const decipher = createDecipheriv('aes-256-gcm', key, bytes.subarray(0, IV_BYTES))
        decipher.setAAD(Buffer.from(name, 'utf8'))
        decipher.setAuthTag(bytes.subarray(IV_BYTES, IV_BYTES + TAG_BYTES))
        return Buffer.concat([decipher.update(bytes.subarray(IV_BYTES + TAG_BYTES)), decipher.final()]).toString('utf8')
      } catch (_otherKeyOrChanged) {
        return undefined
      }
    },
  }
}

const plainSchema = z.object({ version: z.literal(1), keys: z.record(z.string(), z.string()) })
const sealedSchema = z.object({
  version: z.literal(2),
  keys: z.record(z.string(), z.string()),
  /** Unreadable values the user replaced or removed since, newest last; RainyAgent 2.1.0 reads the file without it. */
  unreadable: z.record(z.string(), z.array(z.string())).optional(),
})

/** Replaced unreadable values kept per name. */
const MAX_SET_ASIDE = 8

/** The credential files of one Host home. */
export class CredentialFiles {
  private readonly plainFile: string
  private readonly sealedFile: string
  /** Sealed values this key cannot open; kept in the file under their name until it is stored again or removed. */
  private unreadable: Record<string, string> = {}
  /** Unreadable values the user replaced or removed, kept in the file's `unreadable` section. */
  private setAside: Record<string, string[]> = {}
  /** Without the carrier: names in the sealed file that the plain file does not hold. */
  private unopened: string[] = []

  /**
   * @param home Host home directory.
   * @param sealer Seal from the carrier's key; `undefined` keeps keys in the plain file.
   */
  constructor(home: string, private readonly sealer: CredentialSeal | undefined) {
    this.plainFile = join(home, '.credentials.json')
    this.sealedFile = join(home, '.credentials.sealed.json')
  }

  /** @returns Whether keys are encrypted at rest. */
  get sealed(): boolean {
    return this.sealer !== undefined
  }

  /** @returns Names of stored keys that could not be opened and have not been stored again or removed since. */
  get unreadableNames(): string[] {
    return this.sealer === undefined ? [...this.unopened] : Object.keys(this.unreadable)
  }

  /**
   * Read the stored keys. Keys found in the plain file are newer than sealed ones (an older version or a run without the
   * carrier wrote them); they are sealed and removed from the plain file.
   * @returns Keys by credential name; values that cannot be opened are left out.
   */
  async read(): Promise<Record<string, string>> {
    const plainRaw = await readJson(this.plainFile)
    const plain = plainRaw === undefined ? {} : plainSchema.parse(plainRaw).keys
    const sealedRaw = await readJson(this.sealedFile)
    if (this.sealer === undefined) {
      // Only the carrier's key opens the sealed file; it stays untouched for a run that has the key.
      const sealed = sealedRaw === undefined ? undefined : sealedSchema.safeParse(sealedRaw)
      this.unopened = sealed?.success === true ? Object.keys(sealed.data.keys).filter(name => !(name in plain)) : []
      return plain
    }
    const file = sealedRaw === undefined ? undefined : sealedSchema.parse(sealedRaw)
    const keys: Record<string, string> = {}
    this.unreadable = {}
    this.setAside = file?.unreadable ?? {}
    for (const [name, value] of Object.entries(file?.keys ?? {})) {
      const opened = this.sealer.open(name, value)
      if (opened === undefined) this.unreadable[name] = value
      else keys[name] = opened
    }
    if (Object.keys(plain).length === 0) return keys
    const merged = { ...keys, ...plain }
    await this.write(merged)
    return merged
  }

  /**
   * Store the complete set of keys.
   * @param keys Keys by credential name.
   * @param removed Names the user removed; an unreadable sealed value under such a name is dropped too.
   */
  async write(keys: Record<string, string>, removed: readonly string[] = []): Promise<void> {
    if (this.sealer === undefined) {
      await writeJson(this.plainFile, { version: 1, keys })
      this.unopened = this.unopened.filter(name => !(name in keys) && !removed.includes(name))
      return
    }
    const sealer = this.sealer
    const kept = Object.fromEntries(Object.entries(this.unreadable).filter(([name]) => !(name in keys) && !removed.includes(name)))
    const setAside = { ...this.setAside }
    for (const [name, value] of Object.entries(this.unreadable)) {
      if (!(name in kept)) setAside[name] = [...(setAside[name] ?? []).filter(item => item !== value), value].slice(-MAX_SET_ASIDE)
    }
    const sealed = Object.fromEntries(Object.entries(keys).map(([name, value]) => [name, sealer.seal(name, value)]))
    // Sealed first: an interrupted write leaves the plain key in place, and the next start seals it again.
    await writeJson(this.sealedFile, { version: 2, keys: { ...kept, ...sealed }, ...Object.keys(setAside).length === 0 ? {} : { unreadable: setAside } })
    await writeJson(this.plainFile, { version: 1, keys: {} })
    this.unreadable = kept
    this.setAside = setAside
  }
}
