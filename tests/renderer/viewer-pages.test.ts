/** Viewer page helpers: load failures in the reader's words and the zoom keys. */
import { describe, expect, it } from 'vitest'
import { FileChangedError, HttpStatusError } from '../../src/renderer/ide/viewers/byte-source.ts'
import { loadFailure } from '../../src/renderer/ide/viewers/load-errors.ts'
import { viewerMessages } from '../../src/renderer/ide/viewers/messages.ts'
import { zoomKey } from '../../src/renderer/ide/viewers/zoom-keys.ts'

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
