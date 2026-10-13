/** Font families offered in Settings → Appearance, filtered to those this computer has. */

/** Interface fonts worth offering, Latin and Chinese, in display order. */
export const UI_FONT_CANDIDATES: readonly string[] = [
  'Inter', 'Microsoft YaHei', 'DengXian', 'HarmonyOS Sans SC', 'MiSans', 'OPPOSans', 'Alibaba PuHuiTi 3.0', 'Source Han Sans SC',
  'Noto Sans SC', 'Noto Sans CJK SC', 'Sarasa UI SC', 'LXGW WenKai', 'LXGW WenKai Screen', 'PingFang SC', 'SF Pro Text',
  'Helvetica Neue', 'Roboto', 'Open Sans', 'IBM Plex Sans', 'Source Sans 3', 'Noto Sans', 'Ubuntu', 'Arial', 'Calibri',
  'Verdana', 'Tahoma', 'Trebuchet MS', 'Bahnschrift', 'Georgia', 'Cambria', 'SimHei', 'SimSun', 'KaiTi', 'FangSong',
]

/** Code fonts worth offering, in display order. */
export const CODE_FONT_CANDIDATES: readonly string[] = [
  'Cascadia Code', 'Cascadia Mono', 'Consolas', 'JetBrains Mono', 'Fira Code', 'Fira Mono', 'Source Code Pro', 'Sarasa Mono SC',
  'Sarasa Term SC', 'Maple Mono', 'Maple Mono NF CN', 'LXGW WenKai Mono', 'Iosevka', 'Hack', 'IBM Plex Mono', 'Roboto Mono',
  'Victor Mono', 'Geist Mono', 'Monaspace Neon', 'Inconsolata', 'Ubuntu Mono', 'DejaVu Sans Mono', 'Noto Sans Mono',
  'Liberation Mono', 'SF Mono', 'Menlo', 'Monaco', 'MesloLGS NF', 'CaskaydiaCove Nerd Font', 'JetBrainsMono Nerd Font',
  'Lucida Console', 'Courier New',
]

/** Chinese names of families Windows lists by their English names, shown in the Chinese interface. */
export const FONT_NAMES_ZH: Readonly<Record<string, string>> = {
  'Microsoft YaHei': '微软雅黑', DengXian: '等线', SimHei: '黑体', SimSun: '宋体', KaiTi: '楷体', FangSong: '仿宋',
  'Source Han Sans SC': '思源黑体', 'HarmonyOS Sans SC': '鸿蒙黑体', 'Alibaba PuHuiTi 3.0': '阿里巴巴普惠体 3.0',
  'LXGW WenKai': '霞鹜文楷', 'LXGW WenKai Screen': '霞鹜文楷 屏幕阅读版', 'LXGW WenKai Mono': '霞鹜文楷等宽',
  'Sarasa UI SC': '更纱黑体 UI', 'Sarasa Mono SC': '更纱黑体 Mono', 'Sarasa Term SC': '更纱黑体 Term',
}

/** Latin, digits and Chinese, so a family covering any of them changes the width. */
const SAMPLE = 'mmmmmmmmmmlli1WQ@#汉字字体'
/** A missing family falls back to the generic one after it; one of these always differs from an installed family. */
const GENERIC_FAMILIES = ['monospace', 'sans-serif', 'serif'] as const

let context: CanvasRenderingContext2D | null | undefined
const known = new Map<string, boolean>()

/**
 * @param family A font family name.
 * @returns Whether the system has it: text set in it measures differently from the generic family it would fall back to.
 */
export function fontInstalled(family: string): boolean {
  const cached = known.get(family)
  if (cached !== undefined) return cached
  if (context === undefined) {
    try {
      context = typeof document === 'undefined' ? null : document.createElement('canvas').getContext('2d')
    } catch (_unsupported) {
      context = null
    }
  }
  if (context === null) return false
  const canvas = context
  const width = (font: string): number => { canvas.font = `40px ${font}`; return canvas.measureText(SAMPLE).width }
  const installed = GENERIC_FAMILIES.some(generic => width(`"${family}", ${generic}`) !== width(generic))
  known.set(family, installed)
  return installed
}

/**
 * @param candidates Families in display order.
 * @returns The installed ones, in the same order.
 */
export function installedFonts(candidates: readonly string[]): string[] {
  return candidates.filter(fontInstalled)
}
