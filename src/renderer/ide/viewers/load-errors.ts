/** Why a viewer could not fetch or parse a file, in the reader's words. The Host's byte route answers with a status and
 *  the libraries that read documents (JSZip, docx-preview, SheetJS) throw English, so the original text is kept only
 *  as the detail behind a localized reason. */
import { FileChangedError, HttpStatusError } from './byte-source.ts'
import type { useViewerT } from './messages.ts'

type Translate = ReturnType<typeof useViewerT>

/** A reason to show and the original message for its tooltip or detail line. */
export interface LoadFailure {
  readonly reason: string
  readonly detail: string
}

/**
 * @param error What fetching or parsing threw.
 * @param t Viewer messages.
 * @returns The reason; a parse error of a document library reads as a damaged file.
 */
export function loadFailure(error: unknown, t: Translate): LoadFailure {
  const detail = error instanceof Error ? error.message : String(error)
  if (error instanceof FileChangedError) return { reason: t('pdfErrorChanged'), detail }
  if (error instanceof HttpStatusError) {
    // `native/crates/host/src/ide/raw.rs`: 409 when the file is no longer the version the address names, 503 while
    // the Host is busy or closing.
    const reason = error.status === 404 || error.status === 410 ? t('pdfErrorMissing')
      : error.status === 409 || error.status === 412 || error.status === 416 ? t('pdfErrorChanged')
        : error.status === 503 ? t('pdfErrorBusy') : t('pdfErrorHttp', { status: error.status })
    return { reason, detail }
  }
  if (error instanceof TypeError && /fetch|network/iu.test(detail)) return { reason: t('pdfErrorNetwork'), detail }
  return { reason: t('fileDamaged'), detail }
}
