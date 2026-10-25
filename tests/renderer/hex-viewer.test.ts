/** Hex view helpers restored from 3.x: format detection, hex text forms and byte paging. */
import { describe, expect, it } from 'vitest'
import { ByteSource, CHUNK_BYTES, FileChangedError } from '../../src/renderer/ide/viewers/byte-source.ts'
import { formatBytes, formatOffset, inspectBytes, parseOffset } from '../../src/renderer/ide/viewers/hex-format.ts'
import { detectSignature } from '../../src/renderer/ide/viewers/signature.ts'

const bytes = (...values: (number | string)[]): Uint8Array => {
  const out: number[] = []
  for (const value of values) {
    if (typeof value === 'number') out.push(value)
    else for (const char of value) out.push(char.charCodeAt(0))
  }
  return Uint8Array.from(out)
}

function elfHeader(bits: 1 | 2, type: number, machine: number): Uint8Array {
  const head = new Uint8Array(64)
  head.set([0x7f, 0x45, 0x4c, 0x46, bits, 1, 1])
  head[16] = type
  head[18] = machine & 0xff
  head[19] = machine >> 8
  return head
}

function peHeader(machine: number, magic: number, dll: boolean, clr: boolean): Uint8Array {
  const head = new Uint8Array(1024)
  head.set([0x4d, 0x5a])
  const pe = 0x80
  head[0x3c] = pe
  head.set([0x50, 0x45, 0, 0], pe)
  head[pe + 4] = machine & 0xff; head[pe + 5] = machine >> 8
  head[pe + 22] = 0x02; head[pe + 23] = dll ? 0x20 : 0
  head[pe + 24] = magic & 0xff; head[pe + 25] = magic >> 8
  const directories = pe + 24 + (magic === 0x20b ? 112 : 96)
  if (clr) head[directories + 14 * 8] = 0x08
  return head
}

describe('detectSignature', () => {
  it('names images, documents, archives and captures by their magic numbers', () => {
    expect(detectSignature(bytes(0x89, 'PNG', 0x0d, 0x0a, 0x1a, 0x0a))?.label).toBe('PNG image')
    expect(detectSignature(bytes(0xff, 0xd8, 0xff, 0xe0))?.label).toBe('JPEG image')
    expect(detectSignature(bytes('GIF89a'))?.label).toBe('GIF image (89a)')
    expect(detectSignature(bytes('%PDF-1.7'))?.label).toBe('PDF document, version 1.7')
    expect(detectSignature(bytes(0x1f, 0x8b, 8))?.label).toBe('gzip compressed data')
    expect(detectSignature(bytes(0xd4, 0xc3, 0xb2, 0xa1))?.label).toBe('pcap capture file')
    expect(detectSignature(bytes('SQLite format 3', 0))?.family).toBe('database')
    expect(detectSignature(bytes('-----BEGIN RSA PRIVATE KEY-----'))?.label).toBe('PEM RSA PRIVATE KEY')
    expect(detectSignature(bytes('#!/usr/bin/env python3\n'))?.label).toBe('Script (/usr/bin/env python3)')
    expect(detectSignature(bytes('hello'))).toBeUndefined()
  })

  it('tells Office documents, Java archives and plain Zip archives apart', () => {
    const local = (name: string): Uint8Array => {
      const head = new Uint8Array(30 + name.length)
      head.set([0x50, 0x4b, 3, 4])
      head[26] = name.length
      head.set([...name].map(char => char.charCodeAt(0)), 30)
      return head
    }
    expect(detectSignature(local('[Content_Types].xml'))?.label).toMatch(/^Office Open XML/u)
    expect(detectSignature(local('META-INF/MANIFEST.MF'))?.label).toBe('Java archive (JAR)')
    expect(detectSignature(local('AndroidManifest.xml'))?.label).toBe('Android package (APK)')
    expect(detectSignature(local('flag.txt'))?.label).toBe('Zip archive')
  })

  it('reads the class, byte order, type and machine of ELF files and the format of PE files', () => {
    expect(detectSignature(elfHeader(2, 3, 0x3e))?.label).toBe('ELF 64-bit LSB shared object, x86-64')
    expect(detectSignature(elfHeader(1, 2, 3))?.label).toBe('ELF 32-bit LSB executable, Intel 80386')
    expect(detectSignature(peHeader(0x8664, 0x20b, false, false))?.label).toBe('PE32+ executable, x86-64')
    expect(detectSignature(peHeader(0x14c, 0x10b, true, true))?.label).toBe('PE32 DLL, Intel 80386, .NET assembly')
    expect(detectSignature(bytes('MZ', 0, 0))?.label).toBe('MS-DOS executable')
    expect(detectSignature(bytes(0xca, 0xfe, 0xba, 0xbe, 0, 0, 0, 52))?.label).toBe('Java class file')
  })
})

describe('hex text forms', () => {
  it('parses hex with 0x, $ or h and decimal offsets', () => {
    expect([parseOffset('0x1F'), parseOffset('$10'), parseOffset('20h'), parseOffset('496'), parseOffset('1_000'), parseOffset('zz'), parseOffset('-1')])
      .toEqual([31, 16, 32, 496, 1000, undefined, undefined])
  })

  it('pads offsets to the width the file size needs', () => {
    expect(formatOffset(255, 1024)).toBe('000000FF')
    expect(formatOffset(1, 0x1_0000_0001)).toBe('000000001')
  })

  it('copies bytes as hex, text, Python, C and Base64', () => {
    const data = bytes('A', 0, 0x27, 0x5c, 0xff)
    expect(formatBytes(data, 'hex')).toBe('41 00 27 5c ff')
    expect(formatBytes(data, 'text')).toBe("A.'\\.")
    expect(formatBytes(data, 'python')).toBe("b'A\\x00\\x27\\x5c\\xff'")
    expect(formatBytes(data, 'c')).toBe('{ 0x41, 0x00, 0x27, 0x5c, 0xff }')
    expect(formatBytes(data, 'base64')).toBe('QQAnXP8=')
  })

  it('interprets the bytes at the cursor in both byte orders', () => {
    const data = bytes(0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0xf0, 0x3f)
    const little = Object.fromEntries(inspectBytes(data, true).map(row => [row.label, row.value]))
    expect(little).toMatchObject({ uint8: '1', uint16: '1', uint32: '1', binary: '00000001' })
    expect(Object.fromEntries(inspectBytes(bytes(0, 0, 0, 0, 0, 0, 0xf0, 0x3f), true).map(row => [row.label, row.value])).float64).toBe('1')
    expect(Object.fromEntries(inspectBytes(bytes(0x12, 0x34), false).map(row => [row.label, row.value])).uint16).toBe(String(0x1234))
    expect(Object.fromEntries(inspectBytes(bytes(0xe4, 0xb8, 0xad), true).map(row => [row.label, row.value]))['UTF-8']).toBe('中 (U+4E2D)')
  })
})


describe('ByteSource', () => {
  const fileBytes = Uint8Array.from({ length: CHUNK_BYTES * 2 + 100 }, (_unused, index) => index % 251)
  const fake = (etags: () => string): typeof fetch => async (_input, init) => {
    const range = /bytes=(\d+)-(\d+)/u.exec(new Headers(init?.headers).get('range') ?? '')
    const start = Number(range?.[1] ?? 0)
    const end = Number(range?.[2] ?? fileBytes.length - 1)
    return new Response(fileBytes.slice(start, end + 1), { status: 206, headers: { etag: etags() } })
  }

  it('reads across chunk boundaries and serves loaded bytes synchronously', async () => {
    const source = new ByteSource('/raw', fileBytes.length, 8, fake(() => '"v1"'))
    expect(source.byte(CHUNK_BYTES - 1)).toBeUndefined()
    const read = await source.read(CHUNK_BYTES - 2, 4)
    expect([...read]).toEqual([...fileBytes.subarray(CHUNK_BYTES - 2, CHUNK_BYTES + 2)])
    expect(source.byte(CHUNK_BYTES - 1)).toBe(fileBytes[CHUNK_BYTES - 1])
    expect((await source.read(fileBytes.length - 3, 10)).length).toBe(3)
  })

  it('reports a file that changed between two chunks', async () => {
    let version = 0
    const source = new ByteSource('/raw', fileBytes.length, 8, fake(() => `"v${version++}"`))
    await source.read(0, 1)
    await expect(source.read(CHUNK_BYTES, 1)).rejects.toBeInstanceOf(FileChangedError)
  })
})
