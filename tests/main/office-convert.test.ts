/** Office conversion: which files go to Office, and the cache of recent results. */
import { describe, expect, it, vi } from 'vitest'
import { convertOfficeToPdf, OfficeConversionError, OfficeConverter, officeKind } from '../../src/main/office-convert.ts'

describe('office conversion', () => {
  it('sends slides to PowerPoint and documents to Word, and nothing else', async () => {
    expect([officeKind('pptx'), officeKind('ppt'), officeKind('doc'), officeKind('docx'), officeKind('xlsx'), officeKind('exe')])
      .toEqual(['slides', 'slides', 'word', 'word', undefined, undefined])
    await expect(convertOfficeToPdf(new Uint8Array([77, 90]), 'exe')).rejects.toBeInstanceOf(OfficeConversionError)
  })

  it('reuses a finished conversion of the same bytes and forgets failures', async () => {
    let fail = true
    const convert = vi.fn(async (bytes: Uint8Array) => {
      if (fail) throw new OfficeConversionError('busy', false)
      return { pdf: Buffer.from(bytes), application: 'Test' }
    })
    const converter = new OfficeConverter(convert, 2)
    await expect(converter.toPdf(new Uint8Array([1]), 'pptx')).rejects.toThrow('busy')
    fail = false
    await converter.toPdf(new Uint8Array([1]), 'pptx')
    await converter.toPdf(new Uint8Array([1]), 'pptx')
    expect(convert).toHaveBeenCalledTimes(2)
    await converter.toPdf(new Uint8Array([1]), 'ppt')
    await converter.toPdf(new Uint8Array([2]), 'pptx')
    await converter.toPdf(new Uint8Array([1]), 'pptx')
    expect(convert).toHaveBeenCalledTimes(5)
  })
})
