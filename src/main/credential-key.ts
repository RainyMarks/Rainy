/**
 * The key that encrypts API keys in the Host's credential file (`src/host/credentials.ts`). It is stored in the user
 * data directory, itself encrypted with Electron's safeStorage (Windows DPAPI, bound to the Windows account), and sent
 * to the Host on its control channel, never through the environment that the Host's commands inherit.
 */
import { randomBytes } from 'node:crypto'
import { readFile, rename, writeFile } from 'node:fs/promises'
import { join } from 'node:path'

/** The parts of Electron's `safeStorage` this module uses. */
export interface SafeStorage {
  isEncryptionAvailable(): boolean
  encryptString(text: string): Buffer
  decryptString(data: Buffer): string
}

const KEY_FILE = 'credential-key.bin'
const KEY_PATTERN = /^[A-Za-z0-9+/]{43}=$/u

/**
 * Read the credential key, creating it on first use.
 * @param userData Electron user data directory.
 * @param storage Electron's safeStorage.
 * @param log Writes one line to the carrier log.
 * @returns The base64 key, or `null` when the system cannot protect it; the Host then keeps keys unencrypted.
 */
export async function credentialKey(userData: string, storage: SafeStorage, log: (line: string) => void): Promise<string | null> {
  if (!storage.isEncryptionAvailable()) {
    log('API keys are stored unencrypted: Windows data protection is unavailable.')
    return null
  }
  const path = join(userData, KEY_FILE)
  let stored: Buffer | undefined
  try { stored = await readFile(path) } catch (error) {
    if ((error as NodeJS.ErrnoException).code !== 'ENOENT') throw error
  }
  if (stored !== undefined) {
    let key: string | undefined
    try { key = storage.decryptString(stored) } catch (_unreadable) { key = undefined }
    if (key !== undefined && KEY_PATTERN.test(key)) return key
    // Windows can no longer open it, for example after an administrator reset the account password. The file is kept
    // for diagnosis; API keys stored with it have to be entered again.
    const aside = join(userData, `credential-key.unreadable-${Date.now()}.bin`)
    await rename(path, aside)
    log(`The credential key could not be read and was moved to ${aside}; stored API keys must be entered again.`)
  }
  const key = randomBytes(32).toString('base64')
  const temporary = `${path}.${randomBytes(8).toString('hex')}.tmp`
  await writeFile(temporary, storage.encryptString(key), { mode: 0o600, flag: 'wx' })
  await rename(temporary, path)
  return key
}
