/** Markdown preview of an editor buffer: follows unsaved edits and keeps its scroll position in step with the source. */
import { useEffect, useMemo, useRef, useState } from 'react'
import type { IdeRootId, WorkspaceId } from '../../../shared/ide-files-protocol.ts'
import { emit } from '../../app/bus.ts'
import { MarkdownDocument, type MarkdownOptions } from '../../chat/Markdown.tsx'
import { frontMatterContent, splitFrontMatter } from './front-matter.ts'
import { useViewerT } from './messages.ts'
import type { ScrollSync } from './scroll-sync.ts'
import css from './Viewers.module.css'

/** Above this size the preview waits for a pause in typing instead of following every keystroke. */
const LIVE_LIMIT = 256 * 1024

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

/**
 * @param props.text Current buffer text, including unsaved edits.
 * @param props.base Project, mounted directory and the document's directory, for relative links and images.
 * @param props.version Observed file version; images reload when it changes.
 * @param props.sync Editor scroll positions to follow, in split view.
 * @param props.onScrollLine The user scrolled the preview; the editor follows.
 * @returns The preview.
 */
export function MarkdownPreview({ text, base, version, sync, onScrollLine }: {
  text: string
  base: { workspaceId: WorkspaceId; rootId?: IdeRootId | undefined; directory: string }
  version: string | null
  sync?: ScrollSync | undefined
  onScrollLine?: ((line: number) => void) | undefined
}): JSX.Element {
  const t = useViewerT()
  const stage = useRef<HTMLDivElement>(null)
  const ignoreScroll = useRef(0)
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
    openLocal: (target) => { emit('editor.open', { path: target.path, workspaceId: base.workspaceId, rootId: base.rootId, line: target.line }) },
  }), [base.workspaceId, base.rootId, base.directory, version, reloads])

  useEffect(() => sync?.subscribe((line) => {
    const element = stage.current
    if (element === null) return
    const top = topForLine(blocks(element), line)
    if (top === undefined) return
    ignoreScroll.current = Date.now()
    element.scrollTop = Math.max(0, top - 8)
  }), [sync])

  const onScroll = (): void => {
    const element = stage.current
    if (element === null || onScrollLine === undefined || Date.now() - ignoreScroll.current < 120) return
    const line = lineForTop(blocks(element), element.scrollTop + 8)
    if (line !== undefined) onScrollLine(line)
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
