/** Workbench glyphs drawn on the shared 16px, one-pixel, currentColor icon grid. */
import type { ReactNode } from 'react'
import type { IconProps } from './props.ts'
import { ICON_REGULAR_STROKE } from './index.tsx'

function Glyph({ size = 16, className, children }: IconProps & { readonly children: ReactNode }) {
  return <svg width={size} height={size} className={className} viewBox="0 0 16 16" fill="none" xmlns="http://www.w3.org/2000/svg"
    aria-hidden="true" stroke="currentColor" strokeWidth={ICON_REGULAR_STROKE} strokeLinecap="round" strokeLinejoin="round">{children}</svg>
}

/** Debug launch. */
export const IconBugOutline = (props: IconProps) => <Glyph {...props}>
  <path d="M6 5.5V5a2 2 0 0 1 4 0v.5" />
  <path d="M5.5 5.5h5A1.5 1.5 0 0 1 12 7v2.5a4 4 0 0 1-8 0V7a1.5 1.5 0 0 1 1.5-1.5Z" />
  <path d="M8 8.5v5" />
  <path d="M4 8.5H2M14 8.5h-2M4.3 11.4 2.5 12.5M11.7 11.4l1.8 1.1M4.4 6.3 3 5.2M11.6 6.3 13 5.2" />
</Glyph>

/** Bottom panel visibility. */
export const IconPanelBottomOutline = (props: IconProps) => <Glyph {...props}>
  <rect x="1.5" y="1.5" width="13" height="13" rx="1" />
  <path d="M1.5 10.5h13" />
</Glyph>

/** Project map: three boxes linked as a small architecture. */
export const IconMapOutline = (props: IconProps) => <Glyph {...props}>
  <rect x="5.5" y="1.5" width="5" height="3.5" rx="0.75" />
  <rect x="1.5" y="11" width="5" height="3.5" rx="0.75" />
  <rect x="9.5" y="11" width="5" height="3.5" rx="0.75" />
  <path d="M8 5v3M4 11V8.5h8V11" />
</Glyph>

/** Right panel visibility. */
export const IconPanelRightOutline = (props: IconProps) => <Glyph {...props}>
  <rect x="1.5" y="1.5" width="13" height="13" rx="1" />
  <path d="M10.5 1.5v13" />
</Glyph>

/** File explorer: a page with a folded corner over another. */
export const IconFilesOutline = (props: IconProps) => <Glyph {...props}>
  <path d="M5.5 1.5h4l3 3v7a1 1 0 0 1-1 1h-6a1 1 0 0 1-1-1v-9a1 1 0 0 1 1-1Z" />
  <path d="M9.5 1.5v3h3" />
  <path d="M2.5 4.5v9a1 1 0 0 0 1 1h6" />
</Glyph>

/** Source control: two commits on a branch joining a third. */
export const IconSourceControlOutline = (props: IconProps) => <Glyph {...props}>
  <circle cx="4.5" cy="3.5" r="1.5" />
  <circle cx="4.5" cy="12.5" r="1.5" />
  <circle cx="11.5" cy="5.5" r="1.5" />
  <path d="M4.5 5v6M11.5 7c0 2.5-2 3.5-5.5 4.2" />
</Glyph>
