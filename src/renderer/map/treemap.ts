/** Squarified treemap of a project's files: every directory a frame, every file a block sized by its lines. */
import { fileWeight, inScope } from '../../shared/map-graph.ts'
import type { MapFile, ProjectMap } from '../../shared/map-protocol.ts'

/** A drawn rectangle: a directory frame, a file, or a directory too small to show its files. */
export interface TreeBlock {
  readonly kind: 'directory' | 'file' | 'folded'
  /** Directory or file path. */
  readonly path: string
  readonly name: string
  readonly x: number
  readonly y: number
  readonly width: number
  readonly height: number
  /** Index into {@link ProjectMap.files} for a file. */
  readonly file?: number | undefined
  /** Lines (or line-equivalents) inside. */
  readonly weight: number
  /** A frame large enough to carry its name above its contents. */
  readonly titled: boolean
}

/** Directory frames get a title row when they are at least this large. */
const TITLE_HEIGHT = 16
const TITLE_MIN_WIDTH = 48
const TITLE_MIN_HEIGHT = 34
const PADDING = 2
/** A directory with less area than this is drawn as one folded block. */
const FOLD_AREA = 900

interface TreeNode {
  name: string
  path: string
  weight: number
  file?: number | undefined
  children?: TreeNode[] | undefined
}

/** @returns How much space a file takes: its lines, or a size-based share for binary and generated files. */
export function blockWeight(file: MapFile): number {
  if (file.kind === 'asset' || file.lines === 0) return Math.min(400, Math.max(1, Math.round(file.bytes / 64)))
  return fileWeight(file)
}

function build(map: ProjectMap, scope: string): TreeNode {
  const root: TreeNode = { name: scope.slice(scope.lastIndexOf('/') + 1), path: scope, weight: 0, children: [] }
  const directories = new Map<string, TreeNode>([[scope, root]])
  const offset = scope === '' ? 0 : scope.length + 1
  map.files.forEach((file, index) => {
    if (!inScope(scope, file.path)) return
    const parts = file.path.slice(offset).split('/')
    let parent = root
    let path = scope
    for (const part of parts.slice(0, -1)) {
      path = path === '' ? part : `${path}/${part}`
      let directory = directories.get(path)
      if (directory === undefined) {
        directory = { name: part, path, weight: 0, children: [] }
        directories.set(path, directory)
        parent.children!.push(directory)
      }
      parent = directory
    }
    parent.children!.push({ name: parts.at(-1)!, path: file.path, weight: blockWeight(file), file: index })
  })
  const total = (node: TreeNode): number => {
    if (node.children === undefined) return node.weight
    node.weight = node.children.reduce((sum, child) => sum + total(child), 0)
    node.children.sort((left, right) => right.weight - left.weight || left.name.localeCompare(right.name))
    return node.weight
  }
  total(root)
  return root
}

/** Split a rectangle among weighted items in rows of near-square blocks (Bruls, Huizing and van Wijk). */
function squarify<T extends { weight: number }>(items: readonly T[], x: number, y: number, width: number, height: number): { item: T; x: number; y: number; width: number; height: number }[] {
  const placed: { item: T; x: number; y: number; width: number; height: number }[] = []
  const total = items.reduce((sum, item) => sum + item.weight, 0)
  if (total <= 0 || width <= 0 || height <= 0) return placed
  const scale = (width * height) / total
  let rest = items.map(item => ({ item, area: item.weight * scale }))
  let left = x
  let top = y
  let w = width
  let h = height
  const worst = (row: readonly { area: number }[], side: number): number => {
    const sum = row.reduce((total, entry) => total + entry.area, 0)
    let max = 0
    let min = Infinity
    for (const entry of row) { max = Math.max(max, entry.area); min = Math.min(min, entry.area) }
    return Math.max((side * side * max) / (sum * sum), (sum * sum) / (side * side * min))
  }
  while (rest.length > 0) {
    const side = Math.min(w, h)
    const row = [rest[0]!]
    let index = 1
    while (index < rest.length && worst([...row, rest[index]!], side) <= worst(row, side)) { row.push(rest[index]!); index++ }
    rest = rest.slice(index)
    const sum = row.reduce((total, entry) => total + entry.area, 0)
    if (w >= h) {
      const columnWidth = sum / h
      let offset = top
      for (const entry of row) {
        const blockHeight = entry.area / columnWidth
        placed.push({ item: entry.item, x: left, y: offset, width: columnWidth, height: blockHeight })
        offset += blockHeight
      }
      left += columnWidth
      w -= columnWidth
    } else {
      const rowHeight = sum / w
      let offset = left
      for (const entry of row) {
        const blockWidth = entry.area / rowHeight
        placed.push({ item: entry.item, x: offset, y: top, width: blockWidth, height: rowHeight })
        offset += blockWidth
      }
      top += rowHeight
      h -= rowHeight
    }
  }
  return placed
}

/**
 * Lay out the files under a directory.
 * @param map Project map.
 * @param scope Directory to show, '' for the whole project.
 * @param width Drawing width in pixels.
 * @param height Drawing height in pixels.
 * @returns Blocks, frames before their contents.
 */
export function layoutTreemap(map: ProjectMap, scope: string, width: number, height: number): TreeBlock[] {
  const blocks: TreeBlock[] = []
  const lay = (node: TreeNode, x: number, y: number, w: number, h: number): void => {
    for (const { item, x: left, y: top, width: blockWidth, height: blockHeight } of squarify(node.children ?? [], x, y, w, h)) {
      if (item.children === undefined) {
        blocks.push({ kind: 'file', path: item.path, name: item.name, x: left, y: top, width: blockWidth, height: blockHeight, file: item.file, weight: item.weight, titled: false })
        continue
      }
      if (blockWidth * blockHeight < FOLD_AREA || blockWidth < 2 * PADDING + 4 || blockHeight < 2 * PADDING + 4) {
        blocks.push({ kind: 'folded', path: item.path, name: item.name, x: left, y: top, width: blockWidth, height: blockHeight, weight: item.weight, titled: false })
        continue
      }
      const titled = blockWidth >= TITLE_MIN_WIDTH && blockHeight >= TITLE_MIN_HEIGHT
      blocks.push({ kind: 'directory', path: item.path, name: item.name, x: left, y: top, width: blockWidth, height: blockHeight, weight: item.weight, titled })
      const inset = titled ? TITLE_HEIGHT : PADDING
      lay(item, left + PADDING, top + inset, blockWidth - 2 * PADDING, blockHeight - inset - PADDING)
    }
  }
  lay(build(map, scope), 0, 0, width, height)
  return blocks
}
