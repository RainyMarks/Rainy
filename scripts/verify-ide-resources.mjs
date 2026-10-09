/** Refuse a release with missing, stale or changed offline development resources. */
import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { readFile, realpath } from 'node:fs/promises'
import { dirname, relative, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

/** Verify the complete prepared IDE payload before building a carrier.
 * @param {string} app - application directory containing source and resource inventories.
 * @returns {Promise<{helpers:number,systemPackages:number,bytes:number}>} verified counts and bytes.
 */
export async function verifyIdeResources(app) {
const sourceFile = resolve(app, 'toolpacks/ide-tools.sources.json')
const root = await realpath(resolve(app, 'resources/ide'))
const sha256 = bytes => createHash('sha256').update(bytes).digest('hex')
const source = JSON.parse(await readFile(sourceFile, 'utf8'))
const helpers = JSON.parse(await readFile(resolve(root, 'manifest.json'), 'utf8'))
assert.equal(helpers.version, 1)
assert.equal(helpers.sourceSha256, sha256(await readFile(sourceFile)))
assert.equal(helpers.preparerSha256, sha256(await readFile(resolve(app, 'scripts/prepare-ide-tools.py'))))
assert.equal(helpers.installerSourceSha256, sha256(await readFile(resolve(app, 'scripts/install-ide-system-packages.py'))))
assert.deepEqual(helpers.assets, source.assets)
assert.deepEqual(helpers.licenses, source.licenses)

async function inventory(directory, rows, field) {
  assert(Array.isArray(rows) && rows.length > 0, 'The IDE inventory is empty')
  const seen = new Set()
  let bytes = 0
  for (const row of rows) {
    const name = row[field]
    assert(typeof name === 'string' && !name.startsWith('/') && !name.includes('\\') && !name.split('/').includes('..'))
    assert(!seen.has(name), `Duplicate IDE inventory file: ${name}`)
    seen.add(name)
    const target = await realpath(resolve(directory, name))
    const child = relative(directory, target)
    assert(child && !child.startsWith('..') && !/^[A-Za-z]:/.test(child), `IDE inventory file escapes its directory: ${name}`)
    const data = await readFile(target)
    assert.equal(data.length, row.bytes, name)
    assert.equal(sha256(data), row.sha256, name)
    bytes += data.length
  }
  return { files: seen.size, bytes, names: seen }
}
const helperFiles = await inventory(root, helpers.files, 'path')
for (const name of helpers.required) assert(helperFiles.names.has(name), `Required IDE helper not inventoried: ${name}`)
for (const license of source.licenses) {
  const entry = helpers.files.find(row => row.path === license.path)
  assert(entry, `Pinned IDE license not inventoried: ${license.path}`)
  assert.equal(entry.bytes, license.bytes, license.path)
  assert.equal(entry.sha256, license.sha256, license.path)
  assert(helpers.required.includes(license.path), `Pinned IDE license not required: ${license.path}`)
}
const systemRoot = await realpath(resolve(root, 'system-packages'))
const system = JSON.parse(await readFile(resolve(systemRoot, 'manifest.json'), 'utf8'))
assert.equal(system.version, 1)
assert.equal(system.os, 'ubuntu')
assert.equal(system.osVersion, source.ubuntuBaseline)
assert.equal(system.architecture, 'amd64')
assert.deepEqual(system.requestedPackages, source.systemPackages)
const systemFiles = await inventory(systemRoot, system.packages, 'file')
return { helpers: helperFiles.files, systemPackages: systemFiles.files, bytes: helperFiles.bytes + systemFiles.bytes }
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  console.log(JSON.stringify(await verifyIdeResources(resolve(dirname(fileURLToPath(import.meta.url)), '..'))))
}
