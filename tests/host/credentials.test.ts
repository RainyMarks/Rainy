/** API keys at rest: sealed under the carrier's key, migrated from the plain file, and safe for an older version to read. */
import { randomBytes } from 'node:crypto'
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { afterEach, describe, expect, it } from 'vitest'
import { CredentialFiles, credentialSeal } from '../../src/host/credentials.ts'
import { credentialKey, type SafeStorage } from '../../src/main/credential-key.ts'
import { Settings } from '../../src/host/settings.ts'
import { followLocale } from '../../src/shared/locale.ts'

const homes: string[] = []
afterEach(async () => { for (const home of homes.splice(0)) await rm(home, { recursive: true, force: true }) })

async function home(): Promise<string> {
  const directory = await mkdtemp(join(tmpdir(), 'rainy-credentials-'))
  homes.push(directory)
  return directory
}

const json = async (path: string): Promise<unknown> => JSON.parse(await readFile(path, 'utf8'))

describe('credential seal', () => {
  it('opens only with the same key and name, and refuses changed values', () => {
    const key = randomBytes(32)
    const seal = credentialSeal(key)
    const sealed = seal.seal('DEEPSEEK_API_KEY', 'sk-test')
    expect(sealed).not.toContain('sk-test')
    expect(seal.open('DEEPSEEK_API_KEY', sealed)).toBe('sk-test')
    expect(seal.open('ANTHROPIC_API_KEY', sealed)).toBeUndefined()
    expect(credentialSeal(randomBytes(32)).open('DEEPSEEK_API_KEY', sealed)).toBeUndefined()
    const changed = Buffer.from(sealed, 'base64')
    changed[changed.length - 1]! ^= 1
    expect(seal.open('DEEPSEEK_API_KEY', changed.toString('base64'))).toBeUndefined()
    expect(seal.open('DEEPSEEK_API_KEY', 'AAAA')).toBeUndefined()
    expect(() => credentialSeal(randomBytes(16))).toThrow()
  })
})

describe('credential files', () => {
  it('seals keys from the plain file and leaves a plain file an older version reads as empty', async () => {
    const directory = await home()
    await writeFile(join(directory, '.credentials.json'), JSON.stringify({ version: 1, keys: { DEEPSEEK_API_KEY: 'sk-old' } }))
    const seal = credentialSeal(randomBytes(32))
    const files = new CredentialFiles(directory, seal)
    expect(await files.read()).toEqual({ DEEPSEEK_API_KEY: 'sk-old' })
    expect(await json(join(directory, '.credentials.json'))).toEqual({ version: 1, keys: {} })
    const text = await readFile(join(directory, '.credentials.sealed.json'), 'utf8')
    expect(text).not.toContain('sk-old')
    expect(await new CredentialFiles(directory, seal).read()).toEqual({ DEEPSEEK_API_KEY: 'sk-old' })
  })

  it('keeps values another key sealed until their name is stored or removed', async () => {
    const directory = await home()
    await new CredentialFiles(directory, credentialSeal(randomBytes(32))).write({ A_KEY: 'a', B_KEY: 'b' })
    const files = new CredentialFiles(directory, credentialSeal(randomBytes(32)))
    expect(await files.read()).toEqual({})
    await files.write({ A_KEY: 'new a' })
    expect(Object.keys((await json(join(directory, '.credentials.sealed.json')) as { keys: object }).keys).sort()).toEqual(['A_KEY', 'B_KEY'])
    await files.write({ A_KEY: 'new a' }, ['B_KEY'])
    expect(Object.keys((await json(join(directory, '.credentials.sealed.json')) as { keys: object }).keys)).toEqual(['A_KEY'])
    expect(await files.read()).toEqual({ A_KEY: 'new a' })
  })

  it('never deletes a value it cannot open: storing or removing its name sets it aside in the sealed file', async () => {
    const directory = await home()
    const sealedFile = join(directory, '.credentials.sealed.json')
    await new CredentialFiles(directory, credentialSeal(randomBytes(32))).write({ A_KEY: 'a', B_KEY: 'b' })
    const original = (await json(sealedFile) as { keys: Record<string, string> }).keys
    const seal = credentialSeal(randomBytes(32))
    const files = new CredentialFiles(directory, seal)
    expect(await files.read()).toEqual({})
    expect(files.unreadableNames.sort()).toEqual(['A_KEY', 'B_KEY'])
    await files.write({ A_KEY: 'new a' })
    expect(files.unreadableNames).toEqual(['B_KEY'])
    await files.write({ A_KEY: 'new a' }, ['B_KEY'])
    expect(files.unreadableNames).toEqual([])
    const setAside = { A_KEY: [original.A_KEY], B_KEY: [original.B_KEY] }
    expect(await json(sealedFile)).toMatchObject({ unreadable: setAside })
    const later = new CredentialFiles(directory, seal)
    expect(await later.read()).toEqual({ A_KEY: 'new a' })
    expect(later.unreadableNames).toEqual([])
    await later.write({ A_KEY: 'newer a' })
    expect(await json(sealedFile)).toMatchObject({ unreadable: setAside })
  })

  it('names the sealed keys it cannot open without the carrier and leaves the sealed file alone', async () => {
    const directory = await home()
    const sealedFile = join(directory, '.credentials.sealed.json')
    await new CredentialFiles(directory, credentialSeal(randomBytes(32))).write({ A_KEY: 'a', B_KEY: 'b' })
    const before = await readFile(sealedFile, 'utf8')
    const plain = new CredentialFiles(directory, undefined)
    expect(await plain.read()).toEqual({})
    expect(plain.unreadableNames.sort()).toEqual(['A_KEY', 'B_KEY'])
    await plain.write({ A_KEY: 'typed' }, ['B_KEY'])
    expect(plain.unreadableNames).toEqual([])
    expect(await readFile(sealedFile, 'utf8')).toBe(before)
  })

  it('keeps keys in the plain file without the carrier and takes newer plain keys over sealed ones', async () => {
    const directory = await home()
    const seal = credentialSeal(randomBytes(32))
    await new CredentialFiles(directory, seal).write({ A_KEY: 'sealed' })
    const plain = new CredentialFiles(directory, undefined)
    expect(await plain.read()).toEqual({})
    await plain.write({ A_KEY: 'typed later' })
    expect(await json(join(directory, '.credentials.json'))).toEqual({ version: 1, keys: { A_KEY: 'typed later' } })
    expect(await new CredentialFiles(directory, seal).read()).toEqual({ A_KEY: 'typed later' })
  })
})

describe('carrier credential key', () => {
  /** A stand-in for DPAPI: reversible, and able to refuse like an account whose protection keys were reset. */
  function storage(available = true): SafeStorage & { broken: boolean } {
    return {
      broken: false,
      isEncryptionAvailable: () => available,
      encryptString: (text) => Buffer.from(`sealed:${text}`),
      decryptString(data) {
        if (this.broken) throw new Error('Error while decrypting the ciphertext provided to safeStorage.decryptString.')
        return data.toString().slice('sealed:'.length)
      },
    }
  }

  it('creates the key once, reads it back, and replaces a key Windows can no longer open', async () => {
    const directory = await home()
    const lines: string[] = []
    const protection = storage()
    const key = await credentialKey(directory, protection, (line) => { lines.push(line) })
    expect(Buffer.from(key ?? '', 'base64')).toHaveLength(32)
    expect(await readFile(join(directory, 'credential-key.bin'), 'utf8')).toBe(`sealed:${key}`)
    expect(await credentialKey(directory, protection, () => {})).toBe(key)
    protection.broken = true
    const replaced = await credentialKey(directory, protection, (line) => { lines.push(line) })
    expect(replaced).not.toBe(key)
    expect(lines.some(line => line.includes('credential-key.unreadable-'))).toBe(true)
    expect(await credentialKey(directory, storage(false), (line) => { lines.push(line) })).toBeNull()
  })
})

describe('settings with sealed keys', () => {
  it('seals keys an earlier run stored as plain text, reads them back and removes them', async () => {
    const directory = await home()
    const plain = new Settings(directory, {})
    await plain.load()
    await plain.setApiKey('rainy-deepseek', 'sk-plain')
    expect(plain.keysSealed).toBe(false)
    const seal = credentialSeal(randomBytes(32))
    const sealed = new Settings(directory, {}, seal)
    await sealed.load()
    expect(sealed.keysSealed).toBe(true)
    expect(sealed.apiKey('rainy-deepseek')).toBe('sk-plain')
    expect(await readFile(join(directory, '.credentials.json'), 'utf8')).not.toContain('sk-plain')
    await sealed.setApiKey('rainy-deepseek', undefined)
    const reloaded = new Settings(directory, {}, seal)
    await reloaded.load()
    expect(reloaded.apiKey('rainy-deepseek')).toBeUndefined()
  })

  it('explains keys it cannot open in the interface language until they are entered again', async () => {
    const directory = await home()
    const before = new Settings(directory, {}, credentialSeal(randomBytes(32)))
    await before.load()
    await before.setApiKey('rainy-claude', 'sk-ant')
    await before.setApiKey('local-llm', 'sk-local')
    await before.setApiKey('rainy-deepseek', 'sk-deepseek')
    const settings = new Settings(directory, { DEEPSEEK_API_KEY: 'sk-from-environment' }, credentialSeal(randomBytes(32)))
    await settings.load()
    expect(settings.unreadableKeys().sort()).toEqual(['ANTHROPIC_API_KEY', 'RAINY_LOCAL_LLM_KEY'])
    expect(settings.unreadableKeysMessage()).toBe('已保存的 Claude、local-llm API 密钥无法用当前的 Windows 数据保护密钥解开（例如重置过 Windows 账户密码后），请重新填写。原来的加密内容仍保留在 .credentials.sealed.json 中。')
    followLocale(() => 'en')
    try {
      expect(settings.unreadableKeysMessage()).toContain('The saved API keys for Claude, local-llm cannot be opened')
    } finally { followLocale(() => 'zh') }
    await settings.setApiKey('rainy-claude', 'sk-ant-again')
    await settings.setApiKey('local-llm', undefined)
    expect(settings.unreadableKeysMessage()).toBeUndefined()
  })

  it('keeps stored keys when settings.json is missing at startup', async () => {
    const directory = await home()
    const seal = credentialSeal(randomBytes(32))
    await new CredentialFiles(directory, credentialSeal(randomBytes(32))).write({ ANTHROPIC_API_KEY: 'unreadable' })
    const files = new CredentialFiles(directory, seal)
    await files.read()
    await files.write({ DEEPSEEK_API_KEY: 'sk-readable' })
    const settings = new Settings(directory, {}, seal)
    await settings.load()
    expect(settings.apiKey('rainy-deepseek')).toBe('sk-readable')
    expect(settings.unreadableKeys()).toEqual(['ANTHROPIC_API_KEY'])
    expect(Object.keys((await json(join(directory, '.credentials.sealed.json')) as { keys: object }).keys).sort())
      .toEqual(['ANTHROPIC_API_KEY', 'DEEPSEEK_API_KEY'])
  })
})
