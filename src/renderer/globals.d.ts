/** The bridges the native window adds to its WebView2 pages (`viewer/ViewerApp.tsx`). */
declare global {
  interface Window {
    /** Office conversion and "open in default app"; absent outside the desktop app. */
    __RAINY_OFFICE__?: import('../shared/office-protocol.ts').OfficeBridge
    /** Saves an exported document through the native save dialog, printing it to PDF first; absent outside the desktop app. */
    __RAINY_EXPORT__?: {
      save(request: { format: 'pdf' | 'html'; html: string; name: string }): Promise<{ path: string } | null>
    }
  }
}

export {}
