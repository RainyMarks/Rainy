/**
 * Layered layout of an architecture graph: parts that import others sit above what they import, so the foundations
 * end up at the bottom. Cycles are broken for layering only; parts without imports go in a row of their own below.
 */

/** Box size and spacing, in SVG units. */
export const NODE_WIDTH = 196
export const NODE_HEIGHT = 66
const GAP_X = 36
const GAP_Y = 76
/** Boxes per row before a layer wraps. */
const ROW_LIMIT = 6
/** Space between the layered graph and the row of parts without imports. */
const ISOLATED_GAP = 56

/** A placed box. */
export interface PlacedNode {
  readonly id: string
  readonly x: number
  readonly y: number
  /** No import connects it to another box. */
  readonly isolated: boolean
}

/** A placed edge: an SVG path from the importing box to the imported one. */
export interface PlacedEdge {
  readonly from: string
  readonly to: string
  readonly weight: number
  readonly path: string
  /** The edge points up the layers: it closes an import cycle. */
  readonly upward: boolean
}

/** The whole drawing. */
export interface GraphLayout {
  readonly nodes: ReadonlyMap<string, PlacedNode>
  readonly edges: readonly PlacedEdge[]
  readonly width: number
  readonly height: number
  /** Top of the row of parts without imports, when there is one. */
  readonly isolatedTop?: number | undefined
}

/**
 * Lay out boxes and imports.
 * @param ids Box ids in their preferred order.
 * @param edges Imports between boxes.
 * @returns Positions and edge paths.
 */
export function layoutGraph(ids: readonly string[], edges: readonly { readonly from: string; readonly to: string; readonly weight: number }[]): GraphLayout {
  const known = new Set(ids)
  const links = edges.filter(edge => edge.from !== edge.to && known.has(edge.from) && known.has(edge.to))
  const outgoing = new Map<string, string[]>(ids.map(id => [id, []]))
  const neighbours = new Map<string, Set<string>>(ids.map(id => [id, new Set()]))
  for (const edge of links) {
    outgoing.get(edge.from)!.push(edge.to)
    neighbours.get(edge.from)!.add(edge.to)
    neighbours.get(edge.to)!.add(edge.from)
  }
  const connected = ids.filter(id => neighbours.get(id)!.size > 0)
  const isolated = ids.filter(id => neighbours.get(id)!.size === 0)

  // Break cycles: an edge back to a box still on the depth-first stack does not count for layering.
  const state = new Map<string, 'open' | 'done'>()
  const forward = new Map<string, string[]>(connected.map(id => [id, []]))
  const visit = (id: string): void => {
    state.set(id, 'open')
    for (const next of outgoing.get(id)!) {
      const seen = state.get(next)
      if (seen === 'open') continue
      forward.get(id)!.push(next)
      if (seen === undefined) visit(next)
    }
    state.set(id, 'done')
  }
  for (const id of connected) if (!state.has(id)) visit(id)

  // Longest path down to a box that imports nothing: those sit at layer 0, the bottom.
  const layer = new Map<string, number>()
  const depth = (id: string): number => {
    const known = layer.get(id)
    if (known !== undefined) return known
    let value = 0
    for (const next of forward.get(id)!) value = Math.max(value, depth(next) + 1)
    layer.set(id, value)
    return value
  }
  for (const id of connected) depth(id)
  const top = Math.max(0, ...layer.values())
  const rows: string[][] = Array.from({ length: connected.length === 0 ? 0 : top + 1 }, () => [])
  for (const id of connected) rows[top - layer.get(id)!]!.push(id)

  // Order each row by the mean position of its neighbours, sweeping down and up a few times.
  const position = new Map<string, number>()
  const place = (): void => {
    for (const row of rows) row.forEach((id, index) => { position.set(id, row.length === 1 ? 0.5 : index / (row.length - 1)) })
  }
  place()
  for (let sweep = 0; sweep < 8; sweep++) {
    const order = sweep % 2 === 0 ? rows : [...rows].reverse()
    for (const row of order) {
      const centre = new Map(row.map((id) => {
        const near = [...neighbours.get(id)!]
        return [id, near.length === 0 ? position.get(id)! : near.reduce((sum, other) => sum + position.get(other)!, 0) / near.length] as const
      }))
      row.sort((left, right) => centre.get(left)! - centre.get(right)!)
      row.forEach((id, index) => { position.set(id, row.length === 1 ? 0.5 : index / (row.length - 1)) })
    }
  }

  // Long rows wrap, so a wide layer becomes several rows of at most ROW_LIMIT boxes.
  const lines: { ids: string[]; isolated: boolean }[] = []
  for (const row of rows) for (let start = 0; start < row.length; start += ROW_LIMIT) lines.push({ ids: row.slice(start, start + ROW_LIMIT), isolated: false })
  const isolatedStart = lines.length
  for (let start = 0; start < isolated.length; start += ROW_LIMIT) lines.push({ ids: isolated.slice(start, start + ROW_LIMIT), isolated: true })
  const rowWidth = (count: number): number => count * NODE_WIDTH + Math.max(0, count - 1) * GAP_X
  const width = Math.max(NODE_WIDTH, ...lines.map(line => rowWidth(line.ids.length)))
  const nodes = new Map<string, PlacedNode>()
  let y = 0
  let isolatedTop: number | undefined
  lines.forEach((line, index) => {
    if (index === isolatedStart && index > 0) y += ISOLATED_GAP
    if (index === isolatedStart) isolatedTop = y
    const left = (width - rowWidth(line.ids.length)) / 2
    line.ids.forEach((id, column) => { nodes.set(id, { id, x: left + column * (NODE_WIDTH + GAP_X), y, isolated: line.isolated }) })
    y += NODE_HEIGHT + GAP_Y
  })
  const height = Math.max(NODE_HEIGHT, y - GAP_Y)

  const placed = links.map((edge): PlacedEdge => {
    const from = nodes.get(edge.from)!
    const to = nodes.get(edge.to)!
    const fromX = from.x + NODE_WIDTH / 2
    const toX = to.x + NODE_WIDTH / 2
    let path: string
    let upward = false
    if (from.y < to.y) {
      const startY = from.y + NODE_HEIGHT
      const bend = (to.y - startY) / 2
      path = `M ${fromX} ${startY} C ${fromX} ${startY + bend}, ${toX} ${to.y - bend}, ${toX} ${to.y}`
    } else if (from.y > to.y) {
      upward = true
      const endY = to.y + NODE_HEIGHT
      const bend = (from.y - endY) / 2
      path = `M ${fromX} ${from.y} C ${fromX} ${from.y - bend}, ${toX} ${endY + bend}, ${toX} ${endY}`
    } else {
      // Same row: an arc above the boxes.
      const startX = fromX + (toX > fromX ? NODE_WIDTH / 4 : -NODE_WIDTH / 4)
      const endX = toX + (toX > fromX ? -NODE_WIDTH / 4 : NODE_WIDTH / 4)
      const lift = Math.min(60, 18 + Math.abs(toX - fromX) / 6)
      path = `M ${startX} ${from.y} C ${startX} ${from.y - lift}, ${endX} ${to.y - lift}, ${endX} ${to.y}`
    }
    return { from: edge.from, to: edge.to, weight: edge.weight, path, upward }
  })
  return { nodes, edges: placed, width, height, ...(isolatedTop === undefined || isolatedStart === 0 ? {} : { isolatedTop }) }
}
