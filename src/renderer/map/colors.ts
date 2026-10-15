/** Colors of the project map: one per language family, defined as CSS variables in ProjectMap.module.css. */
import type { MapFileKind } from '../../shared/map-protocol.ts'

const FAMILY: Record<string, string> = {
  typescript: 'ts', vue: 'ts', svelte: 'ts', astro: 'ts',
  javascript: 'js',
  python: 'py', sage: 'py', jupyter: 'py',
  c: 'c', cpp: 'c', 'objective-c': 'c', cuda: 'c', assembly: 'c', zig: 'c',
  go: 'go', rust: 'rs', swift: 'rs', dart: 'rs',
  java: 'jvm', kotlin: 'jvm', scala: 'jvm', groovy: 'jvm', csharp: 'jvm', fsharp: 'jvm', vb: 'jvm',
  php: 'script', ruby: 'script', lua: 'script', perl: 'script', r: 'script', julia: 'script',
  shell: 'shell', powershell: 'shell', batch: 'shell',
  html: 'web', css: 'web', sql: 'web', graphql: 'web',
}

/**
 * @param language Language id of a file.
 * @param kind Its kind.
 * @returns A CSS color value.
 */
export function fileColor(language: string, kind: MapFileKind): string {
  if (kind === 'doc') return 'var(--map-doc)'
  if (kind === 'config') return 'var(--map-config)'
  if (kind === 'asset' || kind === 'other') return 'var(--map-other)'
  return `var(--map-${FAMILY[language] ?? 'code'})`
}
