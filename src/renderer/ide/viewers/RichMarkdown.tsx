/** Markdown edited in place on its rendered form, as in Typora, with Milkdown Crepe. The text goes to the same buffer as
 *  the source editor, so saving, unsaved markers, review and the agent see every change. */
import { useEffect, useRef, useState } from 'react'
import { ideRawUrl } from '../../../shared/ide-preview.ts'
import { resolveDocumentPath } from '../../chat/Markdown.tsx'
import { toast } from '../../ui/index.ts'
import { preserveSource } from './markdown-merge.ts'
import { useViewerT } from './messages.ts'
import css from './Viewers.module.css'
import './RichMarkdown.css'

/** Editor handle kept between renders; only the parts this view uses. */
interface Rich {
  getMarkdown(): string
  /** Hand the current text to the buffer now; the editor's own notice waits 200 ms and is dropped when it closes. */
  flush(): void
  replace(text: string): void
  destroy(): Promise<unknown>
}

/** How Markdown is written back: the common GitHub style, so an edit does not rewrite the rest of the file. */
const STRINGIFY = { bullet: '-', bulletOther: '*', emphasis: '*', strong: '*', fence: '`', fences: true, rule: '-', listItemIndent: 'one', incrementListMarker: true } as const

/**
 * Write table delimiter rows as `| --- |`, the usual style, instead of the minimal `| - |` an unaligned table gets;
 * lines inside fenced code are left alone.
 * @param markdown Serialized document.
 * @returns The same document with conventional delimiter rows.
 */
export function conventionalTables(markdown: string): string {
  let fence: string | undefined
  return markdown.split('\n').map((line) => {
    const opening = /^\s{0,3}(`{3,}|~{3,})/u.exec(line)?.[1]
    if (opening !== undefined) {
      if (fence === undefined) fence = opening
      else if (opening[0] === fence[0] && opening.length >= fence.length) fence = undefined
      return line
    }
    if (fence !== undefined || !/^\|(?:\s*:?-+:?\s*\|)+\s*$/u.test(line)) return line
    return line.replace(/(\|)\s*(:?)-+(:?)\s*(?=\|)/gu, (_match, pipe: string, left: string, right: string) => `${pipe} ${left}---${right} `)
  }).join('\n')
}

/**
 * @param props.text The buffer's text.
 * @param props.base Project, mounted directory and the file's folder, for relative images.
 * @param props.version The observed file version, so a reloaded image is fetched again.
 * @param props.onChange The user edited the document; receives the new Markdown.
 * @param props.onSave Ctrl+S inside the editor.
 * @returns The editor.
 */
export function RichMarkdown({ text, base, version, onChange, onSave }: {
  text: string
  base: { workspaceId: string; rootId?: string | undefined; directory: string }
  version: string | null
  onChange: (text: string) => void
  onSave: () => void
}): JSX.Element {
  const t = useViewerT()
  const root = useRef<HTMLDivElement>(null)
  const editor = useRef<Rich | undefined>(undefined)
  /** The text the editor holds, as last read from it or written into it. */
  const held = useRef(text)
  const latest = useRef({ onChange, onSave, base, version })
  latest.current = { onChange, onSave, base, version }
  const [failure, setFailure] = useState('')
  const [ready, setReady] = useState(false)

  useEffect(() => {
    const element = root.current
    if (element === null) return
    let cancelled = false
    let created: Rich | undefined
    void (async () => {
      const [{ Crepe }, { remarkStringifyOptionsCtx }, { replaceAll }, { imageInlineComponent, inlineImageConfig }, { remarkGFMPlugin }] = await Promise.all([
        import('@milkdown/crepe'), import('@milkdown/kit/core'), import('@milkdown/kit/utils'), import('@milkdown/kit/component/image-inline'),
        import('@milkdown/kit/preset/gfm'),
      ])
      await import('@milkdown/crepe/theme/common/style.css')
      if (cancelled) return
      const image = (url: string): string => {
        const { base: folder, version: revision } = latest.current
        if (/^(?:[a-z][a-z\d+.-]*:|\/\/|#)/iu.test(url)) return url
        const path = resolveDocumentPath(folder.directory, url)
        return path === undefined ? url : ideRawUrl({ workspaceId: folder.workspaceId, rootId: folder.rootId, path, version: revision ?? undefined })
      }
      const refuseUpload = async (): Promise<string> => {
        toast(t('richNoUpload'), { tone: 'error' })
        throw new Error('upload is not supported')
      }
      const crepe = new Crepe({
        root: element,
        defaultValue: held.current,
        // Crepe's image block keeps its display size in the alt text, which would replace the author's description; the
        // inline image below writes `![alt](src "title")` back unchanged.
        features: { [Crepe.Feature.ImageBlock]: false },
        featureConfigs: {
          [Crepe.Feature.Placeholder]: { text: t('richPlaceholder'), mode: 'block' },
          [Crepe.Feature.LinkTooltip]: { inputPlaceholder: t('richLinkPlaceholder') },
          [Crepe.Feature.CodeMirror]: { searchPlaceholder: t('richLanguageSearch'), noResultText: t('richNoLanguage'), copyText: t('richCopy'),
            previewLabel: t('richPreview'), previewToggleText: preview => preview ? t('richEdit') : t('richPreview') },
          [Crepe.Feature.BlockEdit]: {
            textGroup: { label: t('richGroupText'), text: { label: t('richText') }, h1: { label: t('richHeading', { level: 1 }) },
              h2: { label: t('richHeading', { level: 2 }) }, h3: { label: t('richHeading', { level: 3 }) }, h4: { label: t('richHeading', { level: 4 }) },
              h5: { label: t('richHeading', { level: 5 }) }, h6: { label: t('richHeading', { level: 6 }) }, quote: { label: t('richQuote') }, divider: { label: t('richDivider') } },
            listGroup: { label: t('richGroupList'), bulletList: { label: t('richBullet') }, orderedList: { label: t('richOrdered') }, taskList: { label: t('richTask') } },
            advancedGroup: { label: t('richGroupAdvanced'), image: { label: t('richImage') }, codeBlock: { label: t('richCode') }, table: { label: t('richTable') },
              math: { label: t('richMath') } },
          },
        },
      })
      crepe.editor.config((ctx) => {
        ctx.update(remarkStringifyOptionsCtx, previous => ({ ...previous, ...STRINGIFY }))
        // Aligned table columns would turn one edited cell into a change of every row.
        ctx.set(remarkGFMPlugin.options.key, { tablePipeAlign: false })
        ctx.update(inlineImageConfig.key, previous => ({ ...previous, proxyDomURL: image, onUpload: refuseUpload, uploadButton: undefined,
          uploadPlaceholderText: t('richImageLink'), confirmButton: t('richConfirm') }))
      }).use(imageInlineComponent)
      let quiet = false
      // The file as loaded and the editor's own serialization of it: edits are merged into the file line by line, so
      // escapes, blank lines and table spacing the editor would write differently survive outside the edited lines.
      let source = held.current
      let loaded = ''
      let started = false
      const push = (written: string): void => {
        if (quiet || !started) return
        const markdown = preserveSource(source, loaded, conventionalTables(written))
        if (markdown === held.current) return
        held.current = markdown
        latest.current.onChange(markdown)
      }
      crepe.on((listen) => {
        listen.markdownUpdated((_ctx, written) => { push(written) })
        // Clicking Save in the toolbar, another tab or the chat moves focus out first.
        listen.blur(() => { push(crepe.getMarkdown()) })
      })
      await crepe.create()
      if (cancelled) { await crepe.destroy(); return }
      loaded = conventionalTables(crepe.getMarkdown())
      started = true
      created = {
        getMarkdown: () => crepe.getMarkdown(),
        flush: () => { push(crepe.getMarkdown()) },
        replace: (next) => {
          quiet = true
          try { crepe.editor.action(replaceAll(next)) } finally { quiet = false }
          source = next
          loaded = conventionalTables(crepe.getMarkdown())
          held.current = next
        },
        destroy: () => crepe.destroy(),
      }
      editor.current = created
      setReady(true)
    })().catch((error: unknown) => { if (!cancelled) setFailure(error instanceof Error ? error.message : String(error)) })
    return () => {
      cancelled = true
      editor.current = undefined
      // Switching views or closing the tab within the editor's 200 ms wait would otherwise lose the last keystrokes.
      created?.flush()
      void created?.destroy()
    }
  // The editor is built once per tab; later text arrives through the effect below.
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [])

  // Text changed elsewhere (the source view, a reload, the agent): show it unless it is what the editor itself wrote.
  useEffect(() => {
    if (!ready || text === held.current) return
    editor.current?.replace(text)
  }, [text, ready])

  return <div className={css.viewer} data-viewer="markdown-rich">
    {failure !== '' ? <div className={`${css.message} ${css.error}`}>{t('richFailed', { error: failure })}</div> : <>
      {!ready && <div className={css.message}>{t('loading')}</div>}
      <div className={css.stage} onKeyDown={(event) => {
        if ((event.ctrlKey || event.metaKey) && !event.shiftKey && !event.altKey && event.key.toLowerCase() === 's') {
          event.preventDefault()
          editor.current?.flush()
          latest.current.onSave()
        }
      }}>
        <div ref={root} className="rainy-rich" hidden={!ready} />
      </div>
    </>}
  </div>
}
