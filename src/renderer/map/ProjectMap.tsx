/**
 * The project map: the AI's tour of the project (how to run it, what to read first, how its key features flow through
 * the code, where to make changes), the AI footprint (what the chat's agent read, changed and ran, turn by turn), and
 * the structure (the architecture as boxes and imports, or every file as blocks, marked as the agent works).
 */
import { useCallback, useEffect, useMemo, useRef, useState, type PointerEvent as ReactPointerEvent, type WheelEvent } from 'react'
import clsx from 'clsx'
import type { WorkspaceId } from '../../shared/ide-files-protocol.ts'
import { mapView, nodeForPath, parentOf, type MapNode, type MapView } from '../../shared/map-graph.ts'
import type { ProjectMap } from '../../shared/map-protocol.ts'
import type { ChatChange, SessionId } from '../../shared/rpc.ts'
import { errorMessage } from '../ide/viewers/viewer-utils.ts'
import { Button, IconAction, IconRefreshOutlineRegular, SegmentedControl, toast } from '../ui/index.ts'
import { FootprintView } from './Footprint.tsx'
import { ActivityFeed, compactCount, Details, fitLabel, moduleDescription, nodeLabel, ScopeBar, type MapTranslate } from './MapSidePanel.tsx'
import { TourView } from './Tour.tsx'
import type { AiActivity } from './activity.ts'
import { fileColor } from './colors.ts'
import { fitTransform, layoutForPane, MIN_SCALE, NODE_HEIGHT, NODE_WIDTH, type ViewTransform } from './layout.ts'
import { explainMap, loadMap, useProjectMap } from './map-store.ts'
import { aiMarks, type AiMark } from './marks.ts'
import { useMapT } from './messages.ts'
import { layoutTreemap } from './treemap.ts'
import { useWorkspaceLookup } from './workspace-lookup.ts'
import css from './ProjectMap.module.css'

/** What the map needs from the window. */
export interface ProjectMapProps {
  /** The project; the map finds it among the Host's projects. */
  readonly workspaceId: WorkspaceId
  /** The map is on screen; it loads and follows changes only then. */
  readonly visible: boolean
  /** The chat shown beside the map, whose footprint the map lists. */
  readonly sessionId: SessionId | null
  /** Files the agent changed in the current chat and the user has not reviewed. */
  readonly changes: readonly ChatChange[]
  /** The agents' running and recent tool calls. */
  readonly activity: { readonly active: readonly AiActivity[]; readonly recent: readonly AiActivity[] }
  /** Open a file of the project in the editor, by editor key, at a 1-based line when given. */
  readonly openFile: (path: string, line?: number) => void
  /** Send a message to the AI. */
  readonly ask: (text: string) => void
}

/** How the agent touches a box: the strongest mark of its files, and how many of them it changed. */
interface NodeActivity { mark?: 'reading' | 'writing' | undefined; changed: number }

/** Adaptive badge dimensions so 3+ digit change counts never overflow the pill or the node box. */
export function changeBadgeLayout(changed: number): { readonly label: string; readonly width: number; readonly x: number; readonly textX: number } {
  const label = changed > 999 ? '✎999+' : `✎${changed}`
  const width = Math.max(26, 12 + label.length * 6)
  return { label, width, x: NODE_WIDTH - width - 8, textX: width / 2 }
}

/** @returns The element's size, kept current. */
function useSize(element: HTMLElement | null): { width: number; height: number } {
  const [size, setSize] = useState({ width: 0, height: 0 })
  useEffect(() => {
    if (element === null) return
    const measure = (): void => {
      setSize(previous => previous.width === element.clientWidth && previous.height === element.clientHeight
        ? previous : { width: element.clientWidth, height: element.clientHeight })
    }
    measure()
    const observer = new ResizeObserver(measure)
    observer.observe(element)
    return () => { observer.disconnect() }
  }, [element])
  return size
}

/** @returns The current time, updated every few seconds while the map is shown; a hidden map does not render for it. */
function useNow(intervalMs: number, running: boolean): number {
  const [now, setNow] = useState(() => Date.now())
  useEffect(() => {
    if (!running) return undefined
    setNow(Date.now())
    const timer = setInterval(() => { setNow(Date.now()) }, intervalMs)
    return () => { clearInterval(timer) }
  }, [intervalMs, running])
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
  t: MapTranslate
}): JSX.Element {
  const [frame, setFrame] = useState<HTMLDivElement | null>(null)
  const size = useSize(frame)
  const ids = useMemo(() => nodes.map(node => node.id), [nodes])
  // Quantize aspect ratio into 0.1 buckets so continuous pane resizing reuses the same expensive graph layout
  // while `fitTransform` still smoothly adapts pan/zoom on every pixel.
  const layoutPane = useMemo(() => {
    if (size.width <= 0 || size.height <= 0) return { width: 0, height: 0 }
    const bucket = Math.max(2, Math.min(40, Math.round((size.width / size.height) * 10)))
    return { width: bucket * 100, height: 1000 }
  }, [size.width, size.height])
  const layout = useMemo(() => layoutForPane(ids, view.edges, layoutPane), [ids, view.edges, layoutPane])
  const [transform, setTransform] = useState<ViewTransform>({ x: 0, y: 0, k: 1 })
  const drag = useRef<{ x: number; y: number; originX: number; originY: number; moved: boolean } | undefined>(undefined)
  const fitView = useCallback((): void => {
    if (size.width === 0 || size.height === 0) return
    setTransform(fitTransform(size, layout))
  }, [size, layout])
  // A new scope, other boxes or a resized pane show the whole drawing again; a rescan with the same boxes keeps the view.
  const shape = `${ids.join('\n')}:${layout.width}x${layout.height}`
  const latestLayout = useRef(layout)
  latestLayout.current = layout
  useEffect(() => {
    if (size.width === 0 || size.height === 0) return
    setTransform(fitTransform(size, latestLayout.current))
  }, [shape, size.width, size.height])
  const zoom = (factor: number, centreX = size.width / 2, centreY = size.height / 2): void => {
    setTransform((current) => {
      const k = Math.min(3, Math.max(MIN_SCALE, current.k * factor))
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
  const byId = useMemo(() => new Map(nodes.map(node => [node.id, node])), [nodes])
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
            const node = byId.get(place.id)
            if (node === undefined) return null
            const touch = activity.get(node.id)
            const description = moduleDescription(explanation, node.id)
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
                <text x={14} y={24} className={css.nodeName}>{fitLabel(`${node.outside ? '↗ ' : ''}${nodeLabel(node, scope, t)}`, 24)}</text>
                <text x={14} y={43} className={css.nodeDetail}>
                  {description !== undefined ? fitLabel(description, 27) : t('mapNodeStats', { files: node.files.length, lines: compactCount(node.lines) })}
                </text>
                {total > 0 && node.languages.slice(0, 4).map((item) => {
                  const width = ((NODE_WIDTH - 28) * item.lines) / total
                  const segment = <rect key={item.language} x={14 + offset} y={NODE_HEIGHT - 12} width={Math.max(1, width)} height={3} rx={1.5}
                    style={{ fill: fileColor(item.language, 'code') }} />
                  offset += width
                  return segment
                })}
                {touch?.mark !== undefined && <circle className={css.pulse} cx={NODE_WIDTH - 14} cy={14} r={5} />}
                {touch !== undefined && touch.changed > 0 && (() => {
                  const badge = changeBadgeLayout(touch.changed)
                  return (
                    <g transform={`translate(${badge.x} ${NODE_HEIGHT - 22})`} className={css.badge} data-change-badge>
                      <title>{String(touch.changed)}</title>
                      <rect width={badge.width} height={15} rx={7.5} />
                      <text x={badge.textX} y={11} textAnchor="middle">{badge.label}</text>
                    </g>
                  )
                })()}
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
  t: MapTranslate
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

/**
 * The project map.
 * @param props Project, visibility, the agents' changes and activity, and window actions.
 * @returns The map.
 */
export function ProjectMapView({ workspaceId, visible, sessionId, changes, activity, openFile, ask }: ProjectMapProps): JSX.Element {
  const t = useMapT()
  const lookup = useWorkspaceLookup(workspaceId)
  const workspace = lookup.status === 'ready' ? lookup.workspace : null
  const state = useProjectMap(workspace?.workspaceId ?? null, visible)
  const [tab, setTab] = useState<'tour' | 'footprint' | 'structure'>('tour')
  const [mode, setMode] = useState<'architecture' | 'files'>('architecture')
  const [scope, setScope] = useState('')
  /** Scopes the user came from, so going back returns to the view they saw rather than to the parent directory. */
  const [trail, setTrail] = useState<readonly string[]>([])
  const [selected, setSelected] = useState<string | undefined>()
  const [all, setAll] = useState(false)
  const now = useNow(15_000, visible)
  const map = state.map
  useEffect(() => { setScope(''); setTrail([]); setSelected(undefined) }, [workspace?.workspaceId])
  const view = useMemo(() => map === undefined ? undefined : mapView(map, scope), [map, scope])
  const directories = useMemo(() => {
    const found = new Set<string>()
    for (const file of map?.files ?? []) {
      let path = file.path
      for (let slash = path.lastIndexOf('/'); slash > 0; slash = path.lastIndexOf('/')) {
        path = path.slice(0, slash)
        if (found.has(path)) break
        found.add(path)
      }
    }
    return found
  }, [map])
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
  if (workspace === null || map === undefined || view === undefined) {
    const failure = lookup.status === 'failed' ? { title: t('mapProjectFailed'), message: lookup.message, retry: lookup.retry }
      : workspace !== null && state.error !== undefined ? { title: t('mapFailed'), message: state.error, retry: () => { loadMap(workspace.workspaceId, true) } }
        : undefined
    return (
      <div className={css.root} data-project-map>
        {failure !== undefined
          ? <div className={css.empty} role="alert">
            <span>{failure.title}</span>
            {/* The Host's or the connection's own words, which may be English: a detail under the message, not the message. */}
            <span className={css.failureDetail}>{failure.message}</span>
            <Button size="sm" variant="outline" onClick={failure.retry}>{t('mapRetry')}</Button>
          </div>
          : <div className={css.empty} role="status">{t(lookup.status === 'missing' ? 'mapProjectMissing' : 'mapLoading')}</div>}
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
      toast(t('tourFailed', { message: errorMessage(error) }), { tone: 'error' })
    })
  }
  const showDirectory = (path: string): void => {
    setTab('structure')
    jump(path)
  }
  return (
    <div className={css.root} data-project-map>
      <header className={css.header}>
        <div className={css.headline}>
          <h2 className={css.title} title={workspace.path}>{map.title || workspace.title}</h2>
          <span className={css.stats}>{t('mapStats', { files: map.files.length, codeFiles: codeFiles.length, lines: compactCount(codeLines) })}</span>
          {map.truncated && <span className={css.warning}>{t('mapTruncated')}</span>}
          <span className={css.spacer} />
          <SegmentedControl id="project-map-tab" label={t('mapViews')} value={tab} className={css.tabs}
            options={[{ value: 'tour', label: t('tabTour') }, { value: 'footprint', label: t('tabFootprint') }, { value: 'structure', label: t('tabStructure') }]}
            onChange={(value) => { setTab(value) }} />
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
      </header>
      {tab === 'tour' && <TourView explanation={explanation} explaining={state.explaining} isDirectory={path => directories.has(path)}
        onGenerate={explain} onOpenFile={openFile} onShowDirectory={showDirectory} t={t} />}
      {tab === 'footprint' && <FootprintView workspace={workspace} sessionId={sessionId} onOpenFile={openFile} t={t} />}
      {tab === 'structure' && <div className={css.body}>
        <section className={css.main}>
          <ScopeBar scope={scope} onJump={jump} onBack={back} t={t}>
            <SegmentedControl id="project-map-mode" label={t('mapViews')} value={mode} className={css.modes}
              options={[{ value: 'architecture', label: t('mapArchitecture') }, { value: 'files', label: t('mapFiles') }]}
              onChange={(value) => { setMode(value) }} />
            {mode === 'architecture' && (
              <label className={css.toggle}><input type="checkbox" checked={all} onChange={(event) => { setAll(event.target.checked) }} />{t('mapShowAll')}</label>
            )}
          </ScopeBar>
          {mode === 'architecture'
            ? <Architecture map={map} view={view} nodes={nodes} scope={scope} selected={selected} activity={nodeActivity}
              explanation={explanation?.modules} onSelect={setSelected} onOpen={open} t={t} />
            : <Files map={map} scope={scope} marks={marks} onScope={enter} onOpenFile={openFile} t={t} />}
          <div className={css.legend}>
            <span><i className={css.legendCode} />{t('mapLegendCode')}</span>
            <span><i className={css.legendTest} />{t('mapLegendTest')}</span>
            <span><i className={css.legendOther} />{t('mapLegendOther')}</span>
            {mode === 'architecture' && nodes.some(node => node.outside) && <span><i className={css.legendOutside} />{t('mapLegendOutside')}</span>}
            <span><i className={css.legendReading} />{t('mapLegendReading')}</span>
            <span><i className={css.legendWriting} />{t('mapLegendWriting')}</span>
            <span><i className={css.legendChanged} />{t('mapLegendChanged')}</span>
          </div>
        </section>
        <aside className={css.side}>
          {selectedNode !== undefined
            ? <Details map={map} view={view} node={selectedNode} scope={scope} explanation={moduleDescription(explanation?.modules, selectedNode.id)} recent={activity.recent}
              workspace={workspace} now={now} onSelect={setSelected} onOpen={open} onOpenFile={openFile} ask={ask} t={t} />
            : <ActivityFeed workspace={workspace} activity={activity} changes={changes} now={now} onOpenFile={openFile} t={t} />}
        </aside>
      </div>}
    </div>
  )
}
