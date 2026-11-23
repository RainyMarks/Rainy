/** The desktop bridge for office documents: PDF copies made by the installed Office, and opening files in their own app. */
import type { IdeRootId, WorkspaceId } from './ide-files-protocol.ts'

/** Why a conversion made no PDF (`native/crates/app/src/web/office.rs`); the page explains each in the reader's language. */
export type OfficeFailureCode = 'missing' | 'password' | 'too-large' | 'timeout' | 'no-output' | 'unsupported' | 'failed'

/** Project file locator for zero-Base64 Office → PDF conversion across the GPUI / Host boundary. */
export interface OfficePdfSource {
  readonly workspaceId: WorkspaceId
  readonly rootId?: IdeRootId | undefined
  readonly path: string
  readonly version?: string | undefined
  /** Lower-case extension without the dot. */
  readonly extension: string
}

/** A conversion's PDF, or why there is none; `missing` means neither Microsoft Office nor WPS Office is installed, and
 *  `encrypted` that the file needs a password. `error` is the detail as Office or Windows put it, possibly English. */
export type OfficePdfResult = { pdf: Uint8Array; application: string }
  | { code?: OfficeFailureCode | undefined; error: string; missing: boolean; encrypted?: boolean | undefined }

/** Exposed by the viewer page of the desktop (`ViewerApp.tsx`) as `__RAINY_OFFICE__`; absent in a plain browser. */
export interface OfficeBridge {
  /**
   * Convert an Office document to PDF.
   * Passing an {@link OfficePdfSource} avoids Base64 round-trips completely: GPUI streams the source from the Host and
   * registers a one-shot `/rainy/ide/office-pdf?token=...` route that the page fetches as binary `ArrayBuffer`.
   * A raw `Uint8Array` + `extension` remains accepted as a fallback for legacy callers.
   */
  toPdf(source: OfficePdfSource): Promise<OfficePdfResult>
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
