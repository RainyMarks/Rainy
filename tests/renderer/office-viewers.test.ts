/** Office documents and Windows bookkeeping files: which viewer they get, lock-file owners and shortcut targets. */
import { describe, expect, it } from 'vitest'
import { officeLockOwner, shortcutFacts } from '../../src/renderer/ide/viewers/file-facts.ts'
import { chooseViewer } from '../../src/renderer/ide/viewers/viewer-choice.ts'
import { ideIsMedia, idePreviewKind } from '../../src/shared/ide-preview.ts'

/** A shell link with a target ID list, LinkInfo with a local base path, and the given string data. */
function shortcut(target: string, strings: { description?: string; workingDirectory?: string; arguments?: string } = {}): Uint8Array {
  const parts: number[] = []
  const u16 = (value: number): void => { parts.push(value & 0xff, value >> 8) }
  const u32 = (value: number): void => { u16(value & 0xffff); u16(value >>> 16) }
  const flags = 0x01 | 0x02 | 0x80 | (strings.description !== undefined ? 0x04 : 0) | (strings.workingDirectory !== undefined ? 0x10 : 0) | (strings.arguments !== undefined ? 0x20 : 0)
  u32(0x4c)
  parts.push(0x01, 0x14, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0xc0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46)
  u32(flags)
  while (parts.length < 0x4c) parts.push(0)
  u16(4); parts.push(2, 0, 0, 0)
  const path = [...new TextEncoder().encode(target), 0]
  const header = 0x1c
  u32(header + path.length + 1); u32(header); u32(1); u32(0); u32(header); u32(0); u32(header + path.length)
  parts.push(...path, 0)
  const text = (value: string | undefined): void => {
    if (value === undefined) return
    u16(value.length)
    for (const unit of value) u16(unit.charCodeAt(0))
  }
  text(strings.description); text(strings.workingDirectory); text(strings.arguments)
  return Uint8Array.from(parts)
}

function lockFile(name: string): Uint8Array {
  const bytes = new Uint8Array(162)
  bytes[0] = name.length
  bytes.set(new TextEncoder().encode(name), 1)
  bytes[0x36] = name.length
  for (let index = 0; index < name.length; index++) bytes[0x38 + index * 2] = name.charCodeAt(index)
  return bytes
}

describe('office file kinds', () => {
  it('previews documents, spreadsheets and slides instead of showing hex', () => {
    expect(['报告.docx', 'a.DOCM', 'b.xlsx', 'c.xls', 'd.ods', 'e.pptx', 'f.ppt', 'g.doc', 'h.lnk', '~$报告.docx', '~$e.pptx'].map(idePreviewKind))
      .toEqual(['word', 'word', 'sheet', 'sheet', 'sheet', 'office', 'office', 'office', 'shortcut', 'office-lock', 'office-lock'])
    expect(['a.docx', 'b.xlsx', 'c.pptx', 'd.lnk', '~$x.docx'].every(ideIsMedia)).toBe(true)
    expect(idePreviewKind('~$notes.txt')).toBeUndefined()
    expect(chooseViewer('slides.pptx', 'media', undefined, false)).toEqual({ editor: false, viewer: 'office', split: false })
    expect(chooseViewer('report.docx', 'media', undefined, false).viewer).toBe('word')
    expect(chooseViewer('report.docx', 'media', 'hex', false).viewer).toBe('hex')
  })
})

describe('office lock files', () => {
  it('reads the owner, preferring the UTF-16 copy', () => {
    expect(officeLockOwner(lockFile('ROG'))).toBe('ROG')
    const chinese = new Uint8Array(162)
    chinese[0x36] = 2
    chinese.set([0x20, 0x5f, 0x09, 0x4e], 0x38)
    expect(officeLockOwner(chinese)).toBe('张三')
  })

  it('rejects files that do not look like owner files', () => {
    expect(officeLockOwner(new Uint8Array(162))).toBeUndefined()
    expect(officeLockOwner(new Uint8Array([200, 1, 2]))).toBeUndefined()
  })
})

describe('shortcuts', () => {
  it('reads the target path and the string data', () => {
    expect(shortcutFacts(shortcut('C:\Program Files\App\app.exe', { workingDirectory: 'C:\Program Files\App', arguments: '--flag' })))
      .toEqual({ target: 'C:\Program Files\App\app.exe', workingDirectory: 'C:\Program Files\App', arguments: '--flag', description: undefined })
    expect(shortcutFacts(shortcut('D:\tool.exe', { description: 'Tool' }))?.description).toBe('Tool')
  })

  it('rejects other files', () => {
    expect(shortcutFacts(new Uint8Array(100))).toBeUndefined()
    expect(shortcutFacts(Uint8Array.from([0x4c, 0, 0, 0]))).toBeUndefined()
  })
})
