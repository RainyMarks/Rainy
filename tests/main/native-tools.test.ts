import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { mkdtemp, mkdir, readFile, realpath, rm, symlink, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join, relative } from 'node:path'
import { createHash } from 'node:crypto'
import { execFile } from 'node:child_process'
import { promisify } from 'node:util'
import { NativeToolsLibrary, parseNativeFavorites, parseNativeLaunch, resolveNativePath, type NativeInvocation, type NativeToolsInventorySource } from '../../src/main/native-tools.ts'
import { nativeConsoleCommand, nativeConsoleLauncher, nativeToolEnvironment } from '../../src/main/native-tool-process.ts'
import { serveNativeTool, type NativeToolWebPage } from '../../src/main/native-tool-web.ts'
import { windowsPowerShellPath } from '../../src/main/powershell.ts'

let fixture: string
let installRoot: string
let userData: string
const pages: NativeToolWebPage[] = []

beforeEach(async () => {
  fixture = await mkdtemp(join(tmpdir(), 'rainy-native-tools-'))
  installRoot = join(fixture, '应用 Rainy')
  userData = join(fixture, 'user')
  await mkdir(join(installRoot, 'tools/7zip'), { recursive: true })
  await writeFile(join(installRoot, 'tools/7zip/7z.exe'), 'fixture')
  await mkdir(userData, { recursive: true })
})

afterEach(async () => {
  await Promise.all(pages.splice(0).map(page => page.close()))
  if (relative(tmpdir(), fixture).startsWith('..')) throw new Error('Fixture cleanup escaped the temporary directory')
  await rm(fixture, { recursive: true, force: true })
})

function installedTool() {
  return { id: '7zip', category: 'misc', name: '7-Zip', version: 'test', roots: ['tools/7zip'],
    entry: { kind: 'console', path: 'tools/7zip/7z.exe', cwd: 'tools/7zip', args: ['--help'] } }
}

async function catalog(tools: unknown[] = [installedTool()]): Promise<string> {
  const content = `${JSON.stringify({ version: 1, tools })}\n`
  await writeFile(join(installRoot, 'tools/manifest.json'), content)
  return content
}

describe('native tool catalog', () => {
  it('keeps a personally installed IDA separate from the downloadable tool pack', async () => {
    await catalog([{ ...installedTool(), id: 'ida', name: 'IDA', version: 'old', roots: ['tools/ida'],
      entry: { kind: 'gui', path: 'tools/ida/ida64.exe', cwd: 'tools/ida', args: [] } }])
    const root = join(fixture, 'personal')
    await mkdir(join(root, 'tools/ida'), { recursive: true })
    await writeFile(join(root, 'tools/ida/ida.exe'), 'personal licensed program')
    await writeFile(join(userData, 'native-tools.local.json'), JSON.stringify({ version: 1, root,
      catalog: { version: 1, tools: [{ id: 'ida', name: 'IDA Pro 9.5', version: '9.5', category: 'reverse',
        roots: ['tools/ida'], entry: { kind: 'gui', path: 'tools/ida/ida.exe', cwd: 'tools/ida', args: [] } }] } }))
    const start = vi.fn()
    const library = new NativeToolsLibrary({ installRoot, userData, start })
    expect((await library.listTools()).tools).toHaveLength(1)
    expect((await library.listTools()).tools[0]).toMatchObject({ name: 'IDA Pro 9.5', version: '9.5', status: 'ready', verified: false })
    await library.launchTool('ida')
    expect(start.mock.calls[0][0]).toMatchObject({ target: await realpath(join(root, 'tools/ida/ida.exe')) })
    await rm(join(root, 'tools/ida/ida.exe'))
    expect((await library.listTools()).tools[0]).toMatchObject({ status: 'missing' })
    expect(await library.launchTool('ida')).toMatchObject({ ok: false })
  })
  it('lists tools not yet downloaded from the newest catalog and launches installed ones with their own entries', async () => {
    const onlineRoot = join(userData, 'native-tools')
    const newest = JSON.stringify({ version: 1, tools: [{ ...installedTool(), version: 'next' }] })
    let inventory: NativeToolsInventorySource = { root: onlineRoot, catalogText: newest, catalogOutdated: false, updateBytes: 0,
      states: new Map([['7zip', { installed: false, outdated: false, downloadBytes: 42 }]]) }
    const start = vi.fn()
    const library = new NativeToolsLibrary({ installRoot, userData, inventory: async () => inventory, start })
    expect((await library.listTools()).tools).toMatchObject([{ id: '7zip', status: 'available', version: 'next', downloadBytes: 42, missing: [] }])
    expect(await library.launchTool('7zip')).toMatchObject({ ok: false })
    expect(start).not.toHaveBeenCalled()
    await mkdir(join(onlineRoot, 'tools/7zip'), { recursive: true })
    await writeFile(join(onlineRoot, 'tools/7zip/7z.exe'), 'downloaded tool')
    inventory = { ...inventory, installedCatalogText: JSON.stringify({ version: 1, tools: [installedTool()] }),
      catalogOutdated: true, updateBytes: 9,
      states: new Map([['7zip', { installed: true, outdated: true, downloadBytes: 7 }]]) }
    expect(await library.listTools()).toMatchObject({ catalogOutdated: true, updateBytes: 9,
      tools: [{ id: '7zip', status: 'ready', version: 'test', outdated: true, downloadBytes: 7 }] })
    await library.launchTool('7zip')
    expect(start.mock.calls[0][0]).toMatchObject({ target: await realpath(join(onlineRoot, 'tools/7zip/7z.exe')) })
  })

  it('reads the carrier catalog when the installation has none', async () => {
    const catalogPath = join(fixture, 'bundled-catalog.json')
    await writeFile(catalogPath, JSON.stringify({ version: 1, tools: [installedTool()] }))
    const library = new NativeToolsLibrary({ installRoot, userData, catalogPath, start: vi.fn() })
    expect((await library.listTools()).tools).toMatchObject([{ id: '7zip', status: 'ready', outdated: false, downloadBytes: 0 }])
  })
  it('separates installed dependencies from functional acceptance and invalidates stale acceptance', async () => {
    const text = await catalog()
    const library = new NativeToolsLibrary({ installRoot, userData, start: vi.fn() })
    expect((await library.listTools()).tools[0]).toMatchObject({ status: 'ready', verified: false })
    await writeFile(join(installRoot, 'tools/verified.json'), JSON.stringify({ version: 1,
      catalogSha256: createHash('sha256').update(text).digest('hex'), tools: ['7zip'] }))
    expect((await library.listTools()).tools[0]?.verified).toBe(true)
    await catalog([{ ...installedTool(), version: 'next' }])
    expect((await library.listTools()).tools[0]?.verified).toBe(false)
  })

  it('rejects invalid identity syntax, launch arguments, and variants at the wire boundary', () => {
    expect(parseNativeLaunch({ id: 'x64dbg', variant: 'x32' })).toEqual({ id: 'x64dbg', variant: 'x32' })
    expect(() => parseNativeLaunch({ id: '7zip', args: ['arbitrary'] })).toThrow()
    expect(() => parseNativeLaunch({ id: '7zip', variant: 'x32' })).toThrow()
    expect(() => parseNativeLaunch({ id: '../outside' })).toThrow()
    expect(parseNativeFavorites(['7zip', '7zip', 'die'])).toEqual(['7zip', 'die'])
    expect(() => parseNativeFavorites(['../outside'])).toThrow()
  })

  it('rejects duplicate tool IDs and file entries outside their declared tool directory', async () => {
    const library = new NativeToolsLibrary({ installRoot, userData, start: vi.fn() })
    await catalog([installedTool(), installedTool()])
    await expect(library.listTools()).rejects.toThrow('Duplicate tool')
    await catalog([{ ...installedTool(), entry: { ...installedTool().entry, path: '../outside.exe' } }])
    await expect(library.listTools()).rejects.toThrow('installation-relative')
    await catalog([{ ...installedTool(), entry: { ...installedTool().entry, cwd: 'tools/7zip-other' } }])
    await expect(library.listTools()).rejects.toThrow('Invalid installed entry')
  })

  it('keeps the source directory and another installation out of launch resolution', async () => {
    await mkdir(join(fixture, 'outside'), { recursive: true })
    await writeFile(join(fixture, 'outside/tool.exe'), 'outside')
    await symlink(join(fixture, 'outside'), join(installRoot, 'tools/7zip/linked'), process.platform === 'win32' ? 'junction' : 'dir')
    await expect(resolveNativePath(installRoot, 'tools/7zip/linked/tool.exe')).rejects.toThrow('超出安装目录')
    await expect(resolveNativePath(installRoot, 'C:/elsewhere/tool.exe')).rejects.toThrow()
    await expect(resolveNativePath(installRoot, 'tools/7zip/../outside.exe')).rejects.toThrow()
  })

  it('retains missing entries in the list and never calls the native launcher for them', async () => {
    await catalog()
    await rm(join(installRoot, 'tools/7zip/7z.exe'))
    const start = vi.fn()
    const library = new NativeToolsLibrary({ installRoot, userData, start })
    expect((await library.listTools()).tools[0]).toMatchObject({ status: 'missing', missing: ['tools/7zip/7z.exe'] })
    expect(await library.launchTool('7zip')).toMatchObject({ ok: false })
    expect(start).not.toHaveBeenCalled()
  })

  it('coalesces overlapping starts and preserves concurrent favorite edits with recent launches', async () => {
    await catalog()
    const created = Promise.withResolvers<undefined>()
    const release = Promise.withResolvers<undefined>()
    const start = vi.fn(async (invocation: NativeInvocation) => {
      expect(invocation.executable).toBe(await resolveNativePath(installRoot, 'tools/7zip/7z.exe'))
      created.resolve(undefined)
      await release.promise
    })
    const library = new NativeToolsLibrary({ installRoot, userData, start })
    const first = library.launchTool('7zip')
    await created.promise
    const second = library.launchTool('7zip')
    expect(second).toBe(first)
    await library.setFavorites(['7zip'])
    release.resolve(undefined)
    expect(await first).toEqual({ ok: true })
    expect(start).toHaveBeenCalledOnce()
    const reloaded = new NativeToolsLibrary({ installRoot, userData, start })
    expect((await reloaded.listTools()).preferences).toEqual({ favorites: ['7zip'], recent: ['7zip'] })
  })

  it('does not record a native startup failure as recent use', async () => {
    await catalog()
    const library = new NativeToolsLibrary({ installRoot, userData, start: async () => { throw new Error('system refused') } })
    expect(await library.launchTool('7zip')).toEqual({ ok: false, error: 'system refused' })
    expect((await library.listTools()).preferences.recent).toEqual([])
  })

  it('reports an opened tool independently from an unavailable preference destination', async () => {
    await catalog()
    await writeFile(join(fixture, 'not-a-directory'), 'file')
    const start = vi.fn(async () => {})
    const library = new NativeToolsLibrary({ installRoot, userData: join(fixture, 'not-a-directory'), start })
    const result = await library.launchTool('7zip')
    expect(start).toHaveBeenCalledOnce()
    expect(result.ok).toBe(true)
    expect(result.warning).toContain('最近使用记录未保存')
  })

  it('retains malformed user preferences instead of replacing them with an empty list', async () => {
    await catalog()
    const path = join(userData, 'native-tools.json')
    await writeFile(path, '{broken')
    const library = new NativeToolsLibrary({ installRoot, userData, start: vi.fn() })
    await expect(library.setFavorites(['7zip'])).rejects.toThrow()
    expect(await readFile(path, 'utf8')).toBe('{broken')
  })

  it('migrates Burp favorites to Yakit without inventing recent Yakit launches', async () => {
    await catalog()
    const path = join(userData, 'native-tools.json')
    const previous = JSON.stringify({ version: 1, favorites: ['burp-community', '7zip', 'yakit'], recent: ['burp-community', '7zip'] })
    await writeFile(path, previous)
    const library = new NativeToolsLibrary({ installRoot, userData, start: vi.fn() })
    expect((await library.listTools()).preferences).toEqual({ favorites: ['yakit', '7zip'], recent: ['7zip'] })
    expect(await readFile(path, 'utf8')).toBe(previous)
    await library.setFavorites(['yakit', '7zip'])
    expect(JSON.parse(await readFile(path, 'utf8'))).toEqual({ version: 1, favorites: ['yakit', '7zip'], recent: ['7zip'] })
    expect(() => parseNativeLaunch({ id: 'burp-community' })).toThrow()
    expect(() => parseNativeFavorites(['burp-community'])).toThrow()
  })

  it('reads an older catalog during repair while hiding the retired Burp entry', async () => {
    await catalog([installedTool(), { id: 'burp-community', category: 'web', name: 'Burp Suite Community', version: 'old',
      roots: ['tools/burp-community'], entry: { kind: 'gui', path: 'tools/burp-community/app.exe', cwd: 'tools/burp-community' } }])
    const library = new NativeToolsLibrary({ installRoot, userData, start: vi.fn() })
    expect((await library.listTools()).tools.map(tool => tool.id)).toEqual(['7zip'])
  })

  it('reports missing bundled entry dependencies before accepting a launch', async () => {
    await catalog([{ ...installedTool(), entry: { ...installedTool().entry, requiredFiles: ['tools/7zip/7z.dll'] } }])
    const start = vi.fn()
    const library = new NativeToolsLibrary({ installRoot, userData, start })
    expect((await library.listTools()).tools[0]).toMatchObject({ status: 'missing', missing: ['tools/7zip/7z.dll'] })
    expect((await library.launchTool('7zip')).ok).toBe(false)
    expect(start).not.toHaveBeenCalled()
    await writeFile(join(installRoot, 'tools/7zip/7z.dll'), 'fixture dependency')
    expect((await library.listTools()).tools[0]?.status).toBe('ready')
    await catalog([{ ...installedTool(), entry: { ...installedTool().entry, requiredFiles: ['tools/unowned/runtime.dll'] } }])
    await expect(library.listTools()).rejects.toThrow('outside the installed roots')
  })
})

describe('native process preparation', () => {
  function invocation(): NativeInvocation {
    return { id: '7zip', name: '7-Zip', kind: 'console', target: "C:\\应用 O'Brien\\7z.exe", executable: "C:\\应用 O'Brien\\7z.exe",
      cwd: "C:\\应用 O'Brien", args: ["literal'quote", '$(not-code)', '& unchanged'], roots: [], userData: 'C:\\test-user-data' }
  }

  it('quotes console paths and arguments as literals rather than PowerShell expressions', () => {
    const text = Buffer.from(nativeConsoleCommand(invocation()), 'base64').toString('utf16le')
    expect(text).toContain("'C:\\应用 O''Brien\\7z.exe'")
    expect(text).toContain("'literal''quote' '$(not-code)' '& unchanged'")
  })

  it('doubles typographic single quotes, which PowerShell also accepts as string delimiters', () => {
    const value = 'x‘; Write-Output injected; ’y'
    const text = Buffer.from(nativeConsoleCommand({ ...invocation(), cwd: 'C:\\O’Brien', args: [value] }), 'base64').toString('utf16le')
    expect(text).toContain("Set-Location -LiteralPath 'C:\\O’’Brien'")
    expect(text).toContain("'x‘‘; Write-Output injected; ’’y'")
  })

  it.runIf(process.platform === 'win32')('parses typographic quotes in launch values as literal PowerShell strings', async () => {
    const values = ['x‘; Write-Output injected; ’y', '‚‛\'\'', 'O’Brien']
    const command = Buffer.from(nativeConsoleCommand({ ...invocation(), args: values }), 'base64').toString('utf16le')
    const script = `[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
    $ast = [Management.Automation.Language.Parser]::ParseInput([Text.Encoding]::Unicode.GetString([Convert]::FromBase64String('${Buffer.from(command, 'utf16le').toString('base64')}')), [ref]$null, [ref]$null)
    $call = $ast.FindAll({ param($node) $node -is [Management.Automation.Language.CommandAst] -and $node.InvocationOperator -eq 'Ampersand' }, $true)
    ConvertTo-Json -Compress -InputObject @($call | ForEach-Object { $_.CommandElements | ForEach-Object { $_.Value } })`
    const powershell = windowsPowerShellPath()
    const result = await promisify(execFile)(powershell, ['-NoLogo', '-NoProfile', '-NonInteractive', '-EncodedCommand',
      Buffer.from(script, 'utf16le').toString('base64')], { windowsHide: true, timeout: 25000, encoding: 'utf8' })
    expect(JSON.parse(result.stdout)).toEqual([invocation().executable, ...values])
  })

  it('keeps the console command encoded and requests its own interactive window', () => {
    const powershell = "C:\\system O'Brien\\powershell.exe"
    const text = Buffer.from(nativeConsoleLauncher(invocation(), powershell), 'base64').toString('utf16le')
    expect(text).toContain("-FilePath 'C:\\system O''Brien\\powershell.exe'")
    expect(text).toContain("-WorkingDirectory 'C:\\应用 O''Brien'")
    expect(text).toContain(`@('-NoLogo', '-NoProfile', '-NoExit', '-EncodedCommand', '${nativeConsoleCommand(invocation())}')`)
    expect(text).not.toMatch(/-NoNewWindow|-RedirectStandard|-NonInteractive/)
  })

  it.runIf(process.platform === 'win32')('passes literal launch values through Windows PowerShell without starting a tool', async () => {
    const powershell = windowsPowerShellPath()
    const launcher = nativeConsoleLauncher(invocation(), powershell)
    const script = `[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
    function Start-Process {
      param([string]$FilePath, [string]$WorkingDirectory, [string[]]$ArgumentList, [string]$ErrorAction)
      [pscustomobject]@{file=$FilePath; directory=$WorkingDirectory; arguments=$ArgumentList} | ConvertTo-Json -Compress
    }
    & ([scriptblock]::Create([Text.Encoding]::Unicode.GetString([Convert]::FromBase64String('${launcher}'))))`
    const result = await promisify(execFile)(powershell, ['-NoLogo', '-NoProfile', '-NonInteractive', '-EncodedCommand',
      Buffer.from(script, 'utf16le').toString('base64')], { windowsHide: true, timeout: 25000, encoding: 'utf8' })
    expect(JSON.parse(result.stdout)).toEqual({ file: powershell, directory: invocation().cwd,
      arguments: ['-NoLogo', '-NoProfile', '-NoExit', '-EncodedCommand', nativeConsoleCommand(invocation())] })
  })

  it('removes service secrets and ambient language overrides while keeping Windows essentials', () => {
    const result = nativeToolEnvironment({ SystemRoot: 'C:\\Windows', Path: 'C:\\system', OPENAI_API_KEY: 'private',
      CUSTOM_TOKEN: 'private', JAVA_HOME: 'wrong', NODE_OPTIONS: '--require=wrong', QT_PLUGIN_PATH: 'wrong',
      QML2_IMPORT_PATH: 'wrong', USERPROFILE: 'C:\\Users\\test' }, invocation())
    expect(result.SystemRoot).toBe('C:\\Windows')
    expect(result.USERPROFILE).toBe('C:\\Users\\test')
    expect(result.OPENAI_API_KEY).toBeUndefined()
    expect(result.CUSTOM_TOKEN).toBeUndefined()
    expect(result.NODE_OPTIONS).toBeUndefined()
    expect(result.JAVA_HOME).toBeUndefined()
    expect(result.QT_PLUGIN_PATH).toBeUndefined()
    expect(result.QML2_IMPORT_PATH).toBeUndefined()
    expect(result.Path).toBeUndefined()
    expect(result.PATH).not.toContain('C:\\system')
    expect(result.PATH).toContain(join('C:\\Windows', 'System32'))
    expect(result.PATH).toContain(invocation().cwd)
  })

  it('sets only the selected private Python and dotnet runtimes', () => {
    const selected: NativeInvocation = { ...invocation(), id: 'ida', dotnetRoot: 'C:\\应用 Rainy\\runtime\\dotnet', pythonRoot: 'C:\\应用 Rainy\\tools\\python' }
    const result = nativeToolEnvironment({ Path: 'C:\\system', PYTHONHOME: 'ambient', PYTHONPATH: 'ambient',
      DOTNET_ROOT: 'ambient', DOTNET_ROOT_X64: 'ambient', DOTNET_MULTILEVEL_LOOKUP: '1', DOTNET_STARTUP_HOOKS: 'ambient',
      IDAUSR: 'ambient', IDADIR: 'ambient', PYTHONUSERBASE: 'ambient', VIRTUAL_ENV: 'ambient', CONDA_PREFIX: 'ambient',
      CLASSPATH: 'ambient', JDK_JAVA_OPTIONS: 'ambient', PERL5LIB: 'ambient', TESSDATA_PREFIX: 'ambient', MAGICK_HOME: 'ambient' }, selected)
    expect(result.DOTNET_ROOT).toBe(selected.dotnetRoot)
    expect(result.DOTNET_ROOT_X64).toBe(selected.dotnetRoot)
    expect(result.DOTNET_MULTILEVEL_LOOKUP).toBe('0')
    expect(result.DOTNET_STARTUP_HOOKS).toBeUndefined()
    expect(result.PYTHONHOME).toBe(selected.pythonRoot)
    expect(result.PYTHONPATH).toBeUndefined()
    expect(result.PYTHONNOUSERSITE).toBe('1')
    expect(result.PYTHONUTF8).toBe('1')
    expect(result.IDAUSR).toBe(selected.userData)
    for (const name of ['IDADIR', 'PYTHONUSERBASE', 'VIRTUAL_ENV', 'CONDA_PREFIX', 'CLASSPATH', 'JDK_JAVA_OPTIONS', 'PERL5LIB', 'TESSDATA_PREFIX', 'MAGICK_HOME']) {
      expect(result[name]).toBeUndefined()
    }
    expect(result.PATH).toContain(selected.pythonRoot)
    expect(result.PATH).toContain(join(selected.pythonRoot!, 'DLLs'))
  })
})

describe('offline tool resources', () => {
  it('owns its dynamic port, serves contained assets, and closes every connection', async () => {
    const folder = join(installRoot, 'tools/cyberchef')
    await mkdir(folder, { recursive: true })
    await writeFile(join(folder, 'index.html'), '<html>tool</html>')
    await writeFile(join(folder, 'asset.js'), 'window.ready=true')
    await writeFile(join(installRoot, 'private.txt'), 'private')
    const page = await serveNativeTool({ id: 'cyberchef', name: 'CyberChef', kind: 'web', target: join(folder, 'index.html'),
      executable: join(folder, 'index.html'), cwd: folder, args: [], roots: [folder], userData })
    pages.push(page)
    const response = await fetch(page.url)
    expect(await response.text()).toBe('<html>tool</html>')
    expect(response.headers.get('content-security-policy')).toContain("frame-ancestors 'none'")
    expect(await (await fetch(new URL('asset.js', page.url))).text()).toBe('window.ready=true')
    expect((await fetch(`${page.origin}/private.txt`)).status).toBe(404)
    expect((await fetch(new URL('%2e%2e%2f%2e%2e%2fprivate.txt', page.url))).status).toBe(404)
    expect((await fetch(page.url, { method: 'POST' })).status).toBe(405)
    await page.close()
    await expect(fetch(page.url)).rejects.toThrow()
  })
})
