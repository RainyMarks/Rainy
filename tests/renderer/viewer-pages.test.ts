/** Viewer page helpers: the document of an Office lock file, load failures in the reader's words and the zoom keys. */
import { describe, expect, it } from 'vitest'
import { FileChangedError, HttpStatusError } from '../../src/renderer/ide/viewers/byte-source.ts'
import { lockedDocumentName } from '../../src/renderer/ide/viewers/file-facts.ts'
import { loadFailure } from '../../src/renderer/ide/viewers/load-errors.ts'
import { viewerMessages } from '../../src/renderer/ide/viewers/messages.ts'
import { zoomKey } from '../../src/renderer/ide/viewers/zoom-keys.ts'

const file = (name: string): { name: string; kind: string } => ({ name, kind: 'file' })

describe('lockedDocumentName', () => {
  it('finds a short name kept whole and a long one whose first two characters Office replaced', () => {
    expect(lockedDocumentName('~$a.docx', [file('~$a.docx'), file('a.docx')])).toBe('a.docx')
    expect(lockedDocumentName('~$季度汇报材料.pptx', [file('~$季度汇报材料.pptx'), file('第三季度汇报材料.pptx'), file('季度汇报材料.pptx.bak')])).toBe('第三季度汇报材料.pptx')
    expect(lockedDocumentName('~$cument1.docx', [{ name: 'Document1.docx', kind: 'directory' }, file('~$cument1.docx')])).toBeUndefined()
    expect(lockedDocumentName('~$port.xlsx', [file('~$port.xlsx')])).toBeUndefined()
  })
})

describe('loadFailure', () => {
  const t = viewerMessages.t
  it('explains the byte route statuses and keeps the original text as the detail', () => {
    expect(loadFailure(new HttpStatusError(404), t)).toEqual({ reason: t('pdfErrorMissing'), detail: 'HTTP 404' })
    expect(loadFailure(new HttpStatusError(409), t).reason).toBe(t('pdfErrorChanged'))
    expect(loadFailure(new FileChangedError(), t).reason).toBe(t('pdfErrorChanged'))
    expect(loadFailure(new HttpStatusError(503), t).reason).toBe(t('pdfErrorBusy'))
    expect(loadFailure(new HttpStatusError(500), t).reason).toBe(t('pdfErrorHttp', { status: 500 }))
    expect(loadFailure(new TypeError('Failed to fetch'), t).reason).toBe(t('pdfErrorNetwork'))
  })

  it('reads a parse error of a document library as a damaged file', () => {
    const failure = loadFailure(new Error("Can't find end of central directory : is this a zip file ?"), t)
    expect(failure).toEqual({ reason: t('fileDamaged'), detail: "Can't find end of central directory : is this a zip file ?" })
    expect(failure.reason).not.toMatch(/central directory/u)
  })
})

describe('zoomKey', () => {
  const key = (value: string, ctrlKey = true, altKey = false): ReturnType<typeof zoomKey> => zoomKey({ key: value, ctrlKey, metaKey: false, altKey })
  it('takes Ctrl with = + - 0 and nothing else', () => {
    expect([key('='), key('+'), key('-'), key('0')]).toEqual(['in', 'in', 'out', 'reset'])
    expect([key('=', false), key('0', true, true), key('c'), key('9')]).toEqual([undefined, undefined, undefined, undefined])
  })
})
