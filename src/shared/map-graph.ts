/**
 * Architecture of a project map: the boxes one directory (the scope) is split into, and the imports between them.
 * Both the page, which draws it, and the Host, which asks the AI to describe it, build the same boxes.
 */
import type { MapFile, MapFileKind, ProjectMap } from './map-protocol.ts'

/** A box: a directory, a single file, or the loose files directly inside a directory. */
export interface MapNode {
  /** A directory or file path; the loose files of directory `d` are `d/*` (`*` at the top). */
  readonly id: string
  readonly type: 'directory' | 'file' | 'loose'
  /** Outside the scope: a directory or file the scope's files import or are imported by. */
  readonly outside: boolean
  /** The kind most of its lines are. */
  readonly kind: MapFileKind
  /** Indexes into {@link ProjectMap.files}. */
  readonly files: readonly number[]
  readonly lines: number
  /** Lines per language, largest first. */
  readonly languages: readonly { readonly language: string; readonly lines: number }[]
  /** A directory with more than one file can be opened as the next scope. */
  readonly openable: boolean
}

/** Imports from the files of one box to the files of another. */
export interface MapEdge {
  readonly from: string
  readonly to: string
  /** Number of importing file pairs. */
  readonly weight: number
}

/** The boxes and imports of one scope. */
export interface MapView {
  /** Directory shown, '' for the whole project. */
  readonly scope: string
  readonly nodes: readonly MapNode[]
  readonly edges: readonly MapEdge[]
}

/** Boxes the automatic split aims for at most. */
export const MAX_NODES = 16
/** A scope with at most this many files shows every file as a box. */
export const FILE_LEVEL_MAX = 24
/** Boxes outside the scope kept, by import count. */
const MAX_OUTSIDE = 8
/** A directory holding more than this share of the scope's code is split into its parts. */
const SPLIT_SHARE = 0.35

const TEST_DIRECTORY = /^(?:tests?|__tests__|specs?|e2e|testing)$/iu
const DOC_DIRECTORY = /^(?:docs?|documentation)$/iu

/** @returns Whether `path` lies inside directory `scope` ('' contains everything). */
export function inScope(scope: string, path: string): boolean {
  return scope === '' || path.startsWith(`${scope}/`)
}

/** @returns The directory part of a path, '' at the top. */
export function parentOf(path: string): string {
  const slash = path.lastIndexOf('/')
  return slash < 0 ? '' : path.slice(0, slash)
}

/** @returns The id of the loose-files box of a directory. */
export function looseId(directory: string): string {
  return directory === '' ? '*' : `${directory}/*`
}

/** @returns How much a file counts when sizing and splitting: its lines, and at least 1. */
export function fileWeight(file: MapFile): number {
  return Math.max(1, file.lines)
}

function codeWeight(file: MapFile): number {
  return file.kind === 'code' || file.kind === 'test' ? fileWeight(file) : 0
}

/**
 * The box a path belongs to just outside a scope: the first directory or file where the path leaves the scope's
 * ancestors. For scope `src/app`, `src/lib/x.ts` belongs to `src/lib` and `package.json` to itself.
 * @param scope Scope directory.
 * @param path File path outside the scope.
 * @returns The outside box id.
 */
export function outsideId(scope: string, path: string): string {
  const scopeParts = scope === '' ? [] : scope.split('/')
  const parts = path.split('/')
  let common = 0
  while (common < scopeParts.length && common < parts.length - 1 && scopeParts[common] === parts[common]) common++
  return parts.slice(0, common + 1).join('/')
}

interface Group { id: string; type: MapNode['type']; files: number[] }

/** Split the files of `directory` into its subdirectories and its loose files. */
function split(map: ProjectMap, directory: string, files: readonly number[], looseLimit: number): Group[] {
  const directories = new Map<string, number[]>()
  const loose: number[] = []
  const offset = directory === '' ? 0 : directory.length + 1
  for (const index of files) {
    const rest = map.files[index]!.path.slice(offset)
    const slash = rest.indexOf('/')
    if (slash < 0) { loose.push(index); continue }
    const id = `${directory === '' ? '' : `${directory}/`}${rest.slice(0, slash)}`
    let group = directories.get(id)
    if (group === undefined) { group = []; directories.set(id, group) }
    group.push(index)
  }
  const groups: Group[] = [...directories].map(([id, members]) => ({ id, type: 'directory' as const, files: members }))
  if (loose.length > looseLimit) groups.push({ id: looseId(directory), type: 'loose', files: loose })
  else for (const index of loose) groups.push({ id: map.files[index]!.path, type: 'file', files: [index] })
  return groups
}

function weightOf(map: ProjectMap, group: Group): number {
  let code = 0
  for (const index of group.files) code += codeWeight(map.files[index]!)
  return code
}

function describe(map: ProjectMap, group: Group, outside: boolean): MapNode {
  const kinds = new Map<MapFileKind, number>()
  const languages = new Map<string, number>()
  let lines = 0
  for (const index of group.files) {
    const file = map.files[index]!
    lines += file.lines
    const weight = file.kind === 'asset' || file.kind === 'other' ? 1 : fileWeight(file)
    kinds.set(file.kind, (kinds.get(file.kind) ?? 0) + weight)
    if (file.language !== '' && file.kind !== 'asset') languages.set(file.language, (languages.get(file.language) ?? 0) + file.lines)
  }
  const name = group.id.slice(group.id.lastIndexOf('/') + 1)
  let kind: MapFileKind = [...kinds].sort((left, right) => right[1] - left[1])[0]?.[0] ?? 'other'
  if (group.type === 'directory' && TEST_DIRECTORY.test(name)) kind = 'test'
  else if (group.type === 'directory' && DOC_DIRECTORY.test(name) && kind !== 'code') kind = 'doc'
  return {
    id: group.id, type: group.type, outside, kind, files: group.files, lines,
    languages: [...languages].filter(entry => entry[1] > 0).sort((left, right) => right[1] - left[1]).map(([language, count]) => ({ language, lines: count })),
    openable: group.type === 'directory' && group.files.length > 1,
  }
}

/** @returns Indexes of the files under `directory`. */
function filesUnder(map: ProjectMap, directory: string): number[] {
  const result: number[] = []
  map.files.forEach((file, index) => { if (inScope(directory, file.path)) result.push(index) })
  return result
}

/**
 * The boxes of one scope and the imports between them, with the directories outside it that its files import or that
 * import them. Directories holding most of the scope's code are split into their parts until about {@link MAX_NODES}
 * boxes remain; a small scope shows each file.
 * @param map Project map.
 * @param scope Directory to show, '' for the whole project.
 * @returns The view.
 */
export function mapView(map: ProjectMap, scope: string): MapView {
  const files = filesUnder(map, scope)
  let groups: Group[]
  if (files.length <= FILE_LEVEL_MAX) {
    groups = files.map(index => ({ id: map.files[index]!.path, type: 'file' as const, files: [index] }))
  } else {
    groups = split(map, scope, files, 6)
    const total = groups.reduce((sum, group) => sum + weightOf(map, group), 0)
    const settled = new Set<string>()
    for (;;) {
      const largest = groups.filter(group => group.type === 'directory' && !settled.has(group.id) && group.files.length > 1 && weightOf(map, group) > total * SPLIT_SHARE)
        .sort((left, right) => weightOf(map, right) - weightOf(map, left))[0]
      if (largest === undefined) break
      const parts = split(map, largest.id, largest.files, 3)
      // Splitting must show more structure: a lone box of loose files does not, and too many boxes crowd the view.
      // A directory with a single subdirectory is replaced by it, whatever the box count.
      const useful = parts.length > 1 || parts[0]?.type === 'directory'
      if (!useful || (parts.length > 1 && groups.length - 1 + parts.length > MAX_NODES + 2)) { settled.add(largest.id); continue }
      groups = groups.flatMap(group => group === largest ? parts : [group])
    }
  }
  const order = { directory: 0, loose: 1, file: 2 }
  groups.sort((left, right) => order[left.type] - order[right.type] || (left.id < right.id ? -1 : left.id > right.id ? 1 : 0))
  const nodeOf = new Map<number, string>()
  for (const group of groups) for (const index of group.files) nodeOf.set(index, group.id)
  const weights = new Map<string, number>()
  const outsideWeights = new Map<string, number>()
  const add = (from: string, to: string): void => {
    const key = `${from}\n${to}`
    weights.set(key, (weights.get(key) ?? 0) + 1)
  }
  for (let index = 0; index + 1 < map.imports.length; index += 2) {
    const from = map.imports[index]!
    const to = map.imports[index + 1]!
    const source = nodeOf.get(from)
    const target = nodeOf.get(to)
    if (source !== undefined && target !== undefined) {
      if (source !== target) add(source, target)
    } else if (source !== undefined) {
      const ghost = outsideId(scope, map.files[to]!.path)
      outsideWeights.set(ghost, (outsideWeights.get(ghost) ?? 0) + 1)
      add(source, ghost)
    } else if (target !== undefined) {
      const ghost = outsideId(scope, map.files[from]!.path)
      outsideWeights.set(ghost, (outsideWeights.get(ghost) ?? 0) + 1)
      add(ghost, target)
    }
  }
  const kept = new Set([...outsideWeights].sort((left, right) => right[1] - left[1] || left[0].localeCompare(right[0])).slice(0, MAX_OUTSIDE).map(entry => entry[0]))
  const nodes = groups.map(group => describe(map, group, false))
  for (const id of [...kept].sort()) {
    const single = map.files.findIndex(file => file.path === id)
    nodes.push(describe(map, single >= 0 ? { id, type: 'file', files: [single] } : { id, type: 'directory', files: filesUnder(map, id) }, true))
  }
  const present = new Set(nodes.map(node => node.id))
  const edges: MapEdge[] = []
  for (const [key, weight] of weights) {
    const [from, to] = key.split('\n') as [string, string]
    if (present.has(from) && present.has(to)) edges.push({ from, to, weight })
  }
  edges.sort((left, right) => left.from.localeCompare(right.from) || left.to.localeCompare(right.to))
  return { scope, nodes, edges }
}

/**
 * The box of a view a file falls in.
 * @param view Map view.
 * @param path File path.
 * @returns The box id, if the file is in one of the boxes.
 */
export function nodeForPath(view: MapView, path: string): string | undefined {
  for (const node of view.nodes) {
    const contains = node.type === 'file' ? node.id === path : node.type === 'loose' ? parentOf(path) === parentOf(node.id) : inScope(node.id, path)
    if (contains) return node.id
  }
  return undefined
}
