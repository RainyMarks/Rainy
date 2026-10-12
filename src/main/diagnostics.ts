/**
 * "Export diagnostics": one zip with versions, the carrier and Strata logs and the newest local crash dumps, for the user
 * to attach to a report. Nothing is uploaded. Logs are cut to their end and scrubbed of keys, tokens, passwords and the
 * Windows user name; settings, chats and API keys are never included.
 */
import { open, readdir, stat, writeFile } from 'node:fs/promises'
import { homedir, userInfo } from 'node:os'
import { join } from 'node:path'
import { crc32, deflateRawSync } from 'node:zlib'

/** One file to include. */
export interface DiagnosticFile {
  /** Path inside the zip, with `/` separators. */
  readonly name: string
  readonly path: string
  /** Only the last bytes of a longer file are kept. */
  readonly maxBytes: number
  /** Text to scrub of secrets; binary crash dumps are copied as they are. */
  readonly text: boolean
}

const SECRETS: readonly [RegExp, string][] = [
  [/([?&](?:token|key|api[_-]?key|access_token|auth)=)[^&\s"']+/giu, '$1***'],
  [/\b(Bearer|Basic)\s+[\w.~+/=-]{8,}/gu, '$1 ***'],
  [/\b(sk-(?:ant-)?)[\w-]{8,}/gu, '$1***'],
  [/("?(?:api[_-]?key|apikey|authorization|password|passwd|secret|token|x-api-key)"?\s*[:=]\s*)("[^"]*"|'[^']*'|[^\s,;}]+)/giu, '$1***'],
  [/(\w+:\/\/[^\s:/@]+:)[^\s@/]+@/gu, '$1***@'],
]

/**
 * Remove secrets and the Windows user name from log text.
 * @param text Log text.
 * @param user Account name and home directory to replace.
 * @returns The scrubbed text.
 */
export function scrub(text: string, user: { name: string; home: string } = { name: userInfo().username, home: homedir() }): string {
  let result = text
  for (const [pattern, replacement] of SECRETS) result = result.replace(pattern, replacement)
  const escape = (value: string): string => value.replace(/[.*+?^${}()|[\]\\]/gu, '\\$&')
  if (user.home.length > 3) result = result.replace(new RegExp(escape(user.home), 'giu'), '~')
  if (user.name.length > 1) result = result.replace(new RegExp(`(?<=[\\\\/])${escape(user.name)}(?=[\\\\/])`, 'giu'), '<user>')
  return result
}

/**
 * @param directories Folders to look in; a missing folder has no files.
 * @param pattern File names to include.
 * @param count Newest files to return.
 * @returns Full paths, newest first across all folders.
 */
export async function newestFiles(directories: readonly string[], pattern: RegExp, count: number): Promise<string[]> {
  const found: { path: string; modified: number }[] = []
  for (const directory of directories) {
    let names: string[]
    try { names = await readdir(directory) } catch (error) {
      if ((error as NodeJS.ErrnoException).code === 'ENOENT') continue
      throw error
    }
    for (const name of names.filter(entry => pattern.test(entry))) {
      const path = join(directory, name)
      const info = await stat(path).catch(() => undefined)
      if (info?.isFile() === true) found.push({ path, modified: info.mtimeMs })
    }
  }
  return found.sort((a, b) => b.modified - a.modified).slice(0, count).map(file => file.path)
}

async function tail(path: string, maxBytes: number): Promise<Buffer | undefined> {
  let handle
  try { handle = await open(path, 'r') } catch (error) {
    if ((error as NodeJS.ErrnoException).code === 'ENOENT') return undefined
    throw error
  }
  try {
    const { size } = await handle.stat()
    const length = Math.min(size, maxBytes)
    const buffer = Buffer.alloc(length)
    await handle.read(buffer, 0, length, size - length)
    return buffer
  } finally {
    await handle.close()
  }
}

function dosTime(date: Date): { time: number; day: number } {
  return {
    time: (date.getHours() << 11) | (date.getMinutes() << 5) | Math.floor(date.getSeconds() / 2),
    day: ((Math.max(1980, date.getFullYear()) - 1980) << 9) | ((date.getMonth() + 1) << 5) | date.getDate(),
  }
}

/**
 * Pack files into a zip: deflate, UTF-8 names, no ZIP64 (the export stays far below 4 GiB).
 * @param entries Names and contents.
 * @param now Modification time recorded for every entry.
 * @returns The archive.
 */
export function zip(entries: readonly { name: string; data: Buffer }[], now = new Date()): Buffer {
  const { time, day } = dosTime(now)
  const locals: Buffer[] = []
  const central: Buffer[] = []
  let offset = 0
  for (const entry of entries) {
    const name = Buffer.from(entry.name, 'utf8')
    const packed = deflateRawSync(entry.data)
    const crc = crc32(entry.data)
    const local = Buffer.alloc(30)
    local.writeUInt32LE(0x04034b50, 0)
    local.writeUInt16LE(20, 4)
    local.writeUInt16LE(0x0800, 6)
    local.writeUInt16LE(8, 8)
    local.writeUInt16LE(time, 10)
    local.writeUInt16LE(day, 12)
    local.writeUInt32LE(crc, 14)
    local.writeUInt32LE(packed.length, 18)
    local.writeUInt32LE(entry.data.length, 22)
    local.writeUInt16LE(name.length, 26)
    locals.push(local, name, packed)
    const header = Buffer.alloc(46)
    header.writeUInt32LE(0x02014b50, 0)
    header.writeUInt16LE(20, 4)
    header.writeUInt16LE(20, 6)
    header.writeUInt16LE(0x0800, 8)
    header.writeUInt16LE(8, 10)
    header.writeUInt16LE(time, 12)
    header.writeUInt16LE(day, 14)
    header.writeUInt32LE(crc, 16)
    header.writeUInt32LE(packed.length, 20)
    header.writeUInt32LE(entry.data.length, 24)
    header.writeUInt16LE(name.length, 28)
    header.writeUInt32LE(offset, 42)
    central.push(header, name)
    offset += local.length + name.length + packed.length
  }
  const directory = Buffer.concat(central)
  const end = Buffer.alloc(22)
  end.writeUInt32LE(0x06054b50, 0)
  end.writeUInt16LE(entries.length, 8)
  end.writeUInt16LE(entries.length, 10)
  end.writeUInt32LE(directory.length, 12)
  end.writeUInt32LE(offset, 16)
  return Buffer.concat([...locals, directory, end])
}

/**
 * Write the diagnostics zip.
 * @param target Destination file.
 * @param about Versions and environment, written to `about.txt`.
 * @param files Logs and crash dumps; missing files are skipped.
 * @returns The names packed.
 */
export async function exportDiagnostics(target: string, about: Readonly<Record<string, string>>, files: readonly DiagnosticFile[]): Promise<string[]> {
  const entries: { name: string; data: Buffer }[] = [{ name: 'about.txt', data: Buffer.from(Object.entries(about).map(([key, value]) => `${key}: ${scrub(value)}`).join('\n') + '\n', 'utf8') }]
  for (const file of files) {
    const data = await tail(file.path, file.maxBytes)
    if (data === undefined) continue
    entries.push({ name: file.name, data: file.text ? Buffer.from(scrub(data.toString('utf8')), 'utf8') : data })
  }
  await writeFile(target, zip(entries))
  return entries.map(entry => entry.name)
}
