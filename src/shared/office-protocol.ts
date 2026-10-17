/** The desktop bridge for office documents: PDF copies made by the installed Office, and opening files in their own app. */

/** A conversion's PDF, or why there is none; `missing` means neither Microsoft Office nor WPS Office is installed. */
export type OfficePdfResult = { pdf: Uint8Array; application: string } | { error: string; missing: boolean }

/** Exposed by the Electron preload as `__RAINY_OFFICE__`; absent in a plain browser. */
export interface OfficeBridge {
  /**
   * @param bytes File contents, read through the Host so files in WSL and on SSH servers convert too.
   * @param extension Lower-case extension without the dot.
   */
  toPdf(bytes: Uint8Array, extension: string): Promise<OfficePdfResult>
  /** Open a document in the app Windows associates with it; absent on an SSH server. @param path Host path. */
  open?(path: string): Promise<{ error?: string }>
}
