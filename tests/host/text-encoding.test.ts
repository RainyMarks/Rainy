import { describe, expect, it } from 'vitest'
import { decodeFile, encodeFile, roundTrips } from '../../src/host/agent/tools/text-encoding.ts'

/** "中文注释" followed by a newline, as Chinese Windows stores it. */
const GBK = Buffer.from([0xd6, 0xd0, 0xce, 0xc4, 0xd7, 0xa2, 0xca, 0xcd, 0x0a])

describe('file encodings', () => {
  it('reads UTF-8 with and without a byte-order mark', () => {
    expect(decodeFile(Buffer.from('a 中\n'))).toEqual({ text: 'a 中\n', encoding: 'utf-8', bom: false })
    const marked = Buffer.from('\ufeffa 中\n')
    expect(decodeFile(marked)).toEqual({ text: 'a 中\n', encoding: 'utf-8', bom: true })
    expect(Buffer.from(encodeFile({ encoding: 'utf-8', bom: true }, 'a 中\n')!).equals(marked)).toBe(true)
  })

  it('reads GBK text as GB18030 and writes it back byte for byte', () => {
    const decoded = decodeFile(GBK)!
    expect(decoded).toEqual({ text: '中文注释\n', encoding: 'gb18030', bom: false })
    expect(roundTrips(GBK, decoded)).toBe(true)
    expect(Buffer.from(encodeFile(decoded, 'int x; // 中文注释\n')!).toString('latin1')).toBe(`int x; // ${GBK.subarray(0, 8).toString('latin1')}\n`)
    // Characters outside GBK need four-byte codes, which the tools do not write.
    expect(encodeFile(decoded, '😀')).toBeUndefined()
  })

  it('does not mistake longer GBK text for damaged UTF-8', () => {
    const text = '/* 串口初始化：波特率 115200，数据位 8，停止位 1。 */\r\nvoid uart_init(void) { /* 配置时钟与引脚复用 */ }\r\n'
    const bytes = encodeFile({ encoding: 'gb18030', bom: false }, text.repeat(20))!
    const decoded = decodeFile(bytes)!
    expect(decoded).toMatchObject({ encoding: 'gb18030', text: text.repeat(20) })
    expect(roundTrips(bytes, decoded)).toBe(true)
  })

  it('reads and writes UTF-16 with a byte-order mark, as Windows PowerShell 5.1 redirects', () => {
    const little = Buffer.concat([Buffer.from([0xff, 0xfe]), Buffer.from('ok 中\r\n', 'utf16le')])
    const decoded = decodeFile(little)!
    expect(decoded).toMatchObject({ text: 'ok 中\r\n', encoding: 'utf-16le', bom: true })
    expect(roundTrips(little, decoded)).toBe(true)
    const big = Buffer.concat([Buffer.from([0xfe, 0xff]), Buffer.from('ok', 'utf16le').swap16()])
    expect(decodeFile(big)).toMatchObject({ text: 'ok', encoding: 'utf-16be' })
    expect(Buffer.from(encodeFile({ encoding: 'utf-16be', bom: true }, 'ok')!).equals(big)).toBe(true)
  })

  it('treats NUL bytes as binary', () => {
    expect(decodeFile(Buffer.from([0x50, 0x4b, 0x03, 0x04, 0x00, 0x00]))).toBeUndefined()
  })

  it('keeps a UTF-8 file with one damaged byte as UTF-8 that cannot be written back', () => {
    const bytes = Buffer.concat([Buffer.from('第一行中文内容\n第二行'), Buffer.from([0xff]), Buffer.from('结尾\n')])
    const decoded = decodeFile(bytes)!
    expect(decoded.encoding).toBe('utf-8')
    expect(decoded.damaged).toBe(true)
    expect(decoded.text).toContain('第一行')
    expect(roundTrips(bytes, decoded)).toBe(false)
  })
})
