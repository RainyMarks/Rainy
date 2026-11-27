/** Side panels (node details, live AI activity feed) and breadcrumb scope bar for ProjectMap. */
import { Fragment, useEffect, useRef, type ReactNode } from 'react'
import type { IdeWorkspace } from '../../shared/ide-files-protocol.ts'
import { nodeForPath, type MapNode, type MapView } from '../../shared/map-graph.ts'
import type { MapFileKind, ProjectMap } from '../../shared/map-protocol.ts'
import type { ChatChange } from '../../shared/rpc.ts'
import { keyFromAbsolute } from '../ide/ide-paths.ts'
import { CJK_WIDE } from '../ide/viewers/viewer-utils.ts'
import { Button, IconSparkleRegular } from '../ui/index.ts'
import type { AiActivity } from './activity.ts'
import { fileColor } from './colors.ts'
import type { MapMessageKey, useMapT } from './messages.ts'
import css from './ProjectMap.module.css'

export type MapTranslate = ReturnType<typeof useMapT>

const KIND_LABEL: Record<MapFileKind, MapMessageKey> = {
  code: 'kindCode', test: 'kindTest', doc: 'kindDoc', config: 'kindConfig', asset: 'kindAsset', other: 'kindOther',
}

export function compactCount(value: number): string {
  if (value < 1000) return String(value)
  if (value < 10_000) return `${(value / 1000).toFixed(1).replace(/\.0$/u, '')}k`
  return `${Math.round(value / 1000)}k`
}

/** Shorten text to about `limit` narrow characters; CJK characters count double. */
export function fitLabel(text: string, limit: number): string {
  let width = 0
  let index = 0
  for (const char of text) {
    width += CJK_WIDE.test(char) ? 2 : 1
    if (width > limit) return `${text.slice(0, index)}…`
    index += char.length
  }
  return text
}

export function nodeLabel(node: MapNode, scope: string, t: MapTranslate): string {
  if (node.type === 'loose') {
    const directory = node.id.slice(0, -2)
    return directory === '' ? t('mapRootLoose') : t('mapLoose', { name: directory.slice(directory.lastIndexOf('/') + 1) })
  }
  if (node.outside) return node.id
  if (node.type === 'file') return node.id.slice(node.id.lastIndexOf('/') + 1)
  return scope === '' ? node.id : node.id.slice(scope.length + 1)
}

export function moduleDescription(modules: Readonly<Record<string, unknown>> | undefined, id: string): string | undefined {
  if (modules === undefined || !Object.hasOwn(modules, id)) return undefined
  const value = modules[id]
  return typeof value === 'string' && value !== '' ? value : undefined
}

function relativeTime(at: number, now: number, t: MapTranslate): string {
  const seconds = Math.max(0, Math.round((now - at) / 1000))
  if (seconds < 10) return t('mapJustNow')
  if (seconds < 60) return t('mapSecondsAgo', { count: seconds })
  if (seconds < 3600) return t('mapMinutesAgo', { count: Math.round(seconds / 60) })
  return t('mapHoursAgo', { count: Math.round(seconds / 3600) })
}

function verb(activity: AiActivity, t: MapTranslate): string {
  switch (activity.action) {
    case 'read': return t('mapRead')
    case 'write': return t('mapWrite')
    case 'edit': return t('mapEdit')
    case 'search': return t('mapSearch')
    case 'command': return t('mapCommand')
    default: return t('mapOther')
  }
}

/** Details of the selected box. */
export function Details({ map, view, node, scope, explanation, recent, workspace, now, onSelect, onOpen, onOpenFile, ask, t }: {
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
  t: MapTranslate
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
          <span className={css.muted}>{compactCount(map.files[index]!.lines)}</span></li>
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
export function ActivityFeed({ workspace, activity, changes, now, onOpenFile, t }: {
  workspace: IdeWorkspace
  activity: { readonly active: readonly AiActivity[]; readonly recent: readonly AiActivity[] }
  changes: readonly ChatChange[]
  now: number
  onOpenFile: (path: string) => void
  t: MapTranslate
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
 * Where in the project the map is, the way back, and the view's own controls. The path scrolls sideways when it is
 * longer than the row, ending at the current directory, so 返回 and the controls stay in reach in a narrow pane.
 */
export function ScopeBar({ scope, onJump, onBack, children, t }: {
  scope: string
  onJump: (scope: string) => void
  onBack: () => void
  children: ReactNode
  t: MapTranslate
}): JSX.Element {
  const trail = useRef<HTMLDivElement>(null)
  useEffect(() => {
    const element = trail.current
    if (element !== null) element.scrollLeft = element.scrollWidth
  }, [scope])
  const crumbs = scope === '' ? [] : scope.split('/')
  return (
    <nav className={css.crumbs} aria-label={t('mapRoot')}>
      <div className={css.path}>
        <button type="button" className={css.crumb} disabled={scope === ''} onClick={() => { onJump('') }}>{t('mapRoot')}</button>
        {crumbs.length > 0 && (
          <div ref={trail} className={css.trail} onWheel={(event) => {
            const element = event.currentTarget
            if (event.deltaX === 0 && element.scrollWidth > element.clientWidth) element.scrollLeft += event.deltaY
          }}>
            {crumbs.map((part, index) => {
              const path = crumbs.slice(0, index + 1).join('/')
              return <Fragment key={path}><span className={css.separator} aria-hidden="true">/</span>
                <button type="button" className={css.crumb} title={path} disabled={path === scope} onClick={() => { onJump(path) }}>{part}</button></Fragment>
            })}
          </div>
        )}
        {scope !== '' && <Button size="sm" variant="ghost" className={css.up} onClick={onBack}>{t('mapUp')}</Button>}
      </div>
      {children}
    </nav>
  )
}
