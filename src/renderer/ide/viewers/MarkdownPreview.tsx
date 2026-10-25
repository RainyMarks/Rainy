/** Markdown preview of an editor buffer: follows unsaved edits and keeps its scroll position in step with the source. */
import { useEffect, useMemo, useRef, useState } from 'react'
import type { IdeRootId, WorkspaceId } from '../../../shared/ide-files-protocol.ts'
import { CodeBlock, MarkdownDocument, type MarkdownOptions } from '../../chat/Markdown.tsx'
import markdownCss from '../../chat/Markdown.module.css'
import { frontMatterContent, splitFrontMatter } from './front-matter.ts'
import { isMermaid, onThemeChange, renderMermaid } from './mermaid.ts'
import { useViewerT } from './messages.ts'
import type { ScrollSync } from './scroll-sync.ts'
import css from './Viewers.module.css'

/** Above this size the preview waits for a pause in typing instead of following every keystroke. */
const LIVE_LIMIT = 256 * 1024
/** After an edit a diagram is drawn again once typing pauses this long, instead of queueing a drawing per keystroke. */
const REDRAW_DELAY = 300
/** Editor lines arriving this soon after the user scrolled the preview are ignored: they would fight the user's scrolling. */
const USER_SCROLL_HOLD = 250

interface Block { line: number; top: number }

function blocks(stage: HTMLElement): Block[] {
  const origin = stage.getBoundingClientRect().top - stage.scrollTop
  return [...stage.querySelectorAll<HTMLElement>('[data-line]')].map(element => ({ line: Number(element.dataset.line), top: element.getBoundingClientRect().top - origin }))
    .filter(block => Number.isFinite(block.line)).sort((left, right) => left.top - right.top)
}

/** @returns The scroll offset that shows `line` at the top, interpolated between the blocks around it. */
function topForLine(list: readonly Block[], line: number): number | undefined {
  if (list.length === 0) return undefined
  let previous = list[0]!
  for (const block of list) {
    if (block.line > line) {
      if (block.line === previous.line) return previous.top
      return previous.top + (Math.max(0, line - previous.line) / (block.line - previous.line)) * (block.top - previous.top)
    }
    previous = block
  }
  return previous.top
}

/** @returns The source line at scroll offset `top`, interpolated between the blocks around it. */
function lineForTop(list: readonly Block[], top: number): number | undefined {
  if (list.length === 0) return undefined
  let previous = list[0]!
  for (const block of list) {
    if (block.top > top) {
      if (block.top === previous.top || top < previous.top) return previous.line
      return previous.line + ((top - previous.top) / (block.top - previous.top)) * (block.line - previous.line)
    }
    previous = block
  }
  return previous.line
}

/** A ```mermaid fence as its diagram, drawn again when its source or the theme changes; a failure shows why and the source. */
function MermaidDiagram({ source, line }: { source: string; line: number | undefined }): JSX.Element {
  const t = useViewerT()
  const [drawn, setDrawn] = useState<{ svg: string } | { error: string } | undefined>(undefined)
  const [theme, setTheme] = useState(0)
  const drawnSource = useRef<string | undefined>(undefined)
  useEffect(() => onThemeChange(() => { setTheme(value => value + 1) }), [])
  useEffect(() => {
    let cancelled = false
    const delay = drawnSource.current === undefined || drawnSource.current === source ? 0 : REDRAW_DELAY
    const timer = window.setTimeout(() => {
      renderMermaid(source).then((svg) => { if (!cancelled) setDrawn({ svg }) },
        (error: unknown) => { if (!cancelled) setDrawn({ error: error instanceof Error ? error.message : String(error) }) })
        .finally(() => { if (!cancelled) drawnSource.current = source })
    }, delay)
    return () => { cancelled = true; window.clearTimeout(timer) }
  }, [source, theme])
  if (drawn === undefined) return <div className={markdownCss.diagram} data-line={line}>{t('mermaidLoading')}</div>
  if ('error' in drawn) {
    return (
      <div data-line={line}>
        <div className={markdownCss.diagramError} role="alert">{t('mermaidFailed', { error: drawn.error })}</div>
        <CodeBlock code={source} language="mermaid" actions={false} />
      </div>
    )
  }
  // Mermaid's strict security level sanitizes the SVG it returns.
  return <div className={markdownCss.diagram} data-line={line} dangerouslySetInnerHTML={{ __html: drawn.svg }} />
}

/**
 * @param props.text Current buffer text, including unsaved edits.
 * @param props.base Project, mounted directory and the document's directory, for relative links and images.
 * @param props.version Observed file version; images reload when it changes.
 * @param props.openLocal Open a project file a link names; the path is resolved against the document's directory.
 * @param props.sync Editor scroll positions to follow, in split view.
 * @param props.onScrollLine The user scrolled the preview, to the 1-based source line now at its top; at most once per
 *   frame, and never for scrolling that `sync` caused.
 * @returns The preview.
 */
export function MarkdownPreview({ text, base, version, openLocal, sync, onScrollLine }: {
  text: string
  base: { workspaceId: WorkspaceId; rootId?: IdeRootId | undefined; directory: string }
  version: string | null
  openLocal: (target: { path: string; line?: number | undefined }) => void
  sync?: ScrollSync | undefined
  onScrollLine?: ((line: number) => void) | undefined
}): JSX.Element {
  const t = useViewerT()
  const stage = useRef<HTMLDivElement>(null)
  // The offset the last editor line scrolled to: the scroll event that follows is not the user's.
  const synced = useRef<number | undefined>(undefined)
  const userScrolled = useRef(0)
  const frame = useRef(0)
  const posted = useRef<number | undefined>(undefined)
  const latest = useRef({ openLocal, onScrollLine })
  latest.current = { openLocal, onScrollLine }
  const [shown, setShown] = useState(text)
  const [reloads, setReloads] = useState(0)

  useEffect(() => {
    if (text === shown) return
    const timer = window.setTimeout(() => { setShown(text) }, text.length > LIVE_LIMIT ? 1000 : 120)
    return () => { window.clearTimeout(timer) }
  }, [text, shown])

  // Front matter shows in its own box: as Markdown its `---` lines would become a rule and a heading. Blank lines take
  // its place, so every block keeps the source line the scroll sync goes by.
  const { front, rendered } = useMemo(() => {
    const split = splitFrontMatter(shown.replace(/\r\n?/gu, '\n'))
    return { front: split.front, rendered: split.front === '' ? shown : '\n'.repeat((split.front.match(/\n/gu) ?? []).length) + split.body }
  }, [shown])

  const options = useMemo<MarkdownOptions>(() => ({
    base,
    codeActions: false,
    sourceLines: true,
    revision: `${version ?? 'unsaved'}-${reloads}`,
    openLocal: (target) => { latest.current.openLocal(target) },
    file: {
      footnoteLabel: t('footnotes'),
      footnoteBackLabel: t('footnoteBack'),
      fence: (code, language, line) => isMermaid(language) ? <MermaidDiagram source={code} line={line} /> : undefined,
    },
  }), [base.workspaceId, base.rootId, base.directory, version, reloads, t])

  useEffect(() => sync?.subscribe((line) => {
    const element = stage.current
    if (element === null || Date.now() - userScrolled.current < USER_SCROLL_HOLD) return
    const top = topForLine(blocks(element), line)
    if (top === undefined) return
    element.scrollTop = Math.max(0, top - 8)
    synced.current = element.scrollTop
    posted.current = undefined
  }), [sync])

  useEffect(() => () => { cancelAnimationFrame(frame.current) }, [])

  const onScroll = (): void => {
    const element = stage.current
    if (element === null) return
    const own = synced.current !== undefined && Math.abs(element.scrollTop - synced.current) < 2
    synced.current = undefined
    if (own) return
    userScrolled.current = Date.now()
    if (latest.current.onScrollLine === undefined || frame.current !== 0) return
    frame.current = requestAnimationFrame(() => {
      frame.current = 0
      const current = stage.current
      const line = current === null ? undefined : lineForTop(blocks(current), current.scrollTop + 8)
      if (line === undefined || (posted.current !== undefined && Math.abs(line - posted.current) < 0.01)) return
      posted.current = line
      latest.current.onScrollLine?.(line)
    })
  }

  return (
    <div className={css.viewer} data-viewer="markdown">
      <div className={css.toolbar}>
        {text.length > LIVE_LIMIT && <span>{t('previewLarge')}</span>}
        <span className={css.grow} />
        <button type="button" className={css.toolButton} title={t('previewRefreshTitle')} onClick={() => { setReloads(value => value + 1) }}>{t('previewRefresh')}</button>
      </div>
      <div ref={stage} className={css.stage} onScroll={onScroll}>
        <div className={css.markdownStage}>
          {front !== '' && <pre className={css.frontMatter} data-line={1} title={t('richFrontMatterTitle')}>{frontMatterContent(front)}</pre>}
          <MarkdownDocument text={rendered} options={options} />
        </div>
      </div>
    </div>
  )
}
