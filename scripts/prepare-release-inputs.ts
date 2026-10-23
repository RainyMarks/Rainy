/** Authenticate and reuse immutable offline inputs before publishing a release catalog. */
import { createHash } from 'node:crypto'
import { createReadStream } from 'node:fs'
import { copyFile, mkdir, readFile, stat, writeFile } from 'node:fs/promises'
import { dirname, join, resolve } from 'node:path'
import { environmentMedia } from './environment-media.ts'

/** A release-owned environment component descriptor (the carrier's `modules` catalog entry). */
interface EnvironmentComponent {
  version: 1
  id: string
  platform: 'windows' | 'linux'
  architecture: 'x64'
  file: string
  bytes: number
  sha256: string
  unpackedBytes: number
  manifestSha256: string
}

const DIGEST = /^[a-f0-9]{64}$/u
const FIELDS = ['version', 'id', 'platform', 'architecture', 'file', 'bytes', 'sha256', 'unpackedBytes', 'manifestSha256']

/** Validate a component descriptor before any path is built from it. */
function componentDescriptor(value: unknown): EnvironmentComponent {
  const item = value as Record<string, unknown>
  const count = (field: string): boolean => Number.isSafeInteger(item[field]) && (item[field] as number) > 0
  const valid = typeof value === 'object' && value !== null && Object.keys(item).every(key => FIELDS.includes(key)) && item.version === 1
    && typeof item.id === 'string' && /^(?:windows|linux)-(?:basic|science-cpu|science-cuda|cpp|development)$/u.test(item.id)
    && (item.platform === 'windows' || item.platform === 'linux') && item.id.startsWith(`${item.platform}-`) && item.architecture === 'x64'
    && item.file === `${item.id}.tar.gz` && count('bytes') && count('unpackedBytes')
    && typeof item.sha256 === 'string' && DIGEST.test(item.sha256) && typeof item.manifestSha256 === 'string' && DIGEST.test(item.manifestSha256)
  if (!valid) throw new Error('Invalid environment component descriptor')
  return item as unknown as EnvironmentComponent
}

async function componentDigest(path: string): Promise<string> {
  const digest = createHash('sha256')
  for await (const chunk of createReadStream(path)) digest.update(chunk as Buffer)
  return digest.digest('hex')
}

const app = resolve(import.meta.dirname, '..')
const version = JSON.parse(await readFile(join(app, 'package.json'), 'utf8')).version
function option(name: string): string | undefined {
  const at = process.argv.indexOf(name)
  return at < 0 ? undefined : process.argv[at + 1]
}
const output = resolve(option('--output') ?? join(app, `release/offline-${version}`))
const components = resolve(option('--components') ?? join(output, 'environment-components'))
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
const catalog = []
for (const id of ['windows-basic', 'windows-science-cpu', 'windows-science-cuda', 'windows-cpp', 'linux-basic', 'linux-science-cpu', 'linux-science-cuda']) {
  const descriptor = componentDescriptor(JSON.parse(await readFile(join(components, `${id}.json`), 'utf8')))
  if (descriptor.id !== id) throw new Error('A component descriptor belongs to another requested component.')
  await publish(join(components, descriptor.file), join(output, 'environment-components', descriptor.file), descriptor)
  await writeFile(join(output, 'environment-components', `${id}.json`), JSON.stringify(descriptor, null, 2) + '\n')
  catalog.push(descriptor)
}
for (const item of Object.values(environmentMedia)) await publish(join(app, 'runtime/environment', item.file), join(output, 'environment', item.file), item)
await copyFile(join(app, 'runtime/environment/media-verification.json'), join(output, 'environment/media-verification.json'))
await writeFile(join(app, 'runtime/environment-component-catalog.json'), JSON.stringify({ version: 1, components: catalog }, null, 2) + '\n')
console.log(JSON.stringify({ output, components: catalog.length, componentBytes: catalog.reduce((sum, item) => sum + item.bytes, 0) }))
