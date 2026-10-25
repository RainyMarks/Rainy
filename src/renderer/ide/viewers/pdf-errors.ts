/** Why a PDF cannot be opened, in the reader's words: pdf.js and the loader of pdf.js fail with English messages, which
 *  are kept only as the detail behind a localized reason chosen by the error's name. */

/** pdf.js itself could not be loaded from the resources the Host serves. */
export class PdfJsUnavailableError extends Error {
  override name = 'PdfJsUnavailableError'
}

/** Message keys of the reasons, in `messages.ts`. */
export type PdfErrorKey =
  | 'pdfErrorLibrary' | 'pdfErrorInvalid' | 'pdfErrorDamaged' | 'pdfErrorMissing' | 'pdfErrorChanged' | 'pdfErrorBusy'
  | 'pdfErrorNetwork' | 'pdfErrorHttp' | 'pdfErrorPassword' | 'pdfErrorUnknown'

/** A reason to show, and the original message for its tooltip. */
export interface PdfErrorText {
  readonly key: PdfErrorKey
  readonly vars?: { readonly status: number }
  /** The original message, as pdf.js wrote it. */
  readonly detail: string
  /** Whether the reason says too little without the detail, which is then shown as a line of its own. */
  readonly detailed: boolean
}

function field(error: unknown, name: string): unknown {
  return typeof error === 'object' && error !== null && name in error ? (error as Record<string, unknown>)[name] : undefined
}

/**
 * pdf.js 2.16 names its errors (`PasswordException`, `InvalidPDFException`, `MissingPDFException`,
 * `UnexpectedResponseException` with `status`, `UnknownErrorException`, `FormatError`) and rebuilds the errors of its
 * worker by name, so the name is what counts, not the class.
 * @param error What loading or reading the PDF threw.
 * @returns The reason to show.
 */
export function describePdfError(error: unknown): PdfErrorText {
  const name = String(field(error, 'name') ?? '')
  const message = field(error, 'message')
  const detail = typeof message === 'string' ? message : String(error)
  const reason = (key: PdfErrorKey, vars?: { status: number }): PdfErrorText =>
    ({ key, ...vars === undefined ? {} : { vars }, detail, detailed: key === 'pdfErrorDamaged' || key === 'pdfErrorUnknown' })
  switch (name) {
    case 'PdfJsUnavailableError': return reason('pdfErrorLibrary')
    case 'InvalidPDFException': return reason('pdfErrorInvalid')
    case 'FormatError': case 'UnknownErrorException': return reason('pdfErrorDamaged')
    case 'MissingPDFException': return reason('pdfErrorMissing')
    case 'PasswordException': return reason('pdfErrorPassword')
    case 'UnexpectedResponseException': {
      const status = field(error, 'status')
      const code = typeof status === 'number' ? status : 0
      // The byte route (`native/crates/host/src/ide/raw.rs`) answers 409 when the file is no longer the version the
      // address names, and 503 while the Host is closing or busy.
      if (code === 404 || code === 410) return reason('pdfErrorMissing')
      if (code === 409 || code === 412) return reason('pdfErrorChanged')
      if (code === 503) return reason('pdfErrorBusy')
      return code === 0 ? reason('pdfErrorNetwork') : reason('pdfErrorHttp', { status: code })
    }
    default: break
  }
  // The worker script failed to start: "Setting up fake worker failed: …".
  if (/fake worker/i.test(detail)) return reason('pdfErrorLibrary')
  if (name === 'TypeError' && /fetch|network/i.test(detail)) return reason('pdfErrorNetwork')
  return reason('pdfErrorUnknown')
}
