/** Build a deterministic content version for the workbench's packaged browser resources. */
import { createHash } from 'node:crypto'
import { readFile, readdir, rename, writeFile } from 'node:fs/promises'
import { dirname, extname, join, relative, resolve } from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'

/** Static response MIME types, including PDF.js and OCR worker dependencies. */
export const ICE_SKY_MIME_TYPES = {
  '.html': 'text/html', '.js': 'text/javascript', '.mjs': 'text/javascript', '.css': 'text/css',
  '.svg': 'image/svg+xml', '.png': 'image/png', '.jpg': 'image/jpeg', '.jpeg': 'image/jpeg',
  '.woff': 'font/woff', '.woff2': 'font/woff2', '.ttf': 'font/ttf', '.json': 'application/json',
  '.wasm': 'application/wasm', '.bcmap': 'application/octet-stream', '.pfb': 'application/octet-stream',
  '.gz': 'application/gzip', '.traineddata': 'application/octet-stream',
}

/**
 * Write a manifest after all local integration edits and offline vendor acquisition complete.
 * @param resourceRoot - directory containing the shipped workbench source and vendor resources.
 * @returns the generated resource version and immutable asset metadata.
 */
export async function writeIceSkyManifest(resourceRoot) {
  const paths = []
  async function visit(directory) {
    const entries = await readdir(directory, { withFileTypes: true })
    for (const entry of entries) {
      if (entry.name === '.git' || entry.name === 'assets-manifest.json' || entry.name.endsWith('.map')) continue
      const path = join(directory, entry.name)
      if (entry.isDirectory()) await visit(path)
      else if (entry.isFile() && ICE_SKY_MIME_TYPES[extname(entry.name).toLowerCase()]) paths.push(path)
    }
  }
  await visit(resourceRoot)
  // Windows path spelling orders the files so every build platform derives the same version.
  const key = path => relative(resourceRoot, path).split('/').join('\\')
  paths.sort((a, b) => key(a) < key(b) ? -1 : key(a) > key(b) ? 1 : 0)
  const files = {}
  for (const path of paths) {
    const bytes = await readFile(path)
    const name = relative(resourceRoot, path).split('\\').join('/')
    files[name] = { sha256: createHash('sha256').update(bytes).digest('hex'), size: bytes.length, type: ICE_SKY_MIME_TYPES[extname(path).toLowerCase()] }
  }
  const version = createHash('sha256').update(JSON.stringify(files)).digest('hex')
  const manifest = { format: 1, version, files }
  const target = join(resourceRoot, 'assets-manifest.json')
  const temporary = `${target}.${process.pid}.tmp`
  await writeFile(temporary, JSON.stringify(manifest, null, 2) + '\n')
  await rename(temporary, target)
  return manifest
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  const appRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..')
  const manifest = await writeIceSkyManifest(resolve(appRoot, 'resources/icesky'))
  process.stdout.write(`IceSky assets: ${Object.keys(manifest.files).length} files, version ${manifest.version}\n`)
}
