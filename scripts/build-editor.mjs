/** Build same-origin ESM Monaco and terminal assets into resources/editor, loaded by the renderer on first use. */
import { build } from 'esbuild'
import { mkdir, readFile, writeFile, copyFile, readdir, rename, rm, realpath } from 'node:fs/promises'
import { dirname, resolve, basename, relative } from 'node:path'
import { fileURLToPath } from 'node:url'
import { createHash, randomUUID } from 'node:crypto'

const appRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const resources = await realpath(resolve(appRoot, 'resources'))
const finalOutput = resolve(resources, 'editor')
const output = resolve(resources, `.editor-build-${randomUUID()}`)
const previous = resolve(resources, `.editor-old-${randomUUID()}`)
const removeOwnedDirectory = async path => {
  const contained = relative(resources, resolve(path))
  if (contained.includes('..') || dirname(resolve(path)) !== resources || !/^\.editor-(build|old)-[a-f0-9-]+$/.test(basename(path))) {
    throw new Error(`Refusing to remove a non-build directory: ${path}`)
  }
  await rm(path, { recursive: true, force: true })
}
try {
await mkdir(output, { recursive: true })
const copied = new Map()
const assets = {
  name: 'vscode-offline-resources',
  setup(builder) {
    builder.onLoad({ filter: /\.js$/ }, async ({ path }) => {
      if (!path.includes('node_modules')) return undefined
      let source = await readFile(path, 'utf8')
      const matches = [...source.matchAll(/new URL\((['"])([^'"\n]+)\1,\s*import\.meta\.url\)/g)]
      for (const match of matches) {
        if (!match[2].startsWith('.') && !match[2].endsWith('.wasm')) continue
        const origin = resolve(dirname(path), match[2])
        const bytes = await readFile(origin)
        const extension = origin.slice(origin.lastIndexOf('.'))
        const name = `resource-${createHash('sha256').update(bytes).digest('hex').slice(0, 16)}${extension}`
        await copyFile(origin, resolve(output, name))
        copied.set(origin, resolve(output, name))
        source = source.replace(match[0], `new URL(${JSON.stringify(`./${name}`)}, import.meta.url)`)
      }
      return { contents: source, loader: 'js' }
    })
  },
}
const result = await build({
  absWorkingDir: appRoot,
  entryPoints: {
    editor: 'src/editor/editor.ts',
    'editor.worker': 'src/editor/editor.worker.ts',
    'textmate.worker': 'src/editor/textmate.worker.ts',
  },
  outdir: output, format: 'esm', platform: 'browser', target: 'es2022', bundle: true, splitting: true,
  entryNames: '[name]', chunkNames: 'chunk-[hash]', assetNames: 'asset-[hash]',
  loader: { '.ttf': 'file', '.woff': 'file', '.woff2': 'file', '.wasm': 'file', '.svg': 'dataurl', '.png': 'file' },
  define: { 'process.env.NODE_ENV': '"production"', 'process.versions.node': 'undefined' }, minify: true, metafile: true,
  plugins: [assets],
})
const outputs = Object.keys(result.metafile.outputs).map(path => resolve(appRoot, path))
const styles = outputs.filter(path => path.endsWith('.css')).sort()
await writeFile(resolve(output, 'editor.css'), (await Promise.all(styles.map(path => readFile(path, 'utf8')))).join('\n') + '\n')
const packageCache = new Map()
const packageFor = directory => {
  if (packageCache.has(directory)) return packageCache.get(directory)
  const pending = (async () => {
    try {
      const metadata = JSON.parse(await readFile(resolve(directory, 'package.json'), 'utf8'))
      return typeof metadata.name === 'string' && typeof metadata.version === 'string'
        ? { directory, metadata } : packageFor(dirname(directory))
    }
    catch (error) {
      if (error?.code !== 'ENOENT') throw error
      const parent = dirname(directory)
      if (parent === directory) throw new Error(`No package metadata for ${directory}`)
      return packageFor(parent)
    }
  })()
  packageCache.set(directory, pending)
  return pending
}
const dependencies = new Map()
for (const input of Object.keys(result.metafile.inputs)) {
  if (!input.includes('node_modules')) continue
  const source = await packageFor(dirname(resolve(appRoot, input)))
  dependencies.set(`${source.metadata.name}@${source.metadata.version}`, source)
}
const notices = ['RainyAgent offline editor: third-party license and notice texts.\n']
const missing = []
for (const [name, source] of [...dependencies].sort(([left], [right]) => left.localeCompare(right))) {
  notices.push(`\n${'='.repeat(72)}\n${name}\nLicense: ${source.metadata.license ?? 'See notice below'}\n`)
  const licenseFiles = (await readdir(source.directory, { withFileTypes: true })).filter(entry => entry.isFile() && /^(licen[cs]e|copying|notice|third[-_ ]?party[-_ ]?notices)/i.test(entry.name)).sort((left, right) => left.name.localeCompare(right.name))
  if (licenseFiles.length === 0) {
    if (name.startsWith('@codingame/')) notices.push('The CodinGame and Microsoft license texts and VS Code third-party notices below apply.\n')
    else missing.push(name)
  }
  for (const entry of licenseFiles) notices.push(`\n--- ${entry.name} ---\n${await readFile(resolve(source.directory, entry.name), 'utf8')}\n`)
}
if (missing.length > 0) throw new Error(`Missing packaged license texts: ${missing.join(', ')}`)
const licenses = resolve(appRoot, 'src/editor/licenses')
const licenseSources = JSON.parse(await readFile(resolve(licenses, 'sources.json'), 'utf8'))
for (const source of licenseSources) notices.push(`\n${'='.repeat(72)}\n${source.Name}\nSource: ${source.Url}\n\n${await readFile(resolve(licenses, source.Name), 'utf8')}\n`)
const noticePath = resolve(output, 'THIRD_PARTY_NOTICES.txt')
await writeFile(noticePath, notices.join(''))
const paths = [...new Set([...outputs, ...copied.values(), resolve(output, 'editor.css'), noticePath])].sort()
const files = await Promise.all(paths.map(async path => {
  const bytes = await readFile(path)
  return { path: path.slice(output.length + 1).replaceAll('\\', '/'), bytes: bytes.length,
    sha256: createHash('sha256').update(bytes).digest('hex') }
}))
await writeFile(resolve(output, 'manifest.json'), JSON.stringify({ version: 1, files }, null, 2) + '\n')
let movedPrevious = false
try { await rename(finalOutput, previous); movedPrevious = true }
catch (error) { if (error?.code !== 'ENOENT') throw error }
try { await rename(output, finalOutput) }
catch (error) { if (movedPrevious) await rename(previous, finalOutput); throw error }
if (movedPrevious) await removeOwnedDirectory(previous)
process.stdout.write(`Built ${files.length} offline editor assets.\n`)
} finally { await removeOwnedDirectory(output) }
