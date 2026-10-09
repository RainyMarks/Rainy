/** Acquire the exact offline browser dependencies; npm integrity is checked before extraction. */
import { createHash } from 'node:crypto'
import { createReadStream } from 'node:fs'
import { cp, mkdir, mkdtemp, readFile, readdir, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { dirname, isAbsolute, relative, resolve, sep } from 'node:path'
import { fileURLToPath } from 'node:url'
import { pipeline } from 'node:stream/promises'
import { createGunzip } from 'node:zlib'
import { Parser } from 'tar'

const scriptRoot = dirname(fileURLToPath(import.meta.url))
const appRoot = resolve(scriptRoot, '..')
const targetRoot = resolve(appRoot, 'resources/icesky/js/vendor')
const lock = JSON.parse(await readFile(resolve(scriptRoot, 'icesky-vendor-lock.json'), 'utf8'))
if (lock.version !== 1 || !Array.isArray(lock.packages)) throw new Error('IceSky vendor lock is invalid.')

async function verifyExisting() {
  const saved = JSON.parse(await readFile(resolve(targetRoot, 'OFFLINE_DEPENDENCIES.json'), 'utf8'))
  const identity = lock.packages.map(({ name, version, integrity, directory }) => ({ name, version, integrity, directory }))
  if (saved.version !== 1 || JSON.stringify(saved.packages) !== JSON.stringify(identity) || !saved.files) throw new Error('Offline dependency metadata does not match the vendor lock.')
  for (const [name, digest] of Object.entries(saved.files)) {
    const bytes = await readFile(child(targetRoot, name))
    if (createHash('sha256').update(bytes).digest('hex') !== digest) throw new Error(`Offline dependency differs from its recorded SHA-256: ${name}`)
  }
  for (const item of lock.files ?? []) if (saved.files[item.path] !== item.sha256) throw new Error('Offline upstream notice is missing or changed.')
}

if (process.argv.includes('--verify')) {
  await verifyExisting()
  process.stdout.write('IceSky offline dependency checksums verified.\n')
  process.exit(0)
}

const temporary = await mkdtemp(resolve(tmpdir(), 'rainy-icesky-vendor-'))
const acquired = []

function child(root, name) {
  const path = resolve(root, name)
  const within = relative(root, path)
  if (isAbsolute(within) || within === '..' || within.startsWith(`..${sep}`)) throw new Error('Vendor path escapes its allocated root.')
  return path
}

async function unpack(archive, destination) {
  const writes = []
  const parser = new Parser({ onReadEntry(entry) {
    const name = entry.path.replace(/^package\//, '')
    if (!name || entry.type !== 'File') { entry.resume(); return }
    const path = child(destination, name)
    const task = (async () => {
      const chunks = []
      for await (const chunk of entry) chunks.push(chunk)
      await mkdir(dirname(path), { recursive: true })
      await writeFile(path, Buffer.concat(chunks))
    })()
    writes.push(task)
  } })
  await pipeline(createReadStream(archive), createGunzip(), parser)
  await Promise.all(writes)
}

try {
  for (const item of lock.packages) {
    if (typeof item.name !== 'string' || typeof item.version !== 'string' || typeof item.integrity !== 'string' || !item.integrity.startsWith('sha512-')) throw new Error('Vendor package identity is invalid.')
    const packageName = item.name.split('/').pop()
    const url = `https://registry.npmjs.org/${item.name}/-/${packageName}-${item.version}.tgz`
    const response = await fetch(url, { redirect: 'error' })
    if (!response.ok) throw new Error(`Cannot acquire ${item.name}@${item.version}: HTTP ${response.status}`)
    const bytes = Buffer.from(await response.arrayBuffer())
    const integrity = `sha512-${createHash('sha512').update(bytes).digest('base64')}`
    if (integrity !== item.integrity) throw new Error(`Integrity mismatch for ${item.name}@${item.version}`)
    const directory = child(temporary, `${packageName}-${item.version}`)
    await mkdir(directory, { recursive: true })
    const archive = child(temporary, `${packageName}-${item.version}.tgz`)
    await writeFile(archive, bytes)
    await unpack(archive, directory)
    const target = child(targetRoot, item.directory)
    await mkdir(target, { recursive: true })
    for (const [from, to] of item.copies) await cp(child(directory, from), child(target, to), { recursive: true, filter: path => !path.endsWith('.map') && !path.endsWith('.d.ts') })
    const licenseTarget = child(targetRoot, `licenses/${packageName}-${item.version}`)
    await mkdir(licenseTarget, { recursive: true })
    const names = await readdir(directory)
    for (const name of names.filter(name => /^(?:licen[cs]e|notice|copying)(?:\.|$)/i.test(name))) await cp(child(directory, name), child(licenseTarget, name))
    const metadata = JSON.parse(await readFile(child(directory, 'package.json'), 'utf8'))
    await writeFile(child(licenseTarget, 'PACKAGE.json'), JSON.stringify({ name: metadata.name, version: metadata.version, license: metadata.license, repository: metadata.repository, integrity }, null, 2) + '\n')
    acquired.push({ name: item.name, version: item.version, integrity, directory: item.directory })
    process.stdout.write(`Prepared ${item.name}@${item.version}\n`)
  }
  for (const item of lock.files ?? []) {
    const response = await fetch(item.url, { redirect: 'error' })
    if (!response.ok) throw new Error(`Cannot acquire upstream notice: HTTP ${response.status}`)
    const bytes = Buffer.from(await response.arrayBuffer())
    if (createHash('sha256').update(bytes).digest('hex') !== item.sha256) throw new Error('Upstream notice checksum mismatch.')
    const target = child(targetRoot, item.path)
    await mkdir(dirname(target), { recursive: true })
    await writeFile(target, bytes)
  }
  const files = {}
  async function inventory(root) {
    const entries = await readdir(root, { withFileTypes: true })
    entries.sort((a, b) => a.name < b.name ? -1 : a.name > b.name ? 1 : 0)
    for (const entry of entries) {
      const path = child(root, entry.name)
      if (entry.isDirectory()) await inventory(path)
      else if (entry.isFile()) {
        const name = relative(targetRoot, path).split('\\').join('/')
        if (name === 'OFFLINE_DEPENDENCIES.json' || name === 'vue.min.js' || name.endsWith('.map') || name.endsWith('.d.ts')) continue
        files[name] = createHash('sha256').update(await readFile(path)).digest('hex')
      }
    }
  }
  const roots = [...new Set(lock.packages.map(item => item.directory.split('/')[0]))].sort()
  for (const root of [...roots, 'licenses']) await inventory(child(targetRoot, root))
  await writeFile(child(targetRoot, 'OFFLINE_DEPENDENCIES.json'), JSON.stringify({ version: 1, packages: acquired, files }, null, 2) + '\n')
} finally {
  const withinTemp = relative(resolve(tmpdir()), resolve(temporary))
  if (!withinTemp || isAbsolute(withinTemp) || withinTemp.startsWith('..')) throw new Error('Temporary vendor cleanup target is invalid.')
  await rm(temporary, { recursive: true, force: true })
}
