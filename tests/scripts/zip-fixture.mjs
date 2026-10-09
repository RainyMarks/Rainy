/** Write small ZIP archives for script tests: deflated entries with UTF-8 names. */
import { crc32, deflateRawSync } from 'node:zlib'

/**
 * @param {Record<string, Uint8Array> | [string, Uint8Array][]} files Entry names and contents, in archive order.
 * @param {{method?: 0 | 8}} [options] Compression method for every entry.
 * @returns {Buffer} ZIP archive.
 */
export function zipSync(files, options = {}) {
  const method = options.method ?? 8
  const locals = []
  const central = []
  let offset = 0
  for (const [name, contents] of Array.isArray(files) ? files : Object.entries(files)) {
    const data = Buffer.from(contents)
    const stored = method === 8 ? deflateRawSync(data) : data
    const encodedName = Buffer.from(name, 'utf8')
    const local = Buffer.alloc(30)
    local.writeUInt32LE(0x04034b50, 0)
    local.writeUInt16LE(20, 4)
    local.writeUInt16LE(0x800, 6)
    local.writeUInt16LE(method, 8)
    local.writeUInt32LE(crc32(data), 14)
    local.writeUInt32LE(stored.length, 18)
    local.writeUInt32LE(data.length, 22)
    local.writeUInt16LE(encodedName.length, 26)
    const entry = Buffer.alloc(46)
    entry.writeUInt32LE(0x02014b50, 0)
    entry.writeUInt16LE(20, 4)
    entry.writeUInt16LE(20, 6)
    entry.writeUInt16LE(0x800, 8)
    entry.writeUInt16LE(method, 10)
    entry.writeUInt32LE(crc32(data), 16)
    entry.writeUInt32LE(stored.length, 20)
    entry.writeUInt32LE(data.length, 24)
    entry.writeUInt16LE(encodedName.length, 28)
    entry.writeUInt32LE(offset, 42)
    locals.push(local, encodedName, stored)
    central.push(entry, encodedName)
    offset += local.length + encodedName.length + stored.length
  }
  const directory = Buffer.concat(central)
  const end = Buffer.alloc(22)
  end.writeUInt32LE(0x06054b50, 0)
  end.writeUInt16LE(central.length / 2, 8)
  end.writeUInt16LE(central.length / 2, 10)
  end.writeUInt32LE(directory.length, 12)
  end.writeUInt32LE(offset, 16)
  return Buffer.concat([...locals, directory, end])
}
