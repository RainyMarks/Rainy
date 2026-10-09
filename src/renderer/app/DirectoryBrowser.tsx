/**
 * In-app directory browser for the Host's filesystem: a breadcrumb with a click-to-edit path, and a two-column
 * view (the listed level and the selected folder's children). Open adopts the selected folder, else the listed level.
 */
import { useCallback, useEffect, useRef, useState } from 'react'
import clsx from 'clsx'
import type { IdeDirectoryListing } from '../../shared/ide-files-protocol.ts'
import {
  Button, IconCheckOutlineRegular, IconChevronRightOutlineRegular, IconEditOutlineRegular, IconFolderCloseRegular,
  IconFolderOpenRegular, IconPlusOutlineRegular, Menu, Modal,
} from '../ui/index.ts'
import { callIde } from '../ide/ide-api.ts'
import { useAppT } from './messages.ts'
import css from './DirectoryBrowser.module.css'

type Entry = IdeDirectoryListing['entries'][number]

/** Lists one level (`undefined` lists the Host home directory), including hidden folders. */
export type ListDirectory = (path: string | undefined, signal: AbortSignal) => Promise<IdeDirectoryListing>
/** Creates one directory by absolute path and lists it. */
export type CreateDirectory = (path: string) => Promise<IdeDirectoryListing>

const hostList: ListDirectory = (path, signal) => callIde({ op: 'directories.list', path, showHidden: true }, signal)
const hostCreate: CreateDirectory = path => callIde({ op: 'directories.create', path })

/** How long a listing may run before the loading note appears. */
const SLOW_SCAN_MS = 300

function separatorOf(path: string): '\\' | '/' {
  return /^[A-Za-z]:/u.test(path) || path.startsWith('\\\\') ? '\\' : '/'
}

function joinPath(base: string, name: string): string {
  const separator = separatorOf(base)
  return base.endsWith(separator) ? base + name : base + separator + name
}

function samePath(left: string, right: string): boolean {
  return separatorOf(left) === '\\' ? left.toLowerCase() === right.toLowerCase() : left === right
}

/**
 * Split an absolute path into its ancestors, root first.
 * @param path Absolute POSIX, drive or UNC path.
 * @returns Breadcrumb names with the path each one opens.
 */
export function pathCrumbs(path: string): { readonly name: string; readonly path: string }[] {
  if (separatorOf(path) === '/') {
    const parts = path.split('/').filter(part => part !== '')
    return [{ name: '/', path: '/' }, ...parts.map((part, index) => ({ name: part, path: `/${parts.slice(0, index + 1).join('/')}` }))]
  }
  const parts = path.split(/[\\/]+/u).filter(part => part !== '')
  const unc = path.startsWith('\\\\')
  const root = unc ? `\\\\${parts[0] ?? ''}\\${parts[1] ?? ''}\\` : `${parts[0] ?? ''}\\`
  const rest = parts.slice(unc ? 2 : 1)
  return [{ name: root, path: root }, ...rest.map((part, index) => ({ name: part, path: root + rest.slice(0, index + 1).join('\\') }))]
}

function failureText(error: unknown): string {
  return error instanceof Error ? error.message : String(error)
}

function Column({ entries, selected, showHidden, filter, busy, onPick }: {
  entries: readonly Entry[]
  selected: string | null
  showHidden: boolean
  filter: string | null
  busy: boolean
  onPick: (entry: Entry) => void
}) {
  const needle = filter?.toLowerCase() ?? ''
  const displayable = (entry: Entry): boolean => showHidden || !entry.hidden || needle.startsWith('.')
  const matches = (entry: Entry): boolean => displayable(entry) && entry.name.toLowerCase().startsWith(needle)
  const narrowing = needle !== '' && entries.some(matches)
  const visible = entries.filter(entry => entry.path === selected || (narrowing ? matches(entry) : showHidden || !entry.hidden))
  return (
    <div className={css.column} role="list">
      {visible.map((entry) => {
        const current = entry.path === selected
        return (
          <span key={entry.path} role="listitem" className={css.rowSeat}>
            <button type="button" aria-current={current || undefined} className={clsx(css.row, current && css.rowSelected)}
              disabled={busy} onClick={() => { onPick(entry) }}>
              {current ? <IconFolderOpenRegular size={16} className={css.rowIconSelected} /> : <IconFolderCloseRegular size={16} className={css.rowIcon} />}
              <span className={css.rowName}>{entry.name}</span>
              <IconChevronRightOutlineRegular size={12} className={css.rowChevron} />
            </button>
          </span>
        )
      })}
    </div>
  )
}

/**
 * Render the browser dialog.
 * @param props.open Whether the dialog is shown; each opening starts at the Host home directory.
 * @param props.busy The owner is adopting the chosen folder: actions are disabled.
 * @param props.onOpen Receives the chosen absolute directory.
 * @param props.onClose Closes without a choice.
 * @param props.listDirectory Listing request; defaults to the Host's `directories.list`.
 * @param props.createDirectory Creation request; defaults to the Host's `directories.create`.
 * @returns The dialog, or nothing while closed.
 */
export function DirectoryBrowser({ open, busy, onOpen, onClose, listDirectory = hostList, createDirectory = hostCreate }: {
  open: boolean
  busy: boolean
  onOpen: (path: string) => void
  onClose: () => void
  listDirectory?: ListDirectory | undefined
  createDirectory?: CreateDirectory | undefined
}) {
  const t = useAppT()
  const [parent, setParent] = useState<IdeDirectoryListing | null>(null)
  const [selected, setSelected] = useState<Entry | null>(null)
  const [child, setChild] = useState<IdeDirectoryListing | null>(null)
  const [loading, setLoading] = useState(false)
  const [slow, setSlow] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [pathDraft, setPathDraft] = useState<string | null>(null)
  const [showHidden, setShowHidden] = useState(false)
  const [locations, setLocations] = useState(false)
  const [folderDraft, setFolderDraft] = useState<string | null>(null)
  const [creating, setCreating] = useState(false)
  const [createError, setCreateError] = useState<string | null>(null)
  const sequence = useRef(0)
  const controller = useRef<AbortController | null>(null)
  const generation = useRef(0)
  const crumbTrail = useRef<HTMLSpanElement | null>(null)

  /** Supersede any running listing and start a new one. */
  const begin = useCallback((): { seq: number; signal: AbortSignal } => {
    controller.current?.abort()
    const next = new AbortController()
    controller.current = next
    return { seq: ++sequence.current, signal: next.signal }
  }, [])
  useEffect(() => () => { sequence.current++; generation.current++; controller.current?.abort() }, [])

  /** List a level and show it beside its parent when the parent lists it; otherwise show it alone. */
  const navigate = useCallback((path?: string) => {
    const { seq, signal } = begin()
    setLoading(true)
    setError(null)
    void (async () => {
      try {
        const target = await listDirectory(path, signal)
        if (seq !== sequence.current) return
        let level: IdeDirectoryListing | undefined
        if (target.parent !== null) {
          try { level = await listDirectory(target.parent, signal) }
          catch (_parentUnreadable) {
            // The target listed fine; an unreadable parent only means the single-column view.
            level = undefined
          }
        }
        if (seq !== sequence.current) return
        const match = level?.entries.find(entry => samePath(entry.path, target.path))
        if (level !== undefined && match !== undefined) {
          setParent(level)
          setSelected(match)
          setChild(target)
        } else {
          setParent(target)
          setSelected(null)
          setChild(null)
        }
        setPathDraft(null)
      } catch (reason) {
        if (seq === sequence.current) setError(failureText(reason))
      } finally {
        if (seq === sequence.current) setLoading(false)
      }
    })()
  }, [begin, listDirectory])

  /** Select a folder of the listed level and show its children. */
  const select = useCallback((entry: Entry) => {
    const { seq, signal } = begin()
    setPathDraft(null)
    setSelected(entry)
    setChild(null)
    setLoading(true)
    setError(null)
    listDirectory(entry.path, signal).then((next) => {
      if (seq !== sequence.current) return
      setChild(next)
      setLoading(false)
    }, (reason: unknown) => {
      if (seq !== sequence.current) return
      setLoading(false)
      setError(failureText(reason))
      setSelected(null)
    })
  }, [begin, listDirectory])

  useEffect(() => {
    generation.current++
    if (open) {
      setParent(null)
      setSelected(null)
      setChild(null)
      setShowHidden(false)
      setCreating(false)
      navigate()
      return
    }
    sequence.current++
    controller.current?.abort()
    setLoading(false)
    setError(null)
    setPathDraft(null)
    setFolderDraft(null)
    setCreateError(null)
  }, [open, navigate])

  useEffect(() => {
    if (!loading) { setSlow(false); return }
    const timer = window.setTimeout(() => { setSlow(true) }, SLOW_SCAN_MS)
    return () => { window.clearTimeout(timer) }
  }, [loading])

  const level = child ?? parent
  const crumbs = level === null ? [] : pathCrumbs(level.path)
  const crumbTail = crumbs.at(-1)?.path
  useEffect(() => {
    const trail = crumbTrail.current
    if (trail !== null) trail.scrollLeft = trail.scrollWidth
  }, [crumbTail])

  if (!open) return null
  const target = selected?.path ?? parent?.path ?? null
  const targetName = selected?.name ?? crumbs.at(-1)?.name ?? ''
  const inert = busy || folderDraft !== null
  const editing = pathDraft !== null
  // While the draft names the last column's directory, its final segment filters that column.
  const draftTail = (() => {
    if (pathDraft === null || level === null) return null
    const separator = separatorOf(level.path)
    const cut = separator === '\\' ? Math.max(pathDraft.lastIndexOf('\\'), pathDraft.lastIndexOf('/')) : pathDraft.lastIndexOf('/')
    if (cut === -1) return null
    const directory = pathDraft.slice(0, cut + 1)
    const levelDirectory = level.path.endsWith(separator) ? level.path : level.path + separator
    return samePath(directory.replaceAll('/', separator === '\\' ? '\\' : '/'), levelDirectory) ? pathDraft.slice(cut + 1) : null
  })()
  const confirmCreate = (): void => {
    if (target === null || folderDraft === null || creating || folderDraft.trim() === '') return
    const name = folderDraft
    const at = generation.current
    setCreating(true)
    setCreateError(null)
    createDirectory(joinPath(target, name)).then((created) => {
      if (at !== generation.current) return
      setCreating(false)
      setFolderDraft(null)
      const { seq, signal } = begin()
      setLoading(true)
      setError(null)
      listDirectory(target, signal).then((listed) => {
        if (seq !== sequence.current) return
        setParent(listed)
        setSelected(listed.entries.find(entry => samePath(entry.path, created.path)) ?? { name, path: created.path, hidden: false })
        setChild(created)
        setLoading(false)
      }, (reason: unknown) => {
        if (seq !== sequence.current) return
        setLoading(false)
        setError(failureText(reason))
      })
    }, (reason: unknown) => {
      if (at !== generation.current) return
      setCreating(false)
      setCreateError(failureText(reason))
    })
  }
  const roots = level?.roots ?? []

  return (
    <Modal open={open} headless title={t('browserTitle')} className={css.dialog}
      onClose={() => {
        if (folderDraft !== null || busy) return
        if (editing) { setPathDraft(null); return }
        onClose()
      }}>
      <div className={css.header}>
        <h2 className={css.title}>{t('browserTitle')}</h2>
        <div className={css.crumbBar}>
          {pathDraft === null ? <>
            <span className={css.crumbTrail} role="navigation" ref={crumbTrail}>
              {crumbs.map((crumb, index) => (
                <span key={crumb.path} className={css.crumbSeat}>
                  {index > 0 && <IconChevronRightOutlineRegular size={12} className={css.crumbChevron} />}
                  <button type="button" className={css.crumb} disabled={inert} onClick={() => { navigate(crumb.path) }}>{crumb.name}</button>
                </span>
              ))}
            </span>
            <button type="button" className={css.crumbEditZone} aria-label={t('browserEditPath')} title={t('browserEditPath')} disabled={inert}
              onClick={() => {
                sequence.current++
                controller.current?.abort()
                setLoading(false)
                if (level === null) { setPathDraft(''); return }
                const base = level.path
                const separator = separatorOf(base)
                setPathDraft(base.endsWith(separator) ? base : base + separator)
              }}>
              <IconEditOutlineRegular size={14} className={css.crumbEditGlyph} />
            </button>
          </> : (
            <input className={css.pathInput} value={pathDraft} aria-label={t('browserEditPath')} data-modal-autofocus autoFocus disabled={inert}
              onChange={(event) => { setPathDraft(event.target.value) }}
              onKeyDown={(event) => {
                if (event.nativeEvent.isComposing) return
                if (event.key === 'Enter' && pathDraft.trim() !== '') { event.preventDefault(); navigate(pathDraft) }
                else if (event.key === 'Escape') { event.preventDefault(); setPathDraft(null) }
              }} />
          )}
        </div>
      </div>
      <div className={css.content}>
        <div className={css.millerRow}>
          {parent !== null && <Column entries={parent.entries} selected={selected?.path ?? null} showHidden={showHidden}
            filter={child === null ? draftTail : null} busy={inert} onPick={select} />}
          {selected !== null && <span className={css.divider} />}
          {selected !== null && child !== null && <Column entries={child.entries} selected={null} showHidden={showHidden}
            filter={draftTail} busy={inert} onPick={(entry) => { setParent(child); select(entry) }} />}
        </div>
        {loading && slow && <div className={clsx(css.status, css.loadingFloat)} role="status">{t('browserLoading')}</div>}
        {error !== null && <div className={css.error} role="alert">{error}</div>}
      </div>
      <div className={css.footerBar}>
        <Button variant="outline" icon={<IconPlusOutlineRegular size={14} />} disabled={parent === null || loading || inert || editing}
          onClick={() => { setFolderDraft(''); setCreateError(null) }}>
          {t('browserNewFolder')}
        </Button>
        {roots.length > 0 && <Menu open={locations} onClose={() => { setLocations(false) }} portal compact side="top"
          anchor={<button type="button" className={css.footerToggle} aria-haspopup="menu" aria-expanded={locations} disabled={inert}
            onClick={() => { setLocations(!locations) }}>{t('browserLocations')}</button>}
          items={roots.map(root => ({ id: root, label: root }))}
          onSelect={(root) => { setLocations(false); navigate(root) }} />}
        <button type="button" className={clsx(css.footerToggle, showHidden && css.footerToggleActive)} aria-pressed={showHidden}
          disabled={inert} onClick={() => { setShowHidden(value => !value) }}>
          {t('browserShowHidden')}
          {showHidden && <IconCheckOutlineRegular size={14} />}
        </button>
        <span className={css.footerGap} />
        <Button variant="outline" className={css.footerAction} disabled={inert} onClick={onClose}>{t('browserCancel')}</Button>
        <Button variant="primary" className={css.footerAction} disabled={target === null || loading || inert || editing}
          onClick={() => { if (target !== null) onOpen(target) }}>
          {t('browserOpen')}
        </Button>
      </div>
      <Modal open={folderDraft !== null} headless title={t('browserNewFolder')} className={css.createDialog}
        onClose={() => { if (!creating) setFolderDraft(null) }}>
        <div className={css.createBody}>
          <h3 className={css.createTitle}>{t('browserNewFolder')}</h3>
          <p className={css.createIn}>{t('browserCreateIn', { name: targetName })}</p>
          <input className={css.createInput} value={folderDraft ?? ''} aria-label={t('browserFolderName')}
            placeholder={t('browserUntitledFolder')} data-modal-autofocus disabled={creating}
            onChange={(event) => { setFolderDraft(event.target.value) }}
            onKeyDown={(event) => {
              if (event.key === 'Enter' && !event.nativeEvent.isComposing) { event.preventDefault(); confirmCreate() }
            }} />
          {createError !== null && <div className={css.error} role="alert">{createError}</div>}
          <div className={css.createActions}>
            <Button variant="outline" disabled={creating} onClick={() => { setFolderDraft(null) }}>{t('browserCancel')}</Button>
            <Button variant="primary" disabled={creating || folderDraft === null || folderDraft.trim() === ''} onClick={confirmCreate}>
              {t('browserCreate')}
            </Button>
          </div>
        </div>
      </Modal>
    </Modal>
  )
}
