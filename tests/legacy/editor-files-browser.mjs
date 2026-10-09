/** Real Monaco filesystem provider on a Windows browser platform, without a model or Host process. */
import assert from 'node:assert/strict'
import { createRequire } from 'node:module'
import { dirname, join, resolve } from 'node:path'
import { mkdir, writeFile } from 'node:fs/promises'
import { fileURLToPath } from 'node:url'

const appRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const repository = resolve(appRoot, '../..')
const { build } = createRequire(join(appRoot, 'package.json'))('esbuild')
const { chromium } = createRequire(join(repository, 'apps/web/package.json'))('playwright')
const bundle = await build({
  absWorkingDir: appRoot,
  stdin: { resolveDir: appRoot, sourcefile: 'editor-files-browser-fixture.js', contents: String.raw`
    import { RegisteredFileSystemProvider, RegisteredMemoryFile } from '@codingame/monaco-vscode-files-service-override'
    import { URI } from '@codingame/monaco-vscode-api/vscode/vs/base/common/uri'
    import { canonicalEditorUri, containsEditorUri } from './src/editor-assets/editor-uri.ts'
    globalThis.rainyEditorFilesProbe = async () => {
      const results = []
      const check = async (name, run) => {
        const provider = new RegisteredFileSystemProvider(false)
        const files = []
        const register = (uri, text) => files.push(provider.registerFile(new RegisteredMemoryFile(URI.parse(uri), text)))
        try { await run(provider, register); results.push({ name, passed: true }) }
        catch (error) { results.push({ name, passed: false, error: String(error) }) }
        finally { for (const file of files) file.dispose(); provider.dispose() }
      }
      const read = async (provider, uri, expected) => {
        const actual = new TextDecoder().decode(await provider.readFile(URI.parse(uri)))
        if (actual !== expected) throw new Error('Unexpected contents for ' + uri)
      }
      await check('drive root mkdir resolves its URI-tree parent', async provider => {
        provider.mkdirSync(URI.parse('file:///C:'))
        if ((await provider.stat(URI.parse('file:///C:/'))).type !== 2) throw new Error('Drive root is not a directory')
      })
      await check('Windows Unicode and space paths retain independent same-named files', async (provider, register) => {
        const paths = [
          ['file:///C:/项目%20空间/主目录/样例.py', 'primary'],
          ['file:///C:/项目%20空间/附加目录/样例.py', 'attached'],
          ['file:///D:/项目%20空间/主目录/样例.py', 'other drive'],
        ]
        for (const [uri, text] of paths) register(uri, text)
        for (const [uri, text] of paths) await read(provider, uri, text)
      })
      await check('UNC authorities and shares remain separate', async (provider, register) => {
        const paths = [
          ['file://first-server/share/中文%20目录/样例.py', 'first share'],
          ['file://first-server/other-share/中文%20目录/样例.py', 'other share'],
          ['file://second-server/share/中文%20目录/样例.py', 'other server'],
        ]
        for (const [uri, text] of paths) register(uri, text)
        for (const [uri, text] of paths) await read(provider, uri, text)
      })
      await check('POSIX root and nested files retain their original lookup behavior', async (provider, register) => {
        register('file:///root.txt', 'root file')
        register('file:///home/user/中文%20project/nested/main.py', 'nested file')
        await read(provider, 'file:///root.txt', 'root file')
        await read(provider, 'file:///home/user/中文%20project/nested/main.py', 'nested file')
      })
      await check('a file cannot become the parent directory of another file', async (provider, register) => {
        register('file:///C:/project/file', 'existing file')
        let rejected = false
        try { register('file:///C:/project/file/child.py', 'must not register') } catch { rejected = true }
        if (!rejected) throw new Error('A file was accepted as a directory')
        await read(provider, 'file:///C:/project/file', 'existing file')
      })
      await check('Windows input and serialized readback use one canonical file identity', async provider => {
        const input = 'file:///C:/项目%20空间/主目录/样例.py'
        const canonical = canonicalEditorUri(input)
        const serialized = canonical.toString()
        const file = provider.registerFile(new RegisteredMemoryFile(canonical, 'canonical source'))
        try {
          if (canonical.path !== URI.parse(serialized).path) throw new Error('Serialization changed the lookup path')
          await read(provider, serialized, 'canonical source')
          const root = 'C:\\项目 空间\\主目录'
          if (!containsEditorUri(input, root) || !containsEditorUri(serialized, root)) throw new Error('Equivalent input and readback selected different roots')
          if (containsEditorUri(input, 'C:\\项目 空间\\主目录-other')) throw new Error('A sibling root matched')
          if (containsEditorUri(input, 'C:\\项目 空间\\附加目录')) throw new Error('An attached root matched the primary file')
        } finally { file.dispose() }
      })
      await check('UNC URI spelling normalizes authority while POSIX matching remains case-sensitive', async () => {
        const input = 'file://SERVER/share/中文%20目录/main.py'
        const serialized = canonicalEditorUri(input).toString()
        if (!containsEditorUri(input, '\\\\SERVER\\share\\中文 目录') || !containsEditorUri(serialized, '\\\\SERVER\\share\\中文 目录'))
          throw new Error('The UNC authority changed file ownership')
        if (containsEditorUri(input, '\\\\OTHER\\share\\中文 目录')) throw new Error('A different server matched')
        if (!containsEditorUri('file:///home/Project/main.py', '/home/Project')) throw new Error('The POSIX root did not match')
        if (containsEditorUri('file:///home/project/main.py', '/home/Project')) throw new Error('The POSIX root ignored path case')
      })
      return { platform: navigator.platform, userAgent: navigator.userAgent, results, passed: results.every(row => row.passed) }
    }
  ` },
  bundle: true, write: false, format: 'iife', platform: 'browser', target: 'es2022',
  loader: { '.css': 'empty' },
  define: { 'process.env.NODE_ENV': '"production"', 'process.versions.node': 'undefined' },
})

const browser = await chromium.launch()
let report
try {
  const context = await browser.newContext({
    userAgent: `Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/${browser.version()} Safari/537.36`,
  })
  const page = await context.newPage()
  await page.addScriptTag({ content: bundle.outputFiles[0].text })
  report = await page.evaluate(() => globalThis.rainyEditorFilesProbe())
  if (process.argv[2]) {
    const path = resolve(process.argv[2])
    await mkdir(dirname(path), { recursive: true })
    await writeFile(path, JSON.stringify(report, null, 2) + '\n')
  }
} finally { await browser.close() }
console.log(JSON.stringify(report, null, 2))
assert.equal(report.passed, true, 'The real Monaco filesystem provider rejected a supported source URI')
