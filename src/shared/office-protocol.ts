/** The desktop bridge for office documents: PDF copies made by the installed Office, and opening files in their own app. */

/** Why a conversion made no PDF (`native/crates/app/src/web/office.rs`); the page explains each in the reader's language. */
export type OfficeFailureCode = 'missing' | 'password' | 'too-large' | 'timeout' | 'no-output' | 'unsupported' | 'failed'

/** A conversion's PDF, or why there is none; `missing` means neither Microsoft Office nor WPS Office is installed, and
 *  `encrypted` that the file needs a password. `error` is the detail as Office or Windows put it, possibly English. */
export type OfficePdfResult = { pdf: Uint8Array; application: string }
  | { code?: OfficeFailureCode | undefined; error: string; missing: boolean; encrypted?: boolean | undefined }

/** Exposed by the viewer page of the desktop (`ViewerApp.tsx`) as `__RAINY_OFFICE__`; absent in a plain browser. */
export interface OfficeBridge {
  /**
   * @param bytes File contents, read through the Host so files in WSL and on SSH servers convert too.
   * @param extension Lower-case extension without the dot.
   */
  toPdf(bytes: Uint8Array, extension: string): Promise<OfficePdfResult>
  /** Open a document in the app Windows associates with it; absent on an SSH server. @param path Host path. */
  open?(path: string): Promise<{ error?: string }>
}

/** Office Open XML formats; they are zip files unless a password encrypted them into a compound file. */
const OPEN_XML = new Set(['pptx', 'pptm', 'ppsx', 'ppsm', 'potx', 'docx', 'docm', 'dotx', 'dotm', 'xlsx', 'xlsm', 'xltx', 'xltm', 'xlsb'])

/**
 * @param bytes File contents, or at least their first 8 bytes.
 * @param extension Lower-case extension.
 * @returns Whether an Office Open XML file is password-encrypted.
 */
export function encryptedOpenXml(bytes: Uint8Array, extension: string): boolean {
  return OPEN_XML.has(extension) && [0xd0, 0xcf, 0x11, 0xe0, 0xa1, 0xb1, 0x1a, 0xe1].every((value, index) => bytes[index] === value)
}
