/** Add pinned tool units to a verified publisher pack without rebuilding unchanged legacy units. */
import { createHash } from 'node:crypto'
import { copyFile, mkdir, readFile, stat, writeFile } from 'node:fs/promises'
import { dirname, relative, resolve, sep } from 'node:path'
import { pathToFileURL } from 'node:url'
import { verifyPack } from './prepare-native-tools.mjs'

function child(root, path) {
  if (typeof path !== 'string' || !path || path.includes('\\') || path.includes(':')
    || path.split('/').some(part => !part || part === '.' || part === '..')) throw new Error(`Invalid pack path: ${path}`)
  const result = resolve(root, ...path.split('/'))
  const inside = relative(root, result)
  if (!inside || inside === '..' || inside.startsWith(`..${sep}`)) throw new Error(`Pack path escapes stage: ${path}`)
  return result
}

const digest = bytes => createHash('sha256').update(bytes).digest('hex')
const json = async path => JSON.parse(await readFile(path, 'utf8'))

/**
 * Merge additions prepared by prepare-native-tools into a stage restored from a publisher archive.
 * The baseline archive must first be checked against the SHA-256 in its publisher metadata.
 * @param {{baselineMetadata:string,baseStage:string,additionStage:string,sourceManifest:string,catalogOutput:string}} options Paths to verified inputs and outputs.
 * @returns {Promise<{tools:number,added:number,files:number}>} Merged counts.
 */
export async function extendNativeToolStage(options) {
  const base = resolve(options.baseStage)
  const addition = resolve(options.additionStage)
  const baseline = await json(options.baselineMetadata)
  const fullSource = await json(options.sourceManifest)
  const oldCatalog = await json(child(base, 'tools/manifest.json'))
  const newCatalog = await json(child(addition, 'tools/manifest.json'))
  const newInventory = await verifyPack(addition)
  if (![1, 2].includes(baseline.version) || !Array.isArray(baseline.files) || oldCatalog.version !== 1
    || newCatalog.version !== 1 || !Array.isArray(fullSource.tools)) throw new Error('Invalid native tool extension inputs')
  const oldManifest = baseline.files.find(file => file.path === 'tools/manifest.json')
  if (!oldManifest || digest(await readFile(child(base, oldManifest.path))) !== oldManifest.sha256) {
    throw new Error('Baseline catalog differs from publisher metadata')
  }
  const oldIds = new Set(oldCatalog.tools.map(tool => tool.id))
  const newIds = new Set(newCatalog.tools.map(tool => tool.id))
  if (newIds.size !== newCatalog.tools.length || [...newIds].some(id => oldIds.has(id))) throw new Error('Tool addition IDs overlap the baseline')
  const byId = new Map([...oldCatalog.tools, ...newCatalog.tools].map(tool => [tool.id, tool]))
  if (byId.size !== fullSource.tools.length || fullSource.tools.some(source => !byId.has(source.id))) {
    throw new Error('Full source manifest must enumerate exactly the merged tools')
  }
  for (const source of fullSource.tools) {
    const packaged = byId.get(source.id)
    if (source.version !== packaged.version || source.category !== packaged.category || source.entry.path !== packaged.entry.path) {
      throw new Error(`Source and packaged tool identity differ: ${source.id}`)
    }
  }
  const names = new Set(baseline.files.map(file => file.path.toLowerCase()))
  const addedFiles = newInventory.files.filter(file => file.path !== 'tools/manifest.json')
  for (const file of addedFiles) {
    if (names.has(file.path.toLowerCase())) throw new Error(`Addition file overlaps baseline: ${file.path}`)
    names.add(file.path.toLowerCase())
  }
  for (const file of addedFiles) {
    const destination = child(base, file.path)
    await mkdir(dirname(destination), { recursive: true })
    await copyFile(child(addition, file.path), destination, 1)
    if ((await stat(destination)).size !== file.bytes) throw new Error(`Copied addition differs: ${file.path}`)
  }
  const catalog = { version: 1, tools: fullSource.tools.map(source => byId.get(source.id)) }
  const catalogBytes = Buffer.from(JSON.stringify(catalog, null, 2) + '\n')
  await writeFile(child(base, 'tools/manifest.json'), catalogBytes)
  const files = [...baseline.files.filter(file => file.path !== 'tools/manifest.json'), ...addedFiles,
    { path: 'tools/manifest.json', bytes: catalogBytes.length, sha256: digest(catalogBytes) }]
    .sort((left, right) => left.path < right.path ? -1 : left.path > right.path ? 1 : 0)
  const inventory = { version: 1, algorithm: 'sha256', toolCount: catalog.tools.length,
    availableToolCount: catalog.tools.filter(tool => tool.available).length,
    sourceManifestSha256: digest(Buffer.from(JSON.stringify(fullSource))),
    bytes: files.reduce((total, file) => total + file.bytes, 0), files }
  await writeFile(child(base, 'toolpack-files.json'), JSON.stringify(inventory, null, 2) + '\n')
  await writeFile(child(base, '.native-tools-stage'), 'RainyAgent native tools stage v1\n')
  await verifyPack(base)
  await writeFile(resolve(options.catalogOutput), catalogBytes)
  return { tools: catalog.tools.length, added: newIds.size, files: files.length }
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  const keys = { '--baseline-metadata': 'baselineMetadata', '--base-stage': 'baseStage',
    '--addition-stage': 'additionStage', '--source-manifest': 'sourceManifest', '--catalog': 'catalogOutput' }
  const options = {}
  const args = process.argv.slice(2)
  if (args.length !== 10) throw new Error(`Usage: extend-native-tool-stage.mjs ${Object.keys(keys).map(key => `${key} VALUE`).join(' ')}`)
  for (let index = 0; index < args.length; index += 2) {
    const key = keys[args[index]]
    if (!key || options[key]) throw new Error('Invalid native tool extension arguments')
    options[key] = args[index + 1]
  }
  console.log(JSON.stringify(await extendNativeToolStage(options)))
}
