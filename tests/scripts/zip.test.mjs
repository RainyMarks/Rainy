/** The in-memory ZIP reader used to unpack pinned tool archives. */
import assert from 'node:assert/strict'
import { test } from 'node:test'
import { readZip } from '../../scripts/zip.mjs'
import { zipSync } from './zip-fixture.mjs'

test('reads stored and deflated entries in archive order, keeping duplicates and directories', () => {
  const text = Buffer.from('payload '.repeat(200))
  for (const method of [0, 8]) {
    const entries = readZip(zipSync([['tools/示例.txt', text], ['tools/', Buffer.alloc(0)], ['tools/示例.txt', Buffer.from('second')]], { method }))
    assert.deepEqual(entries.map(entry => entry.name), ['tools/示例.txt', 'tools/', 'tools/示例.txt'])
    assert.deepEqual(entries[0].data, text)
    assert.equal(entries[1].data.length, 0)
    assert.equal(entries[2].data.toString(), 'second')
  }
})

test('rejects damaged contents, unsupported methods and missing directories', () => {
  const archive = zipSync({ 'tool.exe': Buffer.from('program bytes') }, { method: 0 })
  const damaged = Buffer.from(archive)
  damaged[30 + 'tool.exe'.length] ^= 0xff
  assert.throws(() => readZip(damaged), /checksum differs/)
  const unsupported = Buffer.from(archive)
  const central = unsupported.indexOf(Buffer.from([0x50, 0x4b, 0x01, 0x02]))
  unsupported.writeUInt16LE(14, central + 10)
  assert.throws(() => readZip(unsupported), /Unsupported ZIP compression method 14/)
  assert.throws(() => readZip(Buffer.from('not an archive')), /end of central directory/)
})
