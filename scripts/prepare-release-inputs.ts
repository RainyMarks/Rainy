/** Authenticate and reuse immutable offline inputs before publishing a release catalog. */
import { copyFile, mkdir, readFile, stat, writeFile } from 'node:fs/promises'
import { dirname, join, resolve } from 'node:path'
import { componentDigest, environmentComponentSchema } from '../src/environment-components.ts'
import { toolPackMetadataSchema } from '../src/toolpack-format.ts'
import { environmentMedia } from '../src/environment-media.ts'

const app = resolve(import.meta.dirname, '..')
const version = JSON.parse(await readFile(join(app, 'package.json'), 'utf8')).version
function option(name: string): string | undefined {
  const at = process.argv.indexOf(name)
  return at < 0 ? undefined : process.argv[at + 1]
}
const output = resolve(option('--output') ?? join(app, `release/offline-${version}`))
const components = resolve(option('--components') ?? join(output, 'environment-components'))
const tools = option('--tools')
async function publish(source: string, destination: string, expected: { bytes: number; sha256: string }): Promise<void> {
  if ((await stat(source)).size !== expected.bytes || await componentDigest(source) !== expected.sha256) throw new Error(`Offline input checksum differs: ${source}`)
  await mkdir(dirname(destination), { recursive: true })
  if (resolve(source) !== resolve(destination)) {
    let present = false
    try { present = (await stat(destination)).size === expected.bytes && await componentDigest(destination) === expected.sha256 }
    catch (error) { if (!(error instanceof Error && 'code' in error && error.code === 'ENOENT')) throw error }
    if (!present) { await copyFile(source, destination); if (await componentDigest(destination) !== expected.sha256) throw new Error(`Offline publication checksum differs: ${destination}`) }
  }
}
await mkdir(output, { recursive: true })
if (tools) {
  const source = resolve(tools)
  const metadataText = await readFile(join(source, 'native-tools-metadata.json'), 'utf8')
  const metadata = toolPackMetadataSchema.parse(JSON.parse(metadataText))
  // A per-tool pack lists one archive per unit; earlier packs list split volumes.
  const media = metadata.version === 1 ? metadata.volumes : metadata.archives
  for (const volume of media) await publish(join(source, volume.file), join(output, volume.file), volume)
  await writeFile(join(output, 'native-tools-metadata.json'), metadataText)
  await copyFile(join(source, '工具清单.json'), join(output, '工具清单.json'))
  console.log(JSON.stringify({ reusedToolPack: metadata.id, files: media.length }))
}
const catalog = []
for (const id of ['windows-basic', 'windows-science-cpu', 'windows-science-cuda', 'windows-cpp', 'linux-basic', 'linux-science-cpu', 'linux-science-cuda']) {
  const descriptor = environmentComponentSchema.parse(JSON.parse(await readFile(join(components, `${id}.json`), 'utf8')))
  if (descriptor.id !== id) throw new Error('A component descriptor belongs to another requested component.')
  await publish(join(components, descriptor.file), join(output, 'environment-components', descriptor.file), descriptor)
  await writeFile(join(output, 'environment-components', `${id}.json`), JSON.stringify(descriptor, null, 2) + '\n')
  catalog.push(descriptor)
}
for (const item of Object.values(environmentMedia)) await publish(join(app, 'runtime/environment', item.file), join(output, 'environment', item.file), item)
await copyFile(join(app, 'runtime/environment/media-verification.json'), join(output, 'environment/media-verification.json'))
await writeFile(join(app, 'runtime/environment-component-catalog.json'), JSON.stringify({ version: 1, components: catalog }, null, 2) + '\n')
console.log(JSON.stringify({ output, components: catalog.length, componentBytes: catalog.reduce((sum, item) => sum + item.bytes, 0) }))
