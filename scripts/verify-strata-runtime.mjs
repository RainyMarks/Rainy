/** Verify the complete pinned Strata runtime, including licenses, without loading a model or GPU. */
import { createHash } from 'node:crypto'
import { lstat, readFile, readdir } from 'node:fs/promises'
import { join, resolve } from 'node:path'
import { pathToFileURL } from 'node:url'
import { releaseDigest, releasePath } from './release-assets.mjs'

const hash = bytes => createHash('sha256').update(bytes).digest('hex')
const required = ['portablepython/python.exe', 'portablepython/python312.dll', 'portablepython/LICENSE.txt',
  'portablepython/Lib/site-packages/strata-gguf.pth', 'server/serve/server.py', 'server/tools/iq_pack.py',
  'server/tools/mtp_rt.py', 'server/tools/gguf_reader.py', 'server/tools/strata_tokenizer.py', 'server/tools/_paths.py',
  'server/third_party/llama.cpp/gguf-py/gguf/__init__.py', 'server/data/expert-profile.bin',
  'engine/strata.exe', 'engine/strata-vision.exe', 'engine/BUILD.json', 'engine/cublas64_13.dll',
  'engine/cublasLt64_13.dll', 'engine/cudart64_13.dll', 'licenses/Strata-MIT.txt', 'licenses/llama.cpp-MIT.txt',
  'licenses/ggml-MIT.txt', 'licenses/gguf-MIT.txt', 'licenses/nvidia_cublas-13.0.2.14-LICENSE.txt',
  'licenses/nvidia_cuda_runtime-13.0.96-LICENSE.txt', 'THIRD_PARTY_NOTICES.txt']

function allowed(name) {
  releasePath(name)
  const parts = name.toLowerCase().split('/')
  if (parts.some(part => ['.venv', '__pycache__', 'models', 'weights', 'credentials'].includes(part))
    || /\.(?:gguf|safetensors|ckpt|onnx|pyc|pyo|key)$/iu.test(name)
    || /(?:^|\/)(?:dense\.bin|experts\.bin|pyvenv\.cfg|\.env(?:\..*)?|credentials\.json|settings\.json)$/iu.test(name)) {
    throw new Error(`Strata runtime contains a model, cache, or user-data path: ${name}`)
  }
}

/**
 * Match source-pinned files and the exact generated inventory before embedding this runtime.
 * @param {string} appDirectory Rainy desktop source directory containing the source definition and preparer.
 * @param {string} [runtimeDirectory] Prepared runtime, defaulting to resources/strata-runtime.
 * @returns {Promise<{files:number,bytes:number,strataVersion:string,pythonVersion:string}>} Verified inventory totals.
 */
export async function verifyStrataRuntime(appDirectory, runtimeDirectory = join(appDirectory, 'resources/strata-runtime')) {
  const definitionPath = join(appDirectory, 'toolpacks/strata-runtime.sources.json')
  const definitionBytes = await readFile(definitionPath)
  const source = JSON.parse(definitionBytes)
  if (source.version !== 1 || source.platform !== 'win32' || source.architecture !== 'x64'
    || !Array.isArray(source.inputs) || !Array.isArray(source.wheels) || !Array.isArray(source.generated)) throw new Error('Invalid Strata runtime source definition')
  const expected = new Map()
  function expectFile(row) {
    allowed(row.path)
    const lower = row.path.toLowerCase()
    if (expected.has(lower) || !Number.isSafeInteger(row.bytes) || row.bytes < 0 || !/^[a-f0-9]{64}$/u.test(row.sha256)) throw new Error(`Invalid or duplicate Strata input: ${row.path}`)
    expected.set(lower, { path: row.path, bytes: row.bytes, sha256: row.sha256 })
  }
  for (const row of source.inputs) expectFile(row)
  for (const wheel of source.wheels) for (const row of wheel.members) expectFile(row)
  for (const row of source.generated) {
    if (typeof row.text !== 'string') throw new Error(`Invalid generated Strata resource: ${row.path}`)
    expectFile({ path: row.path, bytes: Buffer.byteLength(row.text), sha256: hash(row.text) })
  }
  for (const name of required) if (!expected.has(name.toLowerCase())) throw new Error(`Required Strata runtime file is not pinned: ${name}`)
  const root = resolve(runtimeDirectory)
  if ((await lstat(root)).isSymbolicLink()) throw new Error('Strata runtime root cannot be a link')
  const manifestPath = join(root, 'runtime-manifest.json')
  if (!(await lstat(manifestPath)).isFile()) throw new Error('Strata runtime manifest must be a regular file')
  const manifest = JSON.parse(await readFile(manifestPath, 'utf8'))
  if (manifest.version !== 1 || manifest.sourceSha256 !== hash(definitionBytes)
    || manifest.preparerSha256 !== (await releaseDigest(join(appDirectory, 'scripts/prepare-strata-runtime.py'))).sha256
    || manifest.platform !== source.platform || manifest.architecture !== source.architecture
    || manifest.strataVersion !== source.strataVersion || manifest.pythonVersion !== source.pythonVersion
    || !Array.isArray(manifest.files) || manifest.files.length !== expected.size) throw new Error('Strata runtime inventory differs from its source definition')
  const recorded = new Set()
  for (const row of manifest.files) {
    const original = expected.get(row.path.toLowerCase())
    if (!original || recorded.has(row.path.toLowerCase()) || row.path !== original.path || row.bytes !== original.bytes || row.sha256 !== original.sha256) throw new Error(`Strata runtime inventory differs: ${row.path}`)
    recorded.add(row.path.toLowerCase())
  }
  const observed = new Set()
  let bytes = 0
  async function walk(directory, prefix = '') {
    for (const entry of await readdir(directory, { withFileTypes: true })) {
      const name = prefix + entry.name
      allowed(name)
      const path = join(directory, entry.name)
      if (entry.isSymbolicLink()) throw new Error(`Strata runtime contains a link: ${name}`)
      if (entry.isDirectory()) { await walk(path, name + '/'); continue }
      if (!entry.isFile()) throw new Error(`Unexpected Strata runtime entry: ${name}`)
      if (name === 'runtime-manifest.json') continue
      const original = expected.get(name.toLowerCase())
      if (!original || name !== original.path || observed.has(name.toLowerCase())) throw new Error(`Unpinned Strata runtime file: ${name}`)
      const actual = await releaseDigest(path)
      if (actual.bytes !== original.bytes || actual.sha256 !== original.sha256) throw new Error(`Strata runtime checksum differs: ${name}`)
      observed.add(name.toLowerCase())
      bytes += actual.bytes
      if (/\.(?:py|json|txt|pth|yml|yaml|ini)$/iu.test(name) && actual.bytes <= 4 * 1024 ** 2) {
        if (/-----BEGIN (?:[A-Z ]+ )?PRIVATE KEY-----/u.test(await readFile(path, 'utf8'))) throw new Error(`Private key in Strata runtime: ${name}`)
      }
    }
  }
  await walk(root)
  if (observed.size !== expected.size) throw new Error('Strata runtime files are missing')
  const engine = JSON.parse(await readFile(join(root, 'engine/BUILD.json'), 'utf8'))
  if (engine.version !== source.strataVersion || engine.cuda !== source.cuda.toolkit) throw new Error('Strata engine version differs from its source definition')
  return { files: observed.size, bytes, strataVersion: source.strataVersion, pythonVersion: source.pythonVersion }
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  const args = process.argv.slice(2)
  if (args.length && (args.length !== 2 || args[0] !== '--runtime')) throw new Error('Usage: verify-strata-runtime.mjs [--runtime DIRECTORY]')
  console.log(JSON.stringify(await verifyStrataRuntime(resolve(import.meta.dirname, '..'), args[1])))
}
