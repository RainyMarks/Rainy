/** Values the Host injects into `index.html` and the bridges the native window adds to its WebView2 pages. */
declare global {
  interface Window {
    __RAINY_AGENT__?: { name: 'Rainy'; version: string; environment: 'Windows' | 'WSL' | 'SSH'; label?: string }
  }
}

declare global {
  interface Window {
    /** Office conversion and "open in default app"; absent outside the desktop app. */
    __RAINY_OFFICE__?: import('../shared/office-protocol.ts').OfficeBridge
    /** Saves an exported document through the native save dialog, printing it to PDF first; absent outside the desktop app. */
    __RAINY_EXPORT__?: {
      save(request: { format: 'pdf' | 'html'; html: string; name: string; directory?: string | undefined }): Promise<{ path: string } | null>
      /** Show a saved file selected in Explorer. */
      showInFolder(path: string): Promise<void>
    }
  }
}

export {}
