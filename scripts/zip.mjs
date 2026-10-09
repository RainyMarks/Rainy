/** Read ZIP archives held in memory: stored and deflated entries, ZIP64 sizes and offsets, checked CRC-32. */
import { crc32, inflateRawSync } from 'node:zlib'

const END_OF_DIRECTORY = 0x06054b50
const ZIP64_LOCATOR = 0x07064b50
const ZIP64_END_OF_DIRECTORY = 0x06064b50
const DIRECTORY_ENTRY = 0x02014b50
const LOCAL_HEADER = 0x04034b50

/**
 * @param {Buffer} bytes Archive.
 * @returns {{count: number, offset: number}} Entry count and central directory offset.
 */
function centralDirectory(bytes) {
  for (let at = bytes.length - 22; at >= Math.max(0, bytes.length - 22 - 0xffff); at--) {
    if (bytes.readUInt32LE(at) !== END_OF_DIRECTORY) continue
    let count = bytes.readUInt16LE(at + 10)
    let offset = bytes.readUInt32LE(at + 16)
    if ((count === 0xffff || offset === 0xffffffff) && at >= 20 && bytes.readUInt32LE(at - 20) === ZIP64_LOCATOR) {
      const record = Number(bytes.readBigUInt64LE(at - 12))
      if (bytes.readUInt32LE(record) !== ZIP64_END_OF_DIRECTORY) throw new Error('Invalid ZIP64 end of central directory')
      count = Number(bytes.readBigUInt64LE(record + 32))
      offset = Number(bytes.readBigUInt64LE(record + 48))
    }
    return { count, offset }
  }
  throw new Error('ZIP end of central directory not found')
}

/**
 * List and decompress every entry in archive order; duplicate names are returned as separate entries.
 * @param {Uint8Array} archive ZIP archive.
 * @returns {{name: string, data: Buffer}[]} Entries; directory names end with `/` and have empty data.
 */
export function readZip(archive) {
  const bytes = Buffer.from(archive.buffer, archive.byteOffset, archive.byteLength)
  const { count, offset } = centralDirectory(bytes)
  const entries = []
  for (let index = 0, at = offset; index < count; index++) {
    if (bytes.readUInt32LE(at) !== DIRECTORY_ENTRY) throw new Error('Invalid ZIP central directory entry')
    const flags = bytes.readUInt16LE(at + 8)
    const method = bytes.readUInt16LE(at + 10)
    const checksum = bytes.readUInt32LE(at + 16)
    let compressedSize = bytes.readUInt32LE(at + 20)
    let size = bytes.readUInt32LE(at + 24)
    const nameLength = bytes.readUInt16LE(at + 28)
    const extraLength = bytes.readUInt16LE(at + 30)
    const commentLength = bytes.readUInt16LE(at + 32)
    let local = bytes.readUInt32LE(at + 42)
    const name = bytes.toString(flags & 0x800 ? 'utf8' : 'latin1', at + 46, at + 46 + nameLength)
    // ZIP64 extra field: original size, compressed size and local header offset, each present only when saturated.
    for (let extra = at + 46 + nameLength, end = extra + extraLength; extra + 4 <= end;) {
      const id = bytes.readUInt16LE(extra)
      const length = bytes.readUInt16LE(extra + 2)
      if (id === 0x0001) {
        let field = extra + 4
        if (size === 0xffffffff) { size = Number(bytes.readBigUInt64LE(field)); field += 8 }
        if (compressedSize === 0xffffffff) { compressedSize = Number(bytes.readBigUInt64LE(field)); field += 8 }
        if (local === 0xffffffff) local = Number(bytes.readBigUInt64LE(field))
      }
      extra += 4 + length
    }
    at += 46 + nameLength + extraLength + commentLength
    if (flags & 0x1) throw new Error(`Encrypted ZIP entry: ${name}`)
    if (bytes.readUInt32LE(local) !== LOCAL_HEADER) throw new Error(`Invalid ZIP local header: ${name}`)
    const start = local + 30 + bytes.readUInt16LE(local + 26) + bytes.readUInt16LE(local + 28)
    const stored = bytes.subarray(start, start + compressedSize)
    if (stored.length !== compressedSize) throw new Error(`Truncated ZIP entry: ${name}`)
    let data
    if (method === 0) data = Buffer.from(stored)
    else if (method === 8) data = inflateRawSync(stored)
    else throw new Error(`Unsupported ZIP compression method ${method}: ${name}`)
    if (data.length !== size || crc32(data) !== checksum) throw new Error(`ZIP entry checksum differs: ${name}`)
    entries.push({ name, data })
  }
  return entries
}
