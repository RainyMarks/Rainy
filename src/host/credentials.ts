/**
 * API keys at rest. With the Electron carrier, `.credentials.sealed.json` holds each key encrypted with AES-256-GCM
 * under a key the carrier keeps with Windows DPAPI (`src/main/credential-key.ts`) and sends over the control channel.
 * `.credentials.json` stays a version 1 file: empty once its keys are sealed, so an older RainyAgent starts without
 * keys instead of failing, and any key it stores there is sealed on the next start. Without the carrier (development
 * runs) keys stay in `.credentials.json` as before.
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
const sealedSchema = z.object({ version: z.literal(2), keys: z.record(z.string(), z.string()) })

/** The credential files of one Host home. */
export class CredentialFiles {
  private readonly plainFile: string
  private readonly sealedFile: string
  /** Sealed values this key cannot open; kept in the file until their name is stored again. */
  private unreadable: Record<string, string> = {}

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

  /**
   * Read the stored keys. Keys found in the plain file are newer than sealed ones (an older version or a run without the
   * carrier wrote them); they are sealed and removed from the plain file.
   * @returns Keys by credential name; values that cannot be opened are left out.
   */
  async read(): Promise<Record<string, string>> {
    const plainRaw = await readJson(this.plainFile)
    const plain = plainRaw === undefined ? {} : plainSchema.parse(plainRaw).keys
    if (this.sealer === undefined) return plain
    const sealedRaw = await readJson(this.sealedFile)
    const keys: Record<string, string> = {}
    this.unreadable = {}
    for (const [name, value] of Object.entries(sealedRaw === undefined ? {} : sealedSchema.parse(sealedRaw).keys)) {
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
      return
    }
    const sealer = this.sealer
    const kept = Object.fromEntries(Object.entries(this.unreadable).filter(([name]) => !(name in keys) && !removed.includes(name)))
    const sealed = Object.fromEntries(Object.entries(keys).map(([name, value]) => [name, sealer.seal(name, value)]))
    // Sealed first: an interrupted write leaves the plain key in place, and the next start seals it again.
    await writeJson(this.sealedFile, { version: 2, keys: { ...kept, ...sealed } })
    await writeJson(this.plainFile, { version: 1, keys: {} })
    this.unreadable = kept
  }
}
