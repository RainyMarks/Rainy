/**
 * Assemble `runtime/windows-host/`, the Windows Host shipped as an installer resource:
 *
 *   rainy-host.exe     the Rust Host (native/target/release, from native/scripts/build-hosts.sh)
 *   app/dist/renderer  the viewer page (scripts/build.ts)
 *   app/resources      the viewer resources (pdfjs and icon.png), without IDE tools: the Windows ones
 *                      (runtime/ide-windows, from `prepare-ide-tools.py --platform win32-x64`) are the optional
 *                      modules ide-python and ide-web (prepare-optional-modules.mjs), the Linux ones belong to the
 *                      Linux runtime. License files live at the Windows installation root via package-native.mjs.
 *   runtime.json       the file inventory
 *
 * No Node.js is shipped. PowerShell is not shipped either: the Host uses PowerShell 7 when the system or the Windows
 * basic component has it, else Windows PowerShell 5.1. The directory is replaced atomically; a failed run leaves the
 * previous staging in place.
 */
import { cp, mkdir, mkdtemp, readFile, readdir, rename, rm, stat, writeFile } from 'node:fs/promises'
import { dirname, join, relative, resolve, sep } from 'node:path'
import { fileURLToPath } from 'node:url'
import { fileDigest } from './lib/common.mjs'

const app = resolve(dirname(fileURLToPath(import.meta.url)), '..')
// RAINY_PROFILE=qa stages the Host native/scripts/build-hosts.sh built with the QA profile (QA packages only).
const host = join(app, 'native/target', process.env.RAINY_PROFILE || 'release', 'rainy-host.exe')
const destination = join(app, 'runtime/windows-host')

async function digest(path) {
  return (await fileDigest(path)).digest
}

async function required(path, hint) {
  try { await stat(path) } catch (error) {
    if (error.code === 'ENOENT') throw new Error(`${relative(app, path)} is missing: ${hint}`)
    throw error
  }
}
await required(host, 'run native/scripts/build-hosts.sh')
await required(join(app, 'dist/renderer/index.html'), 'run pnpm run build:release')

await mkdir(join(app, 'runtime'), { recursive: true })
const staging = await mkdtemp(join(app, 'runtime/windows-host-pending-'))
try {
  await cp(host, join(staging, 'rainy-host.exe'))
  const target = join(staging, 'app')
  await mkdir(join(target, 'dist'), { recursive: true })
  await cp(join(app, 'dist/renderer'), join(target, 'dist/renderer'), { recursive: true, filter: file => !file.endsWith('.map') })
  // Allowlist only the viewer resources the Windows Host actually serves (pdf.js and icon.png).
  await mkdir(join(target, 'resources'), { recursive: true })
  await cp(join(app, 'resources/pdfjs'), join(target, 'resources/pdfjs'), {
    recursive: true, dereference: true, filter: file => !file.endsWith('.map'),
  })
  await cp(join(app, 'resources/icon.png'), join(target, 'resources/icon.png'), { dereference: true })

  const files = []
  async function inventory(directory) {
    for (const entry of await readdir(directory, { withFileTypes: true })) {
      const path = join(directory, entry.name)
      if (entry.isDirectory()) await inventory(path)
      else if (entry.isFile()) files.push({ path: relative(staging, path).split(sep).join('/'), bytes: (await stat(path)).size, sha256: await digest(path) })
      else throw new Error(`Nonportable runtime entry: ${path}`)
    }
  }
  await inventory(staging)
  const version = JSON.parse(await readFile(join(app, 'package.json'), 'utf8')).version
  await writeFile(join(staging, 'runtime.json'), JSON.stringify({ version: 2, platform: 'win32', architecture: 'x64', host: version, files }, null, 2) + '\n')

  const previous = `${destination}.previous`
  await rm(previous, { recursive: true, force: true })
  try { await rename(destination, previous) } catch (error) { if (error.code !== 'ENOENT') throw error }
  await rename(staging, destination)
  await rm(previous, { recursive: true, force: true })
  console.log(`Staged ${files.length} files in runtime/windows-host (${(files.reduce((sum, file) => sum + file.bytes, 0) / 1048576).toFixed(1)} MiB).`)
} catch (error) {
  await rm(staging, { recursive: true, force: true })
  throw error
}
