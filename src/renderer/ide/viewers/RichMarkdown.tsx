/** Markdown edited in place on its rendered form, as in Typora, with Milkdown Crepe. The text goes to the same buffer as
 *  the source editor, so saving, unsaved markers, review and the agent see every change. Front matter gets its own box,
 *  pasted and dropped images are saved next to the document, and Typora's shortcuts, find and replace, outline, focus and
 *  typewriter modes, Mermaid diagrams and Ctrl+click on links work as they do there. */
import { useEffect, useRef, useState } from 'react'
import type { EditorView } from '@milkdown/kit/prose/view'
import type { Node as ProseNode } from '@milkdown/kit/prose/model'
import { ideRawUrl } from '../../../shared/ide-preview.ts'
import { on } from '../../app/bus.ts'
import { resolveDocumentPath } from '../../chat/Markdown.tsx'
import { usePrefs } from '../../prefs.ts'
import { toast } from '../../ui/index.ts'
import { outlinePosition, provideOutline } from '../outline-store.ts'
import { frontMatterContent, frontMatterIsToml, splitFrontMatter, withFrontMatterContent } from './front-matter.ts'
import { conventionalTables, mergeEdit } from './markdown-merge.ts'
import { headingForFragment, markdownOutline } from './markdown-outline.ts'
import { isMermaid, onThemeChange, renderMermaid } from './mermaid.ts'
import { useViewerT } from './messages.ts'
import { RichFindBar, type FindRequest } from './RichFindBar.tsx'
import type * as RichPlugins from './rich-plugins.ts'
import css from './Viewers.module.css'
import './RichMarkdown.css'

/** Editor handle kept between renders; only the parts this view uses. */
interface Rich {
  readonly view: EditorView
  getMarkdown(): string
  /** Hand the current text to the buffer now; the editor's own notice waits 200 ms and is dropped when it closes. */
  flush(): void
  replace(text: string): void
  destroy: () => Promise<unknown>
}

/** @param text Markdown with any line ends. @returns It with LF line ends, as the editor holds it. */
function lf(text: string): string {
  return text.replace(/\r\n?/gu, '\n')
}

/** @param view Editor. @returns The document position of every heading, in document order. */
function headingPositions(view: EditorView): number[] {
  const positions: number[] = []
  view.state.doc.descendants((node: ProseNode, position: number) => {
    if (node.type.name === 'heading') positions.push(position)
    return !node.isTextblock
  })
  return positions
}

type ViewerKey = Parameters<ReturnType<typeof useViewerT>>[0]

/** Crepe's icon-only controls, which come without a tooltip, and what each does. */
const CONTROL_TIPS: readonly (readonly [string, ViewerKey])[] = [
  ['.milkdown-link-preview .link-icon', 'richCopyLink'], ['.milkdown-link-preview .link-edit-button', 'richEditLink'],
  ['.milkdown-link-preview .link-remove-button', 'richRemoveLink'],
  ['.milkdown-block-handle > .operation-item:first-child', 'richAddBlock'], ['.milkdown-block-handle > .operation-item:last-child', 'richDragBlock'],
  ['[data-role="col-drag-handle"] > .milkdown-icon', 'richSelectColumn'], ['[data-role="row-drag-handle"] > .milkdown-icon', 'richSelectRow'],
  ['[data-role="col-drag-handle"] .button-group > button:nth-child(1)', 'richAlignLeft'],
  ['[data-role="col-drag-handle"] .button-group > button:nth-child(2)', 'richAlignCenter'],
  ['[data-role="col-drag-handle"] .button-group > button:nth-child(3)', 'richAlignRight'],
  ['[data-role="col-drag-handle"] .button-group > button:nth-child(4)', 'richDeleteColumn'],
  ['[data-role="row-drag-handle"] .button-group > button', 'richDeleteRow'],
  ['[data-role="x-line-drag-handle"] .add-button', 'richAddRow'], ['[data-role="y-line-drag-handle"] .add-button', 'richAddColumn'],
  ['.milkdown-link-edit .button.confirm', 'richConfirm'], ['.milkdown-latex-inline-edit button', 'richConfirm'],
]

/** Above this size the rich view stays off: Crepe renders and serializes the whole document on every edit. */
const RICH_LIMIT = 2 * 1024 * 1024

/**
 * Crepe's language button reads "Text" for a code block without a language and offers no setting for it.
 * @param root Editor element.
 * @param label What to show instead.
 */
function labelPlainCode(root: Element, label: string): void {
  for (const button of root.querySelectorAll('.milkdown-code-block .language-button')) {
    const text = button.firstChild
    if (text?.nodeType === Node.TEXT_NODE && text.nodeValue === 'Text') text.nodeValue = label
  }
}

/** Give the control under the pointer its tooltip and accessible name, the first time it is pointed at. */
function labelControl(target: EventTarget | null, t: ReturnType<typeof useViewerT>): void {
  if (!(target instanceof Element)) return
  for (const [selector, key] of CONTROL_TIPS) {
    const control = target.closest(selector)
    if (control === null) continue
    if (!control.hasAttribute('title')) {
      control.setAttribute('title', t(key))
      control.setAttribute('aria-label', t(key))
    }
    return
  }
}

type RichProps = Parameters<typeof RichEditor>[0]

/**
 * The rich view of a document, or a notice to use the source view when the document is too large for it.
 * @param props See {@link RichEditor}.
 * @returns The view.
 */
export function RichMarkdown(props: RichProps): JSX.Element {
  const t = useViewerT()
  // Decided when the view opens: a document that grows past the limit while open keeps its editor.
  const [large] = useState(() => props.text.length > RICH_LIMIT)
  if (!large) return <RichEditor {...props} />
  return <div className={css.viewer} data-viewer="markdown-rich">
    <div className={css.message}>{t('richTooLarge', { size: `${(props.text.length / 1024 / 1024).toFixed(1)} MB` })}</div>
  </div>
}

/**
 * @param props.path File key, for the outline.
 * @param props.text The buffer's text.
 * @param props.base Project, mounted directory and the file's folder, for relative and absolute images.
 * @param props.version The observed file version, so a reloaded image is fetched again.
 * @param props.onChange The user edited the document; receives the new Markdown.
 * @param props.onSave Ctrl+S inside the editor.
 * @param props.onImage Saves a pasted or dropped image beside the document; returns the path to write in the Markdown.
 * @param props.onOpenLink Ctrl+click on a link that leaves this document.
 * @returns The editor.
 */
function RichEditor({ path, text, base, version, onChange, onSave, onImage, onOpenLink }: {
  path: string
  text: string
  /** `hostRoot`: the project folder on this computer, when it is a Windows folder, for images written as `C:\…` paths. */
  base: { workspaceId: string; rootId?: string | undefined; directory: string; hostRoot?: string | undefined }
  version: string | null
  onChange: (text: string) => void
  onSave: () => void
  onImage: (file: File) => Promise<string>
  onOpenLink: (href: string) => void
}): JSX.Element {
  const t = useViewerT()
  const prefs = usePrefs()
  const root = useRef<HTMLDivElement>(null)
  const stage = useRef<HTMLDivElement>(null)
  const editor = useRef<Rich | undefined>(undefined)
  const plugins = useRef<typeof RichPlugins | undefined>(undefined)
  /** The text the editor holds, as last read from it or written into it, front matter included. */
  const held = useRef(text)
  const front = useRef(splitFrontMatter(text).front)
  const body = useRef(splitFrontMatter(text).body)
  const [frontMatter, setFrontMatter] = useState(front.current)
  const latest = useRef({ onChange, onSave, onImage, onOpenLink, base, version, t })
  latest.current = { onChange, onSave, onImage, onOpenLink, base, version, t }
  const [failure, setFailure] = useState('')
  const [ready, setReady] = useState(false)
  const [find, setFind] = useState<FindRequest | undefined>()
  const [revision, setRevision] = useState(0)

  /** Hand the document to the buffer unless it is what the buffer already has. */
  const publish = (nextFront: string, nextBody: string): void => {
    front.current = nextFront
    body.current = nextBody
    const full = nextFront + nextBody
    if (full === held.current) return
    held.current = full
    latest.current.onChange(full)
  }

  /** Scroll so heading `index` (document order) is at the top of the view. */
  const reveal = (index: number): void => {
    const view = editor.current?.view
    const scroller = stage.current
    if (view === undefined || scroller === null) return
    const position = headingPositions(view)[index]
    if (position === undefined) return
    const element = view.nodeDOM(position)
    if (!(element instanceof HTMLElement)) return
    scroller.scrollTop += element.getBoundingClientRect().top - scroller.getBoundingClientRect().top - 16
    element.classList.remove('rainy-heading-flash')
    void element.offsetWidth
    element.classList.add('rainy-heading-flash')
  }
  const latestReveal = useRef(reveal)
  latestReveal.current = reveal

  useEffect(() => {
    const element = root.current
    if (element === null) return
    let cancelled = false
    let created: Rich | undefined
    void (async () => {
      const [{ Crepe }, { commandsCtx, editorViewCtx }, { clearTextInCurrentBlockCommand }, { replaceAll }, { imageInlineComponent, inlineImageConfig }, { uploadConfig }, { codeBlockConfig }, rich,
        { codeTheme }] = await Promise.all([
        import('@milkdown/crepe'), import('@milkdown/kit/core'), import('@milkdown/kit/preset/commonmark'), import('@milkdown/kit/utils'), import('@milkdown/kit/component/image-inline'),
        import('@milkdown/kit/plugin/upload'), import('@milkdown/kit/component/code-block'), import('./rich-plugins.ts'), import('./code-theme.ts'),
      ])
      await import('@milkdown/crepe/theme/common/style.css')
      if (cancelled) return
      plugins.current = rich
      const image = (url: string): string => {
        const { base: folder, version: revision } = latest.current
        // `C:\…` and `C:/…` are paths, not URL schemes: shown when they lie in the project, as the Host serves only that.
        const drive = /^[A-Za-z]:[\\/]/u.test(url)
        if (drive) {
          const root = folder.hostRoot?.replaceAll('\\', '/').replace(/\/+$/u, '')
          const target = url.replaceAll('\\', '/')
          const inside = root !== undefined && target.toLowerCase().startsWith(`${root.toLowerCase()}/`)
          if (!inside) return url
          url = `/${target.slice(root.length + 1)}`
        } else if (/^(?:[a-z][a-z\d+.-]*:|\/\/|#)/iu.test(url)) return url
        const resolved = resolveDocumentPath(folder.directory, url)
        return resolved === undefined ? url : ideRawUrl({ workspaceId: folder.workspaceId, rootId: folder.rootId, path: resolved, version: revision ?? undefined })
      }
      const save = async (file: File): Promise<string> => {
        try { return await latest.current.onImage(file) } catch (error) {
          toast(latest.current.t('richImageFailed', { error: error instanceof Error ? error.message : String(error) }), { tone: 'error' })
          throw error
        }
      }
      const failed = (message: string): HTMLElement => {
        const box = document.createElement('div')
        box.className = 'rainy-preview-error'
        box.textContent = message
        return box
      }
      const draw = (content: string, apply: (value: string | HTMLElement) => void): void => {
        renderMermaid(content).then(apply, (error: unknown) => {
          apply(failed(latest.current.t('mermaidFailed', { error: error instanceof Error ? error.message : String(error) })))
        })
      }
      /** The diagrams drawn last, by the callback that shows each; drawn again in the new colors when the theme changes. */
      const diagrams = new Map<(value: string | HTMLElement) => void, string>()
      const unwatch = onThemeChange(() => { for (const [apply, content] of diagrams) draw(content, apply) })
      const crepe = new Crepe({
        root: element,
        defaultValue: lf(body.current),
        // Crepe's image block keeps its display size in the alt text, which would replace the author's description; the
        // inline image below writes `![alt](src "title")` back unchanged.
        features: { [Crepe.Feature.ImageBlock]: false },
        featureConfigs: {
          [Crepe.Feature.Placeholder]: { text: t('richPlaceholder'), mode: 'block' },
          [Crepe.Feature.LinkTooltip]: { inputPlaceholder: t('richLinkPlaceholder') },
          // The selection toolbar's tooltips; Crepe adds the shortcut.
          [Crepe.Feature.Toolbar]: { boldLabel: t('richBold'), italicLabel: t('richItalic'), strikethroughLabel: t('richStrike'),
            codeLabel: t('richInlineCode'), latexLabel: t('richInlineMath'), linkLabel: t('richLink') },
          [Crepe.Feature.Latex]: { inlineEditConfirm: t('richConfirm') },
          // Rainy's highlight colors in place of Crepe's One Dark, which made code unreadable in the light mode.
          [Crepe.Feature.CodeMirror]: { theme: codeTheme(t), searchPlaceholder: t('richLanguageSearch'), noResultText: t('richNoLanguage'), copyText: t('richCopy'),
            previewLabel: t('richPreview'), previewToggleText: preview => preview ? t('richEdit') : t('richPreview'), previewLoading: t('mermaidLoading'),
            // Mermaid fences show the diagram; math is drawn by Crepe's LaTeX feature, which wraps this one.
            renderPreview: (language, content, apply) => {
              if (!isMermaid(language) || content.trim() === '') return null
              diagrams.delete(apply)
              diagrams.set(apply, content)
              if (diagrams.size > 200) diagrams.delete(diagrams.keys().next().value!)
              draw(content, apply)
              return undefined
            } },
          [Crepe.Feature.BlockEdit]: {
            // The handle sits close to its block, in the editor's left gutter (RichMarkdown.css), not outside the window.
            blockHandle: { getOffset: () => 4 },
            // The / menu takes the larger of the room above and below the caret and scrolls within it, instead of running
            // off a short window; the stylesheet caps its list with this room.
            slashMenu: { middleware: [{ name: 'rainyFitHeight', fn: ({ elements }) => {
              const caret = elements.reference.getBoundingClientRect()
              const room = `${Math.max(140, Math.floor(Math.max(window.innerHeight - caret.bottom, caret.top) - 24))}px`
              if (elements.floating.style.getPropertyValue('--rainy-menu-room') === room) return {}
              elements.floating.style.setProperty('--rainy-menu-room', room)
              return { reset: { rects: true } }
            } }] },
            textGroup: { label: t('richGroupText'), text: { label: t('richText') }, h1: { label: t('richHeading', { level: 1 }) },
              h2: { label: t('richHeading', { level: 2 }) }, h3: { label: t('richHeading', { level: 3 }) }, h4: { label: t('richHeading', { level: 4 }) },
              h5: { label: t('richHeading', { level: 5 }) }, h6: { label: t('richHeading', { level: 6 }) }, quote: { label: t('richQuote') }, divider: { label: t('richDivider') } },
            listGroup: { label: t('richGroupList'), bulletList: { label: t('richBullet') }, orderedList: { label: t('richOrdered') }, taskList: { label: t('richTask') } },
            advancedGroup: { label: t('richGroupAdvanced'), image: { label: t('richImage') }, codeBlock: { label: t('richCode') }, table: { label: t('richTable') },
              math: { label: t('richMath') } },
            // Crepe offers an image only with its image block, which is off; this one inserts an inline image.
            buildMenu: (builder) => {
              builder.getGroup('advanced').addItem('rainy-image', { label: t('richImage'), icon: rich.IMAGE_ICON, onRun: (ctx) => {
                ctx.get(commandsCtx).call(clearTextInCurrentBlockCommand.key)
                rich.insertImage(ctx)
              } })
            },
          },
        },
      })
      const syntax = await rich.markdownSyntax(crepe.editor, lf(body.current))
      syntax.config((ctx) => {
        ctx.update(inlineImageConfig.key, previous => ({ ...previous, proxyDomURL: image, onUpload: save, uploadButton: t('richUpload'),
          uploadPlaceholderText: t('richImageLink'), confirmButton: t('richConfirm'), imageIcon: t('richImage') }))
        // Only `$$` math (the language `LaTeX`) is drawn as a formula; a ```latex fence is code, often a whole document.
        ctx.update(codeBlockConfig.key, previous => ({ ...previous, renderPreview: (language, content, apply) =>
          language !== rich.MATH_LANGUAGE && /^latex$/iu.test(language) ? null : previous.renderPreview(language, content, apply) }))
        // Pasted and dropped images are saved beside the document, as Typora does, instead of becoming blob: links.
        ctx.update(uploadConfig.key, previous => ({ ...previous, uploader: async (files, schema) => {
          const nodes: ProseNode[] = []
          for (const file of Array.from(files)) {
            if (!file.type.startsWith('image/')) continue
            // A screenshot from the clipboard is called image.png; a dropped file keeps its name as the description.
            const name = /^image\.\w+$/u.test(file.name) ? '' : file.name.replace(/\.[^.]+$/u, '')
            let src: string
            try { src = await save(file) } catch (_reported) { continue } // save() already told the user why.
            const node = schema.nodes.image?.createAndFill({ src, alt: name })
            if (node !== null && node !== undefined) nodes.push(node)
          }
          return nodes
        } }))
      }).use(imageInlineComponent).use(rich.typoraKeymap).use(rich.tableKeys).use(rich.searchPlugin).use(rich.currentBlockPlugin).use(rich.typewriterPlugin)
        .use(rich.linkClickPlugin((href) => {
          if (!href.startsWith('#')) { latest.current.onOpenLink(href); return }
          const index = headingForFragment(markdownOutline(held.current), href.slice(1))
          if (index >= 0) latestReveal.current(index)
        }))
      let quiet = false
      // The file as loaded and the editor's own serialization of it: edits are merged into the file line by line, so
      // escapes, blank lines and table spacing the editor would write differently survive outside the edited lines.
      // `source` keeps the file's line ends; the editor and `loaded` have LF.
      let source = body.current
      let loaded = ''
      let started = false
      const push = (written: string): void => {
        if (quiet || !started) return
        publish(front.current, mergeEdit(source, loaded, conventionalTables(written), rich.markdownReader(crepe.editor.ctx)))
      }
      crepe.on((listen) => {
        listen.markdownUpdated((_ctx, written) => { push(written); setRevision(value => value + 1) })
        // Clicking Save in the toolbar, another tab or the chat moves focus out first.
        listen.blur(() => { push(crepe.getMarkdown()) })
      })
      await crepe.create()
      if (cancelled) { unwatch(); await crepe.destroy(); return }
      loaded = conventionalTables(crepe.getMarkdown())
      started = true
      created = {
        view: crepe.editor.ctx.get(editorViewCtx),
        getMarkdown: () => crepe.getMarkdown(),
        flush: () => { push(crepe.getMarkdown()) },
        replace: (next) => {
          quiet = true
          // A new editor state: the outside change is not an undo step, so Ctrl+Z cannot bring the old text back.
          const scroll = stage.current?.scrollTop
          try { crepe.editor.action(replaceAll(lf(next), true)) } finally { quiet = false }
          if (stage.current !== null && scroll !== undefined) stage.current.scrollTop = scroll
          source = next
          loaded = conventionalTables(crepe.getMarkdown())
          body.current = next
          setRevision(value => value + 1)
        },
        destroy: () => crepe.destroy(),
      }
      editor.current = created
      labelPlainCode(element, latest.current.t('richPlainText'))
      const observer = new MutationObserver(() => { labelPlainCode(element, latest.current.t('richPlainText')) })
      observer.observe(element, { subtree: true, childList: true, characterData: true })
      const destroy = created.destroy
      created.destroy = () => { observer.disconnect(); unwatch(); return destroy() }
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
    const next = splitFrontMatter(text)
    if (next.front !== front.current) { front.current = next.front; setFrontMatter(next.front) }
    if (next.body !== body.current) editor.current?.replace(next.body)
    held.current = text
  }, [text, ready])

  /** @returns The selected text when it fits on one line, to search for. */
  const selectedText = (): string => {
    const view = editor.current?.view
    if (view === undefined) return ''
    const { from, to } = view.state.selection
    const selected = view.state.doc.textBetween(from, to, ' ')
    return selected.includes('\n') || selected.length > 200 ? '' : selected
  }

  /** Publish the heading at the top of the view to the outline, once per frame. */
  const track = useRef<() => void>(() => undefined)
  // The outline beside the editor scrolls this view, and follows the heading at its top.
  useEffect(() => {
    if (!ready) return
    const withdraw = provideOutline(path, { reveal: (index) => { latestReveal.current(index) } })
    const scroller = stage.current
    let frame = 0
    track.current = () => {
      cancelAnimationFrame(frame)
      frame = requestAnimationFrame(() => {
        const element = root.current
        if (scroller === null || element === null) return
        const top = scroller.getBoundingClientRect().top + 24
        let index = -1
        for (const [position, heading] of [...element.querySelectorAll('.ProseMirror h1, .ProseMirror h2, .ProseMirror h3, .ProseMirror h4, .ProseMirror h5, .ProseMirror h6')].entries()) {
          if (heading.getBoundingClientRect().top <= top) index = position
          else break
        }
        const current = outlinePosition.getSnapshot()
        if (current.path !== path || current.index !== index) outlinePosition.set({ path, index })
      })
    }
    const scrolled = (): void => { track.current() }
    track.current()
    scroller?.addEventListener('scroll', scrolled, { passive: true })
    return () => { cancelAnimationFrame(frame); track.current = () => undefined; scroller?.removeEventListener('scroll', scrolled); withdraw() }
  }, [ready, path])
  // Added or removed headings move the one at the top.
  useEffect(() => { track.current() }, [revision])

  // Links show a hand while Ctrl is held, the way Ctrl+click opens them.
  useEffect(() => {
    const held = (event: KeyboardEvent): void => { stage.current?.toggleAttribute('data-ctrl-held', event.ctrlKey || event.metaKey) }
    const released = (): void => { stage.current?.removeAttribute('data-ctrl-held') }
    window.addEventListener('keydown', held)
    window.addEventListener('keyup', held)
    window.addEventListener('blur', released)
    return () => { window.removeEventListener('keydown', held); window.removeEventListener('keyup', held); window.removeEventListener('blur', released) }
  }, [])

  // Find and Replace from the Edit menu and the command palette.
  useEffect(() => on('markdown.find', (event) => {
    if (event.path === path) setFind({ replace: event.replace, seed: selectedText(), at: Date.now() })
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }), [path])

  return <div className={css.viewer} data-viewer="markdown-rich">
    {failure !== '' ? <div className={`${css.message} ${css.error}`}>{t('richFailed', { error: failure })}</div> : <>
      {!ready && <div className={css.message}>{t('loading')}</div>}
      {find !== undefined && ready && plugins.current !== undefined && <RichFindBar view={() => editor.current?.view} stage={() => stage.current}
        plugins={plugins.current} request={find} revision={revision}
        onClose={() => { setFind(undefined); editor.current?.view.focus() }} />}
      <div ref={stage} className={css.stage} data-rich-stage data-typewriter={prefs.markdownTypewriter ? 'true' : undefined}
        data-focus-mode={prefs.markdownFocus ? 'true' : undefined}
        onPointerOver={(event) => { labelControl(event.target, latest.current.t) }}
        onClickCapture={(event) => {
          // A diagram or formula shows its source when clicked, as in Typora.
          const preview = event.target instanceof Element ? event.target.closest('.milkdown-code-block .preview-panel') : null
          const content = preview?.closest('.milkdown-code-block')?.querySelector<HTMLElement>('.cm-content')
          if (content !== null && content !== undefined) { event.preventDefault(); content.focus() }
        }}
        onKeyDown={(event) => {
          const command = (event.ctrlKey || event.metaKey) && !event.altKey ? event.key.toLowerCase() : ''
          if (command === 's' && !event.shiftKey) {
            event.preventDefault()
            editor.current?.flush()
            latest.current.onSave()
          } else if ((command === 'f' || command === 'h') && !event.shiftKey) {
            event.preventDefault()
            setFind({ replace: command === 'h', seed: selectedText(), at: Date.now() })
          } else if (event.key === 'Escape' && find !== undefined) {
            setFind(undefined)
          }
        }}>
        <div className="rainy-rich" hidden={!ready}>
          {frontMatter !== '' && <div className="rainy-front-matter" data-front-matter>
            <div className="rainy-front-matter-label" title={t('richFrontMatterTitle')}>{t(frontMatterIsToml(frontMatter) ? 'richFrontMatterToml' : 'richFrontMatter')}</div>
            <textarea className="rainy-front-matter-text" spellCheck={false} aria-label={t(frontMatterIsToml(frontMatter) ? 'richFrontMatterToml' : 'richFrontMatter')}
              value={frontMatterContent(frontMatter)}
              onChange={(event) => {
                const next = withFrontMatterContent(front.current, event.target.value)
                setFrontMatter(next)
                publish(next, body.current)
              }} />
          </div>}
          <div ref={root} />
        </div>
      </div>
    </>}
  </div>
}
