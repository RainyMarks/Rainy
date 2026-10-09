/** The installer icon generated from resources/icon.png without native image libraries. */
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { test } from 'node:test'
import { ICO_SIZES, containSquare, decodePng, encodePng, pngToIco } from '../../scripts/icon.mjs'

test('the application icon becomes one PNG entry per Windows icon size', () => {
  const ico = pngToIco(readFileSync(new URL('../../resources/icon.png', import.meta.url)))
  assert.equal(ico.readUInt16LE(2), 1)
  assert.equal(ico.readUInt16LE(4), ICO_SIZES.length)
  ICO_SIZES.forEach((size, index) => {
    const at = 6 + index * 16
    assert.equal(ico[at], size === 256 ? 0 : size)
    const entry = ico.subarray(ico.readUInt32LE(at + 12), ico.readUInt32LE(at + 12) + ico.readUInt32LE(at + 8))
    const image = decodePng(entry)
    assert.deepEqual([image.width, image.height], [size, size])
  })
})

test('scaling keeps the aspect ratio inside a transparent square and averages covered pixels', () => {
  const pixels = new Uint8Array(4 * 2 * 4)
  for (let index = 0; index < 8; index++) pixels.set(index % 2 ? [0, 0, 255, 255] : [255, 0, 0, 255], index * 4)
  const square = containSquare({ width: 4, height: 2, pixels }, 2)
  assert.deepEqual([...square.subarray(0, 8)], [128, 0, 128, 255, 128, 0, 128, 255])
  assert.deepEqual([...square.subarray(8)], [0, 0, 0, 0, 0, 0, 0, 0])
  const decoded = decodePng(encodePng(2, 2, square))
  assert.deepEqual([...decoded.pixels], [...square])
})
