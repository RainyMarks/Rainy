/**
 * The project map: the project's architecture as boxes and imports, or every file as blocks, with the files the agent
 * is reading or changing highlighted as it works, and an AI explanation of the parts.
 */
import { useEffect, useMemo, useRef, useState, type PointerEvent as ReactPointerEvent, type ReactNode, type WheelEvent } from 'react'
import clsx from 'clsx'
import type { IdeWorkspace } from '../../shared/ide-files-protocol.ts'
import { mapView, nodeForPath, parentOf, type MapNode, type MapView } from '../../shared/map-graph.ts'
import type { MapFileKind, ProjectMap } from '../../shared/map-protocol.ts'
import type { ChatChange } from '../../shared/rpc.ts'
import { useLocale } from '../i18n.ts'
import { keyFromAbsolute } from '../ide/ide-paths.ts'
import { Button, IconAction, IconRefreshOutlineRegular, IconSparkleRegular, SegmentedControl, toast } from '../ui/index.ts'
import type { AiActivity } from './activity.ts'
import { fileColor } from './colors.ts'
import { GAP_X, layoutGraph, NODE_HEIGHT, NODE_WIDTH } from './layout.ts'
import { explainMap, loadMap, useProjectMap } from './map-store.ts'
import { aiMarks, type AiMark } from './marks.ts'
import { useMapT, type MapMessageKey } from './messages.ts'
import { layoutTreemap } from './treemap.ts'
import css from './ProjectMap.module.css'

type Translate = ReturnType<typeof useMapT>

/** What the map needs from the window. */
export interface ProjectMapProps {
  readonly workspace: IdeWorkspace | null
  /** The map is on screen; it loads and follows changes only then. */
  readonly visible: boolean
  /** Files the agent changed in the current chat and the user has not reviewed. */
  readonly changes: readonly ChatChange[]
  /** The agents' running and recent tool calls. */
  readonly activity: { readonly active: readonly AiActivity[]; readonly recent: readonly AiActivity[] }
  /** Open a file of the project in the editor, by editor key. */
  readonly openFile: (path: string) => void
  /** Send a message to the AI. */
  readonly ask: (text: string) => void
}

/** How the agent touches a box: the strongest mark of its files, and how many of them it changed. */
interface NodeActivity { mark?: 'reading' | 'writing' | undefined; changed: number }

const KIND_LABEL: Record<MapFileKind, MapMessageKey> = {
  code: 'kindCode', test: 'kindTest', doc: 'kindDoc', config: 'kindConfig', asset: 'kindAsset', other: 'kindOther',
}

function compact(value: number): string {
  if (value < 1000) return String(value)
  if (value < 10_000) return `${(value / 1000).toFixed(1).replace(/\.0$/u, '')}k`
  return `${Math.round(value / 1000)}k`
}

/** East Asian wide characters, which take about two columns. */
const WIDE = /[\u1100-\u115f\u2e80-\ua4cf\uac00-\ud7a3\uf900-\ufaff\ufe30-\ufe4f\uff00-\uff60\uffe0-\uffe6]/u

/** Shorten text to about `limit` narrow characters; CJK characters count double. */
function fit(text: string, limit: number): string {
  let width = 0
  let index = 0
  for (const char of text) {
    width += WIDE.test(char) ? 2 : 1
    if (width > limit) return `${text.slice(0, index)}…`
    index += char.length
  }
  return text
}

function nodeLabel(node: MapNode, scope: string, t: Translate): string {
  if (node.type === 'loose') {
    const directory = node.id.slice(0, -2)
    return directory === '' ? t('mapRootLoose') : t('mapLoose', { name: directory.slice(directory.lastIndexOf('/') + 1) })
  }
  if (node.outside) return node.id
  if (node.type === 'file') return node.id.slice(node.id.lastIndexOf('/') + 1)
  return scope === '' ? node.id : node.id.slice(scope.length + 1)
}

function relativeTime(at: number, now: number, t: Translate): string {
  const seconds = Math.max(0, Math.round((now - at) / 1000))
  if (seconds < 10) return t('mapJustNow')
  if (seconds < 60) return t('mapSecondsAgo', { count: seconds })
  if (seconds < 3600) return t('mapMinutesAgo', { count: Math.round(seconds / 60) })
  return t('mapHoursAgo', { count: Math.round(seconds / 3600) })
}

/** @returns The element's size, kept current. */
function useSize(element: HTMLElement | null): { width: number; height: number } {
  const [size, setSize] = useState({ width: 0, height: 0 })
  useEffect(() => {
    if (element === null) return
    const measure = (): void => { setSize({ width: element.clientWidth, height: element.clientHeight }) }
    measure()
    const observer = new ResizeObserver(measure)
    observer.observe(element)
    return () => { observer.disconnect() }
  }, [element])
  return size
}

/** @returns The current time, updated every few seconds while mounted. */
function useNow(intervalMs: number): number {
  const [now, setNow] = useState(() => Date.now())
  useEffect(() => {
    const timer = setInterval(() => { setNow(Date.now()) }, intervalMs)
    return () => { clearInterval(timer) }
  }, [intervalMs])
  return now
}

/**
 * Boxes shown by default: the scope's code and the code just outside it that it imports or is imported by. When that
 * leaves fewer than two boxes, everything is shown.
 */
function shownNodes(view: MapView, all: boolean): MapNode[] {
  if (all) return [...view.nodes]
  const inside = view.nodes.filter(node => !node.outside && node.kind === 'code')
  if (inside.length < 2) return [...view.nodes]
  const kept = new Set(inside.map(node => node.id))
  const outside = view.nodes.filter(node => node.outside && node.kind === 'code'
    && view.edges.some(edge => (edge.from === node.id && kept.has(edge.to)) || (edge.to === node.id && kept.has(edge.from))))
  return [...inside, ...outside]
}

function verb(activity: AiActivity, t: Translate): string {
  switch (activity.action) {
    case 'read': return t('mapRead')
    case 'write': return t('mapWrite')
    case 'edit': return t('mapEdit')
    case 'search': return t('mapSearch')
    case 'command': return t('mapCommand')
    default: return t('mapOther')
  }
}

/** Architecture: boxes and imports in layers, with pan and zoom. */
function Architecture({ map, view, nodes, scope, selected, activity, explanation, onSelect, onOpen, t }: {
  map: ProjectMap
  view: MapView
  nodes: readonly MapNode[]
  scope: string
  selected: string | undefined
  activity: ReadonlyMap<string, NodeActivity>
  explanation: Readonly<Record<string, string>> | undefined
  onSelect: (id: string | undefined) => void
  onOpen: (node: MapNode) => void
  t: Translate
}): JSX.Element {
  const [frame, setFrame] = useState<HTMLDivElement | null>(null)
  const size = useSize(frame)
  const ids = useMemo(() => nodes.map(node => node.id), [nodes])
  const margin = 40
  // As many boxes per row as fit the pane at full size, so a narrow pane wraps rather than shrinking the text.
  const columns = size.width === 0 ? 6 : Math.min(8, Math.max(2, Math.floor((size.width - 2 * margin + GAP_X) / (NODE_WIDTH + GAP_X))))
  const layout = useMemo(() => layoutGraph(ids, view.edges, columns), [ids, view.edges, columns])
  const [transform, setTransform] = useState({ x: 0, y: 0, k: 1 })
  const drag = useRef<{ x: number; y: number; originX: number; originY: number; moved: boolean } | undefined>(undefined)
  const fitView = (): void => {
    if (size.width === 0 || size.height === 0) return
    const k = Math.min(1.15, (size.width - 2 * margin) / layout.width, (size.height - 2 * margin) / (layout.height + 24))
    const scale = Math.max(0.2, k)
    setTransform({ k: scale, x: (size.width - layout.width * scale) / 2, y: Math.max(margin, (size.height - layout.height * scale) / 2) })
  }
  // A new scope, other boxes or a resized pane show the whole drawing again; a rescan with the same boxes keeps the view.
  const shape = `${ids.join('\n')}:${layout.width}x${layout.height}`
  useEffect(fitView, [shape, size.width, size.height])
  const zoom = (factor: number, centreX = size.width / 2, centreY = size.height / 2): void => {
    setTransform((current) => {
      const k = Math.min(3, Math.max(0.2, current.k * factor))
      return { k, x: centreX - ((centreX - current.x) * k) / current.k, y: centreY - ((centreY - current.y) * k) / current.k }
    })
  }
  const onWheel = (event: WheelEvent<HTMLDivElement>): void => {
    const box = event.currentTarget.getBoundingClientRect()
    zoom(Math.exp(-event.deltaY * 0.0015), event.clientX - box.left, event.clientY - box.top)
  }
  const onPointerDown = (event: ReactPointerEvent<HTMLDivElement>): void => {
    if (event.button !== 0 || (event.target as Element).closest('[data-map-node]') !== null) return
    event.currentTarget.setPointerCapture(event.pointerId)
    drag.current = { x: event.clientX, y: event.clientY, originX: transform.x, originY: transform.y, moved: false }
  }
  const onPointerMove = (event: ReactPointerEvent<HTMLDivElement>): void => {
    const current = drag.current
    if (current === undefined) return
    const dx = event.clientX - current.x
    const dy = event.clientY - current.y
    if (Math.abs(dx) + Math.abs(dy) > 3) current.moved = true
    setTransform(value => ({ ...value, x: current.originX + dx, y: current.originY + dy }))
  }
  const onPointerUp = (): void => {
    const current = drag.current
    drag.current = undefined
    if (current !== undefined && !current.moved) onSelect(undefined)
  }
  const byId = new Map(nodes.map(node => [node.id, node]))
  const highlighted = (from: string, to: string): 'uses' | 'used' | undefined =>
    selected === undefined ? undefined : from === selected ? 'uses' : to === selected ? 'used' : undefined
  return (
    <div ref={setFrame} className={css.canvas} data-map-architecture onWheel={onWheel} onPointerDown={onPointerDown} onPointerMove={onPointerMove}
      onPointerUp={onPointerUp} onLostPointerCapture={() => { drag.current = undefined }}>
      <svg className={css.svg} width="100%" height="100%" role="img" aria-label={t('mapArchitecture')}>
        <defs>
          {(['plain', 'uses', 'used'] as const).map(tone => (
            <marker key={tone} id={`map-arrow-${tone}`} viewBox="0 0 10 10" refX="9" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse">
              <path d="M 0 0 L 10 5 L 0 10 z" className={css[`arrow_${tone}`]} />
            </marker>
          ))}
        </defs>
        <g transform={`translate(${transform.x} ${transform.y}) scale(${transform.k})`}>
          {layout.isolatedTop !== undefined && (
            <text x={0} y={layout.isolatedTop - 14} className={css.groupLabel}>{t('mapIsolated')}</text>
          )}
          {layout.edges.map((edge) => {
            const tone = highlighted(edge.from, edge.to)
            return (
              <path key={`${edge.from}->${edge.to}`} d={edge.path} className={clsx(css.edge, tone !== undefined && css[`edge_${tone}`], edge.upward && css.edgeUp,
                selected !== undefined && tone === undefined && css.edgeDim)}
                strokeWidth={Math.min(4, 1 + Math.log2(edge.weight) / 1.5)} markerEnd={`url(#map-arrow-${tone ?? 'plain'})`}>
                <title>{`${edge.from} → ${edge.to} (${edge.weight})`}</title>
              </path>
            )
          })}
          {[...layout.nodes.values()].map((place) => {
            const node = byId.get(place.id)!
            const touch = activity.get(node.id)
            const description = explanation?.[node.id]
            const color = fileColor(node.languages[0]?.language ?? '', node.kind)
            const total = node.languages.reduce((sum, item) => sum + item.lines, 0)
            let offset = 0
            return (
              <g key={node.id} transform={`translate(${place.x} ${place.y})`} data-map-node={node.id} data-mark={touch?.mark}
                className={clsx(css.node, node.outside && css.nodeOutside, selected === node.id && css.nodeSelected, touch?.mark !== undefined && css[`node_${touch.mark}`])}
                style={{ ['--node-color' as string]: color }} tabIndex={0} role="button"
                aria-label={`${nodeLabel(node, scope, t)}${description === undefined ? '' : `: ${description}`}`}
                onClick={() => { onSelect(node.id) }} onDoubleClick={() => { onOpen(node) }}
                onKeyDown={(event) => { if (event.key === 'Enter') onOpen(node); else if (event.key === ' ') { event.preventDefault(); onSelect(node.id) } }}>
                <title>{`${node.id}\n${t('mapNodeStats', { files: node.files.length, lines: node.lines })}${description === undefined ? '' : `\n${description}`}`}</title>
                <rect className={css.nodeBox} width={NODE_WIDTH} height={NODE_HEIGHT} rx={6} />
                <rect className={css.nodeAccent} width={4} height={NODE_HEIGHT - 12} x={0} y={6} rx={2} />
                <text x={14} y={24} className={css.nodeName}>{fit(`${node.outside ? '↗ ' : ''}${nodeLabel(node, scope, t)}`, 24)}</text>
                <text x={14} y={43} className={css.nodeDetail}>
                  {description !== undefined ? fit(description, 27) : t('mapNodeStats', { files: node.files.length, lines: compact(node.lines) })}
                </text>
                {total > 0 && node.languages.slice(0, 4).map((item) => {
                  const width = ((NODE_WIDTH - 28) * item.lines) / total
                  const segment = <rect key={item.language} x={14 + offset} y={NODE_HEIGHT - 12} width={Math.max(1, width)} height={3} rx={1.5}
                    style={{ fill: fileColor(item.language, 'code') }} />
                  offset += width
                  return segment
                })}
                {touch?.mark !== undefined && <circle className={css.pulse} cx={NODE_WIDTH - 14} cy={14} r={5} />}
                {touch !== undefined && touch.changed > 0 && (
                  <g transform={`translate(${NODE_WIDTH - 34} ${NODE_HEIGHT - 22})`} className={css.badge}>
                    <rect width={26} height={15} rx={7.5} />
                    <text x={13} y={11} textAnchor="middle">{`✎${touch.changed}`}</text>
                  </g>
                )}
              </g>
            )
          })}
        </g>
      </svg>
      <div className={css.zoom}>
        <IconAction label={t('mapZoomIn')} onClick={() => { zoom(1.25) }}><span aria-hidden="true">+</span></IconAction>
        <IconAction label={t('mapZoomOut')} onClick={() => { zoom(0.8) }}><span aria-hidden="true">−</span></IconAction>
        <IconAction label={t('mapFit')} onClick={fitView}><span aria-hidden="true">⤢</span></IconAction>
      </div>
      {map.files.length === 0 && <div className={css.empty}>{t('mapEmptyScope')}</div>}
    </div>
  )
}

/** Files: nested blocks sized by lines. */
function Files({ map, scope, marks, onScope, onOpenFile, t }: {
  map: ProjectMap
  scope: string
  marks: ReadonlyMap<string, AiMark>
  onScope: (scope: string) => void
  onOpenFile: (path: string) => void
  t: Translate
}): JSX.Element {
  const [frame, setFrame] = useState<HTMLDivElement | null>(null)
  const size = useSize(frame)
  const blocks = useMemo(() => size.width === 0 ? [] : layoutTreemap(map, scope, size.width, size.height), [map, scope, size.width, size.height])
  const [hover, setHover] = useState<string | undefined>()
  return (
    <div ref={setFrame} className={css.treemap} data-map-files onPointerLeave={() => { setHover(undefined) }}>
      {blocks.map((block) => {
        const style = { left: block.x, top: block.y, width: block.width, height: block.height }
        if (block.kind === 'directory') {
          return (
            <div key={`d:${block.path}`} className={css.frame} style={style}>
              {block.titled && <button type="button" className={css.frameTitle} title={block.path} onClick={() => { onScope(block.path) }}>{block.name}</button>}
            </div>
          )
        }
        if (block.kind === 'folded') {
          return (
            <button key={`f:${block.path}`} type="button" className={css.folded} style={style} title={`${block.path}\n${t('mapLinesCount', { count: block.weight })}`}
              onClick={() => { onScope(block.path) }} onPointerEnter={() => { setHover(block.path) }}>
              {block.width > 40 && block.height > 14 && <span>{block.name}</span>}
            </button>
          )
        }
        const file = map.files[block.file!]!
        const mark = marks.get(file.path)
        return (
          <button key={`t:${block.path}`} type="button" className={clsx(css.tile, mark !== undefined && css[`tile_${mark}`])} data-mark={mark}
            style={{ ...style, ['--tile-color' as string]: fileColor(file.language, file.kind) }}
            title={`${file.path}\n${file.lines > 0 ? t('mapLinesCount', { count: file.lines }) : ''}`}
            onClick={() => { onOpenFile(file.path) }} onPointerEnter={() => { setHover(file.path) }}>
            {block.width > 46 && block.height > 15 && <span>{block.name}</span>}
          </button>
        )
      })}
      {hover !== undefined && <div className={css.hover}>{hover}</div>}
    </div>
  )
}

/** Details of the selected box. */
function Details({ map, view, node, scope, explanation, recent, workspace, now, onSelect, onOpen, onOpenFile, ask, t }: {
  map: ProjectMap
  view: MapView
  node: MapNode
  scope: string
  explanation: string | undefined
  recent: readonly AiActivity[]
  workspace: IdeWorkspace
  now: number
  onSelect: (id: string) => void
  onOpen: (node: MapNode) => void
  onOpenFile: (path: string) => void
  ask: (text: string) => void
  t: Translate
}): JSX.Element {
  const members = new Set(node.files)
  const uses = view.edges.filter(edge => edge.from === node.id).sort((left, right) => right.weight - left.weight)
  const usedBy = view.edges.filter(edge => edge.to === node.id).sort((left, right) => right.weight - left.weight)
  const files = [...node.files].sort((left, right) => map.files[right]!.lines - map.files[left]!.lines).slice(0, 8)
  const packages = map.packages.map(item => ({ name: item.name, count: item.files.filter(index => members.has(index)).length }))
    .filter(item => item.count > 0).sort((left, right) => right.count - left.count).slice(0, 8)
  const total = node.languages.reduce((sum, item) => sum + item.lines, 0)
  const base = node.type === 'directory' ? `${node.id}/` : node.type === 'loose' ? node.id.slice(0, -1) : ''
  const inside = recent.filter((item) => {
    const key = item.path === undefined ? undefined : keyFromAbsolute(workspace, item.path)
    return key !== undefined && nodeForPath(view, key) === node.id
  }).slice(0, 6)
  const list = (edges: typeof uses, pick: (edge: (typeof uses)[number]) => string): ReactNode => edges.length === 0
    ? <p className={css.muted}>{t('mapNone')}</p>
    : <ul className={css.links}>{edges.map(edge => (
      <li key={pick(edge)}><button type="button" className={css.link} onClick={() => { onSelect(pick(edge)) }}>{pick(edge)}</button>
        <span className={css.muted}>{edge.weight}</span></li>
    ))}</ul>
  return (
    <div className={css.details} data-map-details>
      <h3 className={css.detailsTitle} title={node.id}>{nodeLabel(node, scope, t)}</h3>
      <p className={css.muted}>{t(KIND_LABEL[node.kind])} · {t('mapNodeStats', { files: node.files.length, lines: node.lines })}</p>
      {explanation !== undefined && <p className={css.description}>{explanation}</p>}
      <div className={css.detailActions}>
        {node.type === 'file'
          ? <Button size="sm" variant="primary" onClick={() => { onOpenFile(node.id) }}>{t('mapOpenFile')}</Button>
          : (node.openable || node.outside) && <Button size="sm" variant="primary" title={t('mapOpenTitle')} onClick={() => { onOpen(node) }}>{t('mapOpen')}</Button>}
        <Button size="sm" variant="outline" icon={<IconSparkleRegular size={14} />} onClick={() => { ask(t('mapAskAiText', { path: node.id })) }}>{t('mapAskAi')}</Button>
      </div>
      {total > 0 && <>
        <h4 className={css.section}>{t('mapLanguages')}</h4>
        <ul className={css.links}>{node.languages.slice(0, 5).map(item => (
          <li key={item.language}><span className={css.dot} style={{ background: fileColor(item.language, 'code') }} />{item.language}
            <span className={css.muted}>{Math.round((item.lines / total) * 100)}%</span></li>
        ))}</ul>
      </>}
      <h4 className={css.section}>{t('mapUses')}</h4>
      {list(uses, edge => edge.to)}
      <h4 className={css.section}>{t('mapUsedBy')}</h4>
      {list(usedBy, edge => edge.from)}
      {node.type !== 'file' && <>
        <h4 className={css.section}>{t('mapMainFiles')}</h4>
        <ul className={css.links}>{files.map(index => (
          <li key={index}><button type="button" className={css.link} title={map.files[index]!.path}
            onClick={() => { onOpenFile(map.files[index]!.path) }}>{map.files[index]!.path.slice(base.length)}</button>
          <span className={css.muted}>{compact(map.files[index]!.lines)}</span></li>
        ))}</ul>
      </>}
      {packages.length > 0 && <>
        <h4 className={css.section}>{t('mapPackages')}</h4>
        <div className={css.chips}>{packages.map(item => <span key={item.name} className={css.chip}>{item.name}</span>)}</div>
      </>}
      {inside.length > 0 && <>
        <h4 className={css.section}>{t('mapActivity')}</h4>
        <ul className={css.links}>{inside.map(item => (
          <li key={item.toolCallId}><span>{verb(item, t)}</span><span className={css.ellipsis} title={item.label}>{item.label}</span>
            <span className={css.muted}>{relativeTime(item.endedAt ?? item.startedAt, now, t)}</span></li>
        ))}</ul>
      </>}
    </div>
  )
}

/** The agents' work on this project, newest first. */
function ActivityFeed({ workspace, activity, changes, now, onOpenFile, t }: {
  workspace: IdeWorkspace
  activity: ProjectMapProps['activity']
  changes: readonly ChatChange[]
  now: number
  onOpenFile: (path: string) => void
  t: Translate
}): JSX.Element {
  const inProject = (item: AiActivity): boolean => item.path === undefined ? false : keyFromAbsolute(workspace, item.path) !== undefined
  const running = activity.active.filter(item => item.endedAt === undefined && inProject(item))
  const rows = [...running.map(item => ({ item, live: true })), ...activity.recent.filter(inProject).map(item => ({ item, live: false }))].slice(0, 40)
  return (
    <div className={css.details} data-map-activity>
      <h3 className={css.detailsTitle}>{t('mapActivity')}</h3>
      {changes.length > 0 && <p className={css.description}>{t('mapChangedFiles', { count: changes.length })}</p>}
      {rows.length === 0 ? <p className={css.muted}>{t('mapActivityEmpty')}</p> : (
        <ul className={css.feed}>
          {rows.map(({ item, live }) => {
            const key = item.path === undefined ? undefined : keyFromAbsolute(workspace, item.path)
            return (
              <li key={`${item.toolCallId}:${live ? 'now' : 'done'}`} data-live={live || undefined} data-action={item.action}>
                <span className={css.feedVerb}>{verb(item, t)}</span>
                {key !== undefined && item.action !== 'search'
                  ? <button type="button" className={css.link} title={key} onClick={() => { onOpenFile(key) }}>{key}</button>
                  : <span className={css.ellipsis} title={item.label}>{key ?? item.label}</span>}
                <span className={css.muted}>{live ? t('mapActivityNow') : relativeTime(item.endedAt ?? item.startedAt, now, t)}</span>
              </li>
            )
          })}
        </ul>
      )}
    </div>
  )
}

/**
 * The project map.
 * @param props Project, visibility, the agents' changes and activity, and window actions.
 * @returns The map.
 */
export function ProjectMapView({ workspace, visible, changes, activity, openFile, ask }: ProjectMapProps): JSX.Element {
  const t = useMapT()
  const locale = useLocale()
  const state = useProjectMap(workspace?.workspaceId ?? null, visible)
  const [mode, setMode] = useState<'architecture' | 'files'>('architecture')
  const [scope, setScope] = useState('')
  /** Scopes the user came from, so going back returns to the view they saw rather than to the parent directory. */
  const [trail, setTrail] = useState<readonly string[]>([])
  const [selected, setSelected] = useState<string | undefined>()
  const [all, setAll] = useState(false)
  const now = useNow(15_000)
  const map = state.map
  useEffect(() => { setScope(''); setTrail([]); setSelected(undefined) }, [workspace?.workspaceId])
  const view = useMemo(() => map === undefined ? undefined : mapView(map, scope), [map, scope])
  const nodes = useMemo(() => view === undefined ? [] : shownNodes(view, all), [view, all])
  const marks = useMemo(() => aiMarks(workspace, changes, activity.active), [workspace, changes, activity.active])
  const nodeActivity = useMemo(() => {
    const result = new Map<string, NodeActivity>()
    if (view === undefined) return result
    for (const [key, mark] of marks) {
      const id = nodeForPath(view, key)
      if (id === undefined) continue
      const entry = result.get(id) ?? { changed: 0 }
      if (mark === 'changed' || mark === 'created') entry.changed++
      else if (mark === 'writing' || entry.mark === undefined) entry.mark = mark
      result.set(id, entry)
    }
    return result
  }, [view, marks])
  if (workspace === null) return <div className={css.root}><div className={css.empty}>{t('mapNoProject')}</div></div>
  if (map === undefined || view === undefined) {
    return (
      <div className={css.root} data-project-map>
        <div className={css.empty} role="status">
          {state.error === undefined ? t('mapLoading') : <>
            <span>{t('mapFailed', { message: state.error })}</span>
            <Button size="sm" variant="outline" onClick={() => { loadMap(workspace.workspaceId, true) }}>{t('mapRetry')}</Button>
          </>}
        </div>
      </div>
    )
  }
  const codeFiles = map.files.filter(file => file.kind === 'code')
  const codeLines = codeFiles.reduce((sum, file) => sum + file.lines, 0)
  const explanation = map.explanation
  const selectedNode = selected === undefined ? undefined : view.nodes.find(node => node.id === selected)
  const open = (node: MapNode): void => {
    if (node.type === 'file') { openFile(node.id); return }
    const target = node.type === 'loose' ? parentOf(node.id) : node.id
    if (target === scope) return
    enter(target)
  }
  function enter(next: string): void {
    setTrail(current => [...current, scope])
    setScope(next)
    setSelected(undefined)
  }
  const jump = (next: string): void => {
    setTrail([])
    setScope(next)
    setSelected(undefined)
  }
  const back = (): void => {
    setScope(trail.at(-1) ?? parentOf(scope))
    setTrail(current => current.slice(0, -1))
    setSelected(undefined)
  }
  const explain = (): void => {
    void explainMap(workspace.workspaceId).catch((error: unknown) => {
      toast(t('mapExplainFailed', { message: error instanceof Error ? error.message : String(error) }), { tone: 'error' })
    })
  }
  const crumbs = scope === '' ? [] : scope.split('/')
  return (
    <div className={css.root} data-project-map>
      <header className={css.header}>
        <div className={css.headline}>
          <h2 className={css.title} title={workspace.path}>{map.title || workspace.title}</h2>
          <span className={css.stats}>{t('mapStats', { files: map.files.length, codeFiles: codeFiles.length, lines: compact(codeLines) })}</span>
          {map.truncated && <span className={css.warning}>{t('mapTruncated')}</span>}
          <span className={css.spacer} />
          <SegmentedControl id="project-map-mode" label={t('mapViews')} value={mode} className={css.modes}
            options={[{ value: 'architecture', label: t('mapArchitecture') }, { value: 'files', label: t('mapFiles') }]}
            onChange={(value) => { setMode(value) }} />
          <Button size="sm" variant="outline" icon={<IconSparkleRegular size={14} />} disabled={state.explaining} onClick={explain}>
            {state.explaining ? t('mapExplaining') : explanation === undefined ? t('mapExplain') : t('mapExplainAgain')}
          </Button>
          <IconAction label={t('mapRefresh')} disabled={state.loading} onClick={() => { loadMap(workspace.workspaceId, true) }}>
            <IconRefreshOutlineRegular size={16} className={state.loading ? css.spinning : undefined} />
          </IconAction>
        </div>
        {map.stack.length > 0 && (
          <div className={css.stack} aria-label={t('mapStack')}>
            {map.stack.map(item => (
              <span key={item.name} className={css.chip} data-kind={item.kind}
                title={t(item.kind === 'language' ? 'stackLanguage' : item.kind === 'framework' ? 'stackFramework' : item.kind === 'library' ? 'stackLibrary' : 'stackTool')}>
                {item.name}
              </span>
            ))}
          </div>
        )}
        {explanation !== undefined
          ? <p className={css.summary} data-map-summary><span>{explanation.summary}</span>
            <span className={css.muted}> · {t('mapExplainedBy', { model: explanation.model, time: new Date(explanation.generatedAt).toLocaleString(locale === 'zh' ? 'zh-CN' : 'en-US') })}</span></p>
          : <p className={clsx(css.summary, css.muted)}>{t('mapExplainHint')}</p>}
      </header>
      <div className={css.body}>
        <section className={css.main}>
          <nav className={css.crumbs} aria-label={t('mapRoot')}>
            <button type="button" className={css.crumb} disabled={scope === ''} onClick={() => { jump('') }}>{t('mapRoot')}</button>
            {crumbs.map((part, index) => {
              const path = crumbs.slice(0, index + 1).join('/')
              return <span key={path}><span className={css.muted}> / </span>
                <button type="button" className={css.crumb} disabled={path === scope} onClick={() => { jump(path) }}>{part}</button></span>
            })}
            {scope !== '' && <Button size="sm" variant="ghost" onClick={back}>{t('mapUp')}</Button>}
            <span className={css.spacer} />
            {mode === 'architecture' && (
              <label className={css.toggle}><input type="checkbox" checked={all} onChange={(event) => { setAll(event.target.checked) }} />{t('mapShowAll')}</label>
            )}
          </nav>
          {mode === 'architecture'
            ? <Architecture map={map} view={view} nodes={nodes} scope={scope} selected={selected} activity={nodeActivity}
              explanation={explanation?.modules} onSelect={setSelected} onOpen={open} t={t} />
            : <Files map={map} scope={scope} marks={marks} onScope={enter} onOpenFile={openFile} t={t} />}
          <div className={css.legend}>
            <span><i className={css.legendCode} />{t('mapLegendCode')}</span>
            <span><i className={css.legendTest} />{t('mapLegendTest')}</span>
            <span><i className={css.legendOther} />{t('mapLegendOther')}</span>
            <span><i className={css.legendReading} />{t('mapLegendReading')}</span>
            <span><i className={css.legendWriting} />{t('mapLegendWriting')}</span>
            <span><i className={css.legendChanged} />{t('mapLegendChanged')}</span>
          </div>
        </section>
        <aside className={css.side}>
          {selectedNode !== undefined
            ? <Details map={map} view={view} node={selectedNode} scope={scope} explanation={explanation?.modules[selectedNode.id]} recent={activity.recent}
              workspace={workspace} now={now} onSelect={setSelected} onOpen={open} onOpenFile={openFile} ask={ask} t={t} />
            : <ActivityFeed workspace={workspace} activity={activity} changes={changes} now={now} onOpenFile={openFile} t={t} />}
        </aside>
      </div>
    </div>
  )
}
