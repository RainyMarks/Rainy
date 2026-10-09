/** Bundled inference inputs remain pinned, complete, and free of model or user-state files. */
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { isAbsolute, join, relative } from 'node:path'
import { tmpdir } from 'node:os'
import { verifyStrataRuntime } from '../scripts/verify-strata-runtime.mjs'

const digest = value => createHash('sha256').update(value).digest('hex')
const fixtureFiles = ['portablepython/python.exe', 'portablepython/python312.dll', 'portablepython/LICENSE.txt',
  'portablepython/Lib/site-packages/strata-gguf.pth', 'server/serve/server.py', 'server/tools/iq_pack.py',
  'server/tools/mtp_rt.py', 'server/tools/gguf_reader.py', 'server/tools/strata_tokenizer.py', 'server/tools/_paths.py',
  'server/third_party/llama.cpp/gguf-py/gguf/__init__.py', 'server/data/expert-profile.bin', 'engine/strata.exe',
  'engine/strata-vision.exe', 'engine/cublas64_13.dll', 'engine/cublasLt64_13.dll', 'engine/cudart64_13.dll',
  'licenses/Strata-MIT.txt', 'licenses/llama.cpp-MIT.txt', 'licenses/ggml-MIT.txt', 'licenses/gguf-MIT.txt',
  'licenses/nvidia_cublas-13.0.2.14-LICENSE.txt', 'licenses/nvidia_cuda_runtime-13.0.96-LICENSE.txt', 'THIRD_PARTY_NOTICES.txt']

async function fixture(t) {
  const app = await mkdtemp(join(tmpdir(), 'rainy-strata-runtime-'))
  t.after(async () => {
    const child = relative(tmpdir(), app)
    assert(child && !child.startsWith('..') && !isAbsolute(child))
    await rm(app, { recursive: true, force: true, maxRetries: 5 })
  })
  const root = join(app, 'resources/strata-runtime')
  await mkdir(join(app, 'scripts'), { recursive: true })
  await mkdir(join(app, 'toolpacks'), { recursive: true })
  const preparer = '# fixture preparer\n'
  await writeFile(join(app, 'scripts/prepare-strata-runtime.py'), preparer)
  const files = []
  async function resource(path, text) {
    await mkdir(join(root, path, '..'), { recursive: true })
    await writeFile(join(root, path), text)
    files.push({ path, bytes: Buffer.byteLength(text), sha256: digest(text) })
  }
  for (const path of fixtureFiles) await resource(path, `fixture ${path}\n`)
  await resource('engine/BUILD.json', '{"version":"0.1.39","cuda":"13.0"}\n')
  const source = { version: 1, platform: 'win32', architecture: 'x64', strataVersion: '0.1.39', pythonVersion: '3.12.14',
    cuda: { toolkit: '13.0' }, inputs: files, wheels: [], generated: [] }
  const sourceBytes = Buffer.from(JSON.stringify(source))
  await writeFile(join(app, 'toolpacks/strata-runtime.sources.json'), sourceBytes)
  const inventory = { version: 1, platform: 'win32', architecture: 'x64', strataVersion: '0.1.39', pythonVersion: '3.12.14',
    sourceSha256: digest(sourceBytes), preparerSha256: digest(preparer), files }
  const inventoryPath = join(root, 'runtime-manifest.json')
  await writeFile(inventoryPath, JSON.stringify(inventory))
  return { app, root, inventory, inventoryPath, source }
}

test('a complete source-pinned inference runtime verifies without Python, GPU, or weights', async t => {
  const f = await fixture(t)
  const result = await verifyStrataRuntime(f.app)
  assert.equal(result.files, fixtureFiles.length + 1)
  assert.equal(result.strataVersion, '0.1.39')
  assert.equal(result.pythonVersion, '3.12.14')
})

test('changing an engine and its local inventory cannot replace the source-pinned digest', async t => {
  const f = await fixture(t)
  const text = 'replaced engine'
  await writeFile(join(f.root, 'engine/strata.exe'), text)
  const row = f.inventory.files.find(entry => entry.path === 'engine/strata.exe')
  row.sha256 = digest(text)
  row.bytes = Buffer.byteLength(text)
  await writeFile(f.inventoryPath, JSON.stringify(f.inventory))
  await assert.rejects(verifyStrataRuntime(f.app), /inventory differs/)
})

test('missing license text stops verification', async t => {
  const f = await fixture(t)
  await rm(join(f.root, 'licenses/Strata-MIT.txt'))
  await assert.rejects(verifyStrataRuntime(f.app), /files are missing/)
})

test('model weights and virtual-environment state are rejected even beside valid inputs', async t => {
  const f = await fixture(t)
  for (const name of ['model.gguf', 'experts.bin', 'pyvenv.cfg']) {
    await writeFile(join(f.root, name), 'unwanted')
    await assert.rejects(verifyStrataRuntime(f.app), /model, cache, or user-data path/)
    await rm(join(f.root, name))
  }
})

test('preparer changes require a rebuilt runtime', async t => {
  const f = await fixture(t)
  await writeFile(join(f.app, 'scripts/prepare-strata-runtime.py'), '# changed preparer\n')
  await assert.rejects(verifyStrataRuntime(f.app), /source definition/)
  assert.equal(JSON.parse(await readFile(f.inventoryPath, 'utf8')).strataVersion, '0.1.39')
})
