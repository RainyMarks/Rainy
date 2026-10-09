/** Task-oriented display groups for the human tool directory; the signed catalog keeps its coarse categories. */
import type { NativeToolSummary } from '../native-tools-protocol.ts'
import type { zh } from './locales.ts'

/** Display group shown as a filter and as a heading in the complete list. */
export type ToolGroup = 'web' | 'traffic' | 'reverse' | 'forensics' | 'stego' | 'audio' | 'data' | 'other'

/** Groups in display order with their localized titles. */
export const toolGroups: readonly { readonly id: ToolGroup; readonly copyKey: keyof typeof zh }[] = [
  { id: 'web', copyKey: 'toolsGroupWeb' }, { id: 'traffic', copyKey: 'toolsGroupTraffic' },
  { id: 'reverse', copyKey: 'toolsGroupReverse' }, { id: 'forensics', copyKey: 'toolsGroupForensics' },
  { id: 'stego', copyKey: 'toolsGroupStego' }, { id: 'audio', copyKey: 'toolsGroupAudio' },
  { id: 'data', copyKey: 'toolsGroupData' }, { id: 'other', copyKey: 'toolsGroupOther' },
]

const known: Readonly<Record<string, ToolGroup>> = {
  yakit: 'web', bruno: 'web', curl: 'web',
  wireshark: 'traffic', pcapfix: 'traffic',
  ida: 'reverse', x64dbg: 'reverse', die: 'reverse', dnspy: 'reverse', jadx: 'reverse', imhex: 'reverse', 'pyinstxtractor-ng': 'reverse',
  binwalk: 'forensics', exiftool: 'forensics', '7zip': 'forensics', 'gnu-strings': 'forensics', qpdf: 'forensics',
  winmerge: 'forensics', ripgrep: 'forensics',
  stegsolve: 'stego', 'image-lsb-viewer': 'stego', pngcheck: 'stego', tweakpng: 'stego', imagemagick: 'stego', gimp: 'stego',
  'tesseract-ocr': 'stego', qrazybox: 'stego',
  audacity: 'audio', 'sonic-visualiser': 'audio', sox: 'audio', 'multimon-ng': 'audio', ffmpeg: 'audio',
  cyberchef: 'data', qalculate: 'data', jq: 'data', yq: 'data', sqlite: 'data', 'sqlite-browser': 'data',
}

/**
 * Place a tool in its display group.
 * @param tool - catalog summary; tools added by later catalogs fall back to their signed category.
 * @returns the group used for filtering and headings.
 */
export function toolGroup(tool: Pick<NativeToolSummary, 'id' | 'category'>): ToolGroup {
  const group = Object.hasOwn(known, tool.id) ? known[tool.id] : undefined
  return group ?? (tool.category === 'misc' ? 'other' : tool.category)
}
