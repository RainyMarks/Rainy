import { execFileSync } from 'node:child_process'
import { mkdtemp, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { afterAll, describe, expect, it } from 'vitest'
import { applyShellIntegration, contextMenuLabel, FILE_TYPES, integrationOperations, keptShellKeys, launchCommand, readShellIntegration,
  removableKeys, SHELL_EXTENSIONS, shellIntegrationCurrent, shellIntegrationPreference, syncShellIntegration } from '../../src/main/shell-integration.ts'
import { inspectLaunchPaths, launchPaths } from '../../src/main/launch-paths.ts'

const executable = 'C:\\Program Files\\Rainy\\RainyAgent.exe'

describe('Explorer integration operations', () => {
  it('registers the context menu on files, folders and folder backgrounds with the executable quoted', () => {
    const operations = integrationOperations(executable, 'zh', { contextMenu: true, fileTypes: false })
    const commands = operations.filter(operation => operation.op === 'set' && operation.key.endsWith('\\command'))
    expect(commands).toEqual([
      { op: 'set', key: 'Software\\Classes\\*\\shell\\Rainy\\command', name: '', value: `"${executable}" "%1"` },
      { op: 'set', key: 'Software\\Classes\\Directory\\shell\\Rainy\\command', name: '', value: `"${executable}" "%V"` },
      { op: 'set', key: 'Software\\Classes\\Directory\\Background\\shell\\Rainy\\command', name: '', value: `"${executable}" "%V"` },
    ])
    expect(operations).toContainEqual({ op: 'set', key: 'Software\\Classes\\*\\shell\\Rainy', name: '', value: '通过 Rainy 打开' })
    // File types that are turned off lose only Rainy's value; the keys above it go only when that left them empty.
    expect(operations).toContainEqual({ op: 'removeValue', key: 'Software\\Classes\\.md\\OpenWithProgids', name: 'Rainy.md',
      parents: ['Software\\Classes\\.md'] })
    expect(operations.some(operation => operation.op === 'removeKey' && operation.key === 'Software\\Classes\\.md')).toBe(false)
  })

  it('offers Rainy for every text type through OpenWithProgids, SupportedTypes and the Default apps capabilities', () => {
    const operations = integrationOperations(executable, 'en', { contextMenu: false, fileTypes: true })
    expect(operations).toContainEqual({ op: 'removeKey', key: 'Software\\Classes\\*\\shell\\Rainy',
      parents: ['Software\\Classes\\*\\shell', 'Software\\Classes\\*'] })
    expect(operations).toContainEqual({ op: 'removeKey', key: 'Software\\Classes\\Directory\\Background\\shell\\Rainy',
      parents: ['Software\\Classes\\Directory\\Background\\shell', 'Software\\Classes\\Directory\\Background', 'Software\\Classes\\Directory'] })
    for (const extension of ['md', 'txt', 'py', 'tsx']) {
      expect(operations).toContainEqual({ op: 'set', key: `Software\\Classes\\.${extension}\\OpenWithProgids`, name: `Rainy.${extension}`, value: '' })
      expect(operations).toContainEqual({ op: 'set', key: 'Software\\RainyMarks\\Rainy\\Capabilities\\FileAssociations', name: `.${extension}`, value: `Rainy.${extension}` })
      expect(operations).toContainEqual({ op: 'set', key: 'Software\\Classes\\Applications\\RainyAgent.exe\\SupportedTypes', name: `.${extension}`, value: '' })
    }
    expect(operations).toContainEqual({ op: 'set', key: 'Software\\Classes\\Rainy.md', name: '', value: 'Markdown Document' })
    expect(operations).toContainEqual({ op: 'set', key: 'Software\\RegisteredApplications', name: 'Rainy', value: 'Software\\RainyMarks\\Rainy\\Capabilities' })
    // Never the extension's own default: Windows keeps the user's choice.
    expect(operations.some(operation => operation.op === 'set' && /\\\.[a-z0-9]+$/u.test(operation.key) && operation.name === '')).toBe(false)
  })

  it('lists text formats only, each extension once', () => {
    expect(new Set(SHELL_EXTENSIONS).size).toBe(SHELL_EXTENSIONS.length)
    expect(SHELL_EXTENSIONS).toEqual(expect.arrayContaining(['md', 'markdown', 'txt', 'json', 'py', 'c', 'cpp']))
    for (const extension of ['docx', 'pptx', 'pdf', 'exe', 'lnk', 'png']) expect(SHELL_EXTENSIONS).not.toContain(extension)
    for (const entry of FILE_TYPES) expect(entry.zh !== '' && entry.en !== '').toBe(true)
  })

  it('keeps both parts off until the user turns them on, and compares the registry with this installation', () => {
    expect(shellIntegrationPreference({})).toEqual({ contextMenu: false, fileTypes: false })
    // 3.9.0 turned both on without asking; its key is ignored, so an upgrade removes what it wrote.
    expect(shellIntegrationPreference({ shellIntegration: { contextMenu: true, fileTypes: true } })).toEqual({ contextMenu: false, fileTypes: false })
    expect(shellIntegrationPreference({ explorerIntegration: { contextMenu: true } })).toEqual({ contextMenu: true, fileTypes: false })
    expect(shellIntegrationPreference({ explorerIntegration: { fileTypes: 'yes', contextMenu: null } })).toEqual({ contextMenu: false, fileTypes: false })
    const registered = { contextMenuLabel: contextMenuLabel('zh'), contextMenuCommand: launchCommand(executable), fileTypeCommand: launchCommand(executable) }
    expect(shellIntegrationCurrent(registered, executable, 'zh', { contextMenu: true, fileTypes: true })).toBe(true)
    // Another language, a moved installation or a part turned off all need a rewrite.
    expect(shellIntegrationCurrent(registered, executable, 'en', { contextMenu: true, fileTypes: true })).toBe(false)
    expect(shellIntegrationCurrent(registered, 'D:\\Rainy\\RainyAgent.exe', 'zh', { contextMenu: true, fileTypes: true })).toBe(false)
    expect(shellIntegrationCurrent(registered, executable, 'zh', { contextMenu: false, fileTypes: true })).toBe(false)
    expect(shellIntegrationCurrent({ contextMenuLabel: null, contextMenuCommand: null, fileTypeCommand: null }, executable, 'zh',
      { contextMenu: false, fileTypes: false })).toBe(true)
  })
})

describe('launch paths', () => {
  it('takes the paths after the executable and skips switches', () => {
    expect(launchPaths(['C:\\Rainy\\RainyAgent.exe', '--updated', 'C:\\Notes\\a b.md', '--user-data-dir=C:\\x', 'D:\\Project'], 'C:\\', false))
      .toEqual(['C:\\Notes\\a b.md', 'D:\\Project'])
    // A development run's second argument is the app folder; `--` ends the switches.
    expect(launchPaths(['electron.exe', 'C:\\repo', '--', '-notes.md'], 'C:\\Work', true)).toEqual(['C:\\Work\\-notes.md'])
    expect(launchPaths(['RainyAgent.exe', '.'], 'C:\\Users\\me\\project', false)).toEqual(['C:\\Users\\me\\project'])
    expect(launchPaths(['RainyAgent.exe'], 'C:\\', false)).toEqual([])
  })

  it('tells files from folders and reports what is missing', async () => {
    const folder = await mkdtemp(join(tmpdir(), 'rainy-launch-'))
    try {
      await writeFile(join(folder, 'notes.md'), '# Notes\n')
      const result = await inspectLaunchPaths([join(folder, 'notes.md'), folder, join(folder, 'gone.md')])
      expect(result.entries).toEqual([{ path: join(folder, 'notes.md'), kind: 'file' }, { path: folder, kind: 'directory' }])
      expect(result.missing).toEqual([join(folder, 'gone.md')])
    } finally { await rm(folder, { recursive: true, force: true }) }
  })
})

// The registry script runs against a scratch key of the current user and removes it afterwards.
describe.runIf(process.platform === 'win32')('Explorer integration in the registry', () => {
  const root = `Software\\RainyTest-shell-${process.pid}`
  afterAll(() => {
    try { execFileSync('reg.exe', ['delete', `HKCU\\${root}`, '/f'], { windowsHide: true, stdio: 'ignore' }) } catch (_absent) { /* Already removed by the test. */ }
  })

  it('writes, reads back and removes the registration with Unicode intact', async () => {
    const unicode = 'C:\\Users\\雨天\\Rainy\\RainyAgent.exe'
    await applyShellIntegration(unicode, 'zh', { contextMenu: true, fileTypes: true }, root)
    const status = await readShellIntegration(root)
    expect(status).toEqual({ contextMenuLabel: '通过 Rainy 打开', contextMenuCommand: launchCommand(unicode), fileTypeCommand: launchCommand(unicode) })
    expect(shellIntegrationCurrent(status, unicode, 'zh', { contextMenu: true, fileTypes: true })).toBe(true)
    await applyShellIntegration(unicode, 'zh', { contextMenu: false, fileTypes: false }, root)
    expect(await readShellIntegration(root)).toEqual({ contextMenuLabel: null, contextMenuCommand: null, fileTypeCommand: null })
    // Nothing the registration created is left, not even the empty keys above its values.
    const left = execFileSync('reg.exe', ['query', `HKCU\\${root}`, '/s'], { windowsHide: true, encoding: 'utf8' })
    const keys = left.split(/\r?\n/u).filter(line => line.startsWith('HKEY_CURRENT_USER\\')).map(line => line.slice(`HKEY_CURRENT_USER\\${root}`.length))
    expect(keys.sort()).toEqual(['\\Software', '\\Software\\Classes'])
  })

  it('leaves what other applications and the user had: their values, their keys and keys Rainy never wrote to', async () => {
    const own = `${root}-own`
    const add = (key: string, ...value: string[]): void => {
      execFileSync('reg.exe', ['add', `HKCU\\${own}\\Software\\Classes\\${key}`, ...value, '/f'], { windowsHide: true, stdio: 'ignore' })
    }
    try {
      add('.md\\OpenWithProgids', '/v', 'Other.md', '/t', 'REG_SZ', '/d', '')
      add('*\\shell\\Other', '/ve', '/d', 'Other')
      // Removing an integration that was never written touches nothing, not even an empty key of the user.
      add('.text\\OpenWithProgids')
      await applyShellIntegration(executable, 'zh', { contextMenu: false, fileTypes: false }, own)
      const query = (key: string): string => execFileSync('reg.exe', ['query', `HKCU\\${own}\\Software\\Classes\\${key}`], { windowsHide: true, encoding: 'utf8' })
      expect(query('.text\\OpenWithProgids')).toContain('OpenWithProgids')
      await applyShellIntegration(executable, 'zh', { contextMenu: true, fileTypes: true }, own)
      expect(query('.md\\OpenWithProgids')).toContain('Rainy.md')
      await applyShellIntegration(executable, 'zh', { contextMenu: false, fileTypes: false }, own)
      const md = query('.md\\OpenWithProgids')
      expect(md).toContain('Other.md')
      expect(md).not.toContain('Rainy.md')
      expect(query('*\\shell\\Other')).toContain('Other')
      expect(() => query('*\\shell\\Rainy')).toThrow()
    } finally {
      try { execFileSync('reg.exe', ['delete', `HKCU\\${own}`, '/f'], { windowsHide: true, stdio: 'ignore' }) } catch (_absent) { /* Nothing was left. */ }
    }
  })

  it('remembers the keys the user had when a part is turned on, so turning it off leaves them even when empty', async () => {
    const own = `${root}-keep`
    const classes = `HKCU\\${own}\\Software\\Classes`
    const exists = (key: string): boolean => {
      try { execFileSync('reg.exe', ['query', `${classes}\\${key}`], { windowsHide: true, stdio: 'ignore' }); return true } catch (_absent) { return false }
    }
    try {
      execFileSync('reg.exe', ['add', `${classes}\\.text\\OpenWithProgids`, '/f'], { windowsHide: true, stdio: 'ignore' })
      execFileSync('reg.exe', ['add', `${classes}\\Directory\\shell`, '/f'], { windowsHide: true, stdio: 'ignore' })
      const { changed, keep } = await syncShellIntegration(executable, 'zh', { contextMenu: true, fileTypes: true }, [], own)
      expect(changed).toBe(true)
      expect(keep).toContain('software\\classes\\.text\\openwithprogids')
      expect(keep).toContain('software\\classes\\directory\\shell')
      expect(keep).not.toContain('software\\classes\\.md')
      // Already current: nothing is written or looked up again and the list stays.
      expect(await syncShellIntegration(executable, 'zh', { contextMenu: true, fileTypes: true }, keep, own)).toEqual({ changed: false, keep })
      expect(await syncShellIntegration(executable, 'zh', { contextMenu: false, fileTypes: false }, keep, own)).toEqual({ changed: true, keep: [] })
      expect(exists('.text\\OpenWithProgids')).toBe(true)
      expect(exists('Directory\\shell')).toBe(true)
      expect(exists('Directory\\shell\\Rainy')).toBe(false)
      // What Rainy created goes: the .md type key, the Directory\Background branch and the * branch.
      expect(exists('.md')).toBe(false)
      expect(exists('Directory\\Background')).toBe(false)
      expect(exists('*')).toBe(false)
    } finally {
      try { execFileSync('reg.exe', ['delete', `HKCU\\${own}`, '/f'], { windowsHide: true, stdio: 'ignore' }) } catch (_absent) { /* Nothing was left. */ }
    }
  })

  it('stops removing parents at a kept key and keeps a kept key that held only Rainy\'s value', () => {
    const keep = new Set(['software\\classes\\directory', 'software\\classes\\.txt\\openwithprogids'])
    const operations = integrationOperations(executable, 'zh', { contextMenu: false, fileTypes: false }, '', keep)
    expect(operations).toContainEqual({ op: 'removeKey', key: 'Software\\Classes\\Directory\\Background\\shell\\Rainy',
      parents: ['Software\\Classes\\Directory\\Background\\shell', 'Software\\Classes\\Directory\\Background'] })
    expect(operations).toContainEqual({ op: 'removeValue', key: 'Software\\Classes\\.txt\\OpenWithProgids', name: 'Rainy.txt', parents: [], keepKey: true })
    expect(removableKeys({ contextMenu: true, fileTypes: false })).toContain('Software\\Classes\\Directory')
    expect(removableKeys({ contextMenu: false, fileTypes: false })).toEqual([])
    expect(keptShellKeys({ explorerIntegration: { keep: ['a', 1, 'b'] } })).toEqual(['a', 'b'])
    expect(keptShellKeys({ explorerIntegration: true })).toEqual([])
  })
})
