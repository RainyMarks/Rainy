/** Diagnostics export: secrets scrubbed from logs, the newest crash dumps chosen, and a zip that standard readers open. */
import { mkdtemp, readFile, rm, utimes, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { crc32, inflateRawSync } from 'node:zlib'
import { afterEach, describe, expect, it } from 'vitest'
import { exportDiagnostics, newestFiles, scrub, zip } from '../../src/main/diagnostics.ts'

const folders: string[] = []
afterEach(async () => { for (const folder of folders.splice(0)) await rm(folder, { recursive: true, force: true }) })

async function folder(): Promise<string> {
  const path = await mkdtemp(join(tmpdir(), 'rainy-diagnostics-'))
  folders.push(path)
  return path
}

/** Read every entry back through the central directory, checking each CRC. */
function unzip(archive: Buffer): Record<string, string> {
  const end = archive.lastIndexOf(Buffer.from([0x50, 0x4b, 0x05, 0x06]))
  const count = archive.readUInt16LE(end + 10)
  let at = archive.readUInt32LE(end + 16)
  const files: Record<string, string> = {}
  for (let index = 0; index < count; index++) {
    expect(archive.readUInt32LE(at)).toBe(0x02014b50)
    const crc = archive.readUInt32LE(at + 16)
    const packedSize = archive.readUInt32LE(at + 20)
    const nameLength = archive.readUInt16LE(at + 28)
    const local = archive.readUInt32LE(at + 42)
    const name = archive.subarray(at + 46, at + 46 + nameLength).toString('utf8')
    const start = local + 30 + archive.readUInt16LE(local + 26) + archive.readUInt16LE(local + 28)
    const data = inflateRawSync(archive.subarray(start, start + packedSize))
    expect(crc32(data)).toBe(crc)
    files[name] = data.toString('utf8')
    at += 46 + nameLength
  }
  return files
}

describe('diagnostics export', () => {
  it('scrubs keys, tokens, passwords and the account name from logs', () => {
    const user = { name: 'alice', home: 'C:\\Users\\alice' }
    const text = [
      'GET http://127.0.0.1:4000/?token=abcdef123456 200',
      'Authorization: Bearer abcdefghijklmnop',
      'key sk-ant-api03-SECRETSECRET and sk-proj-12345678abc',
      '{"apiKey": "plain-secret", "password":"hunter22"}',
      'proxy http://me:pw@corp:3128 failed',
      'opened C:\\Users\\alice\\ctf\\solve.py and /home/alice/x',
    ].join('\n')
    const cleaned = scrub(text, user)
    for (const secret of ['abcdef123456', 'abcdefghijklmnop', 'SECRETSECRET', '12345678abc', 'plain-secret', 'hunter22', ':pw@', 'alice'])
      expect(cleaned, secret).not.toContain(secret)
    expect(cleaned).toContain('~\\ctf\\solve.py')
    expect(cleaned).toContain('/home/<user>/x')
  })

  it('picks the newest crash dumps and packs the tail of each log', async () => {
    const source = await folder()
    for (const [name, age] of [['old.dmp', 300], ['mid.dmp', 200], ['new.dmp', 100], ['notes.txt', 0]] as const) {
      const path = join(source, name)
      await writeFile(path, name)
      const time = new Date(Date.now() - age * 1000)
      await utimes(path, time, time)
    }
    expect((await newestFiles([source, join(source, 'missing')], /\.dmp$/u, 2)).map(path => path.slice(source.length + 1))).toEqual(['new.dmp', 'mid.dmp'])
    expect(await newestFiles([join(source, 'missing')], /\.dmp$/u, 2)).toEqual([])
    await writeFile(join(source, 'host.log'), `${'x'.repeat(100)}\nlast line token=abcdef123456\n`)
    const target = join(source, 'out.zip')
    const names = await exportDiagnostics(target, { RainyAgent: '2.1.0', 'Execution target': 'Windows' }, [
      { name: 'logs/host.log', path: join(source, 'host.log'), maxBytes: 40, text: true },
      { name: 'crash/new.dmp', path: join(source, 'new.dmp'), maxBytes: 1024, text: false },
      { name: 'logs/absent.log', path: join(source, 'absent.log'), maxBytes: 1024, text: true },
    ])
    expect(names).toEqual(['about.txt', 'logs/host.log', 'crash/new.dmp'])
    const files = unzip(await readFile(target))
    expect(files['about.txt']).toBe('RainyAgent: 2.1.0\nExecution target: Windows\n')
    // The last 40 bytes: the end of the long line, then the scrubbed last line.
    expect(files['logs/host.log']).toBe(`${'x'.repeat(10)}\nlast line token=***\n`)
    expect(files['crash/new.dmp']).toBe('new.dmp')
  })

  it('keeps names in UTF-8', () => {
    expect(unzip(zip([{ name: '日志/说明.txt', data: Buffer.from('内容') }]))).toEqual({ '日志/说明.txt': '内容' })
  })
})
