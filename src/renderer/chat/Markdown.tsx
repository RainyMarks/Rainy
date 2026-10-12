/** Markdown of assistant replies and file previews: GFM, math, highlighted code blocks, local links and guarded images. */
import { Children, isValidElement, memo, useCallback, useMemo, useState, type ReactElement, type ReactNode } from 'react'
import ReactMarkdown, { type Components } from 'react-markdown'
import remarkGfm from 'remark-gfm'
import remarkMath from 'remark-math'
import rehypeKatex from 'rehype-katex'
import clsx from 'clsx'
import { ideRawUrl } from '../../shared/ide-preview.ts'
import { emit } from '../app/bus.ts'
import { CodeToolbar, highlightLines, Tooltip, writeClipboard } from '../ui/index.ts'
import cardCss from '../ui/CodeCard.module.css'
import { useChatT } from './messages.ts'
import css from './Markdown.module.css'

function textOf(node: ReactNode): string {
  if (typeof node === 'string' || typeof node === 'number') return String(node)
  if (Array.isArray(node)) return node.map(textOf).join('')
  if (isValidElement<{ children?: ReactNode }>(node)) return textOf(node.props.children)
  return ''
}

/** A fenced code block with copy, wrap, and the "open in editor" / "compare with current file" actions. */
export function CodeBlock({ code, language, actions = true }: { code: string; language: string | undefined; actions?: boolean }): JSX.Element {
  const t = useChatT()
  const [copied, setCopied] = useState(false)
  const [wrapped, setWrapped] = useState(false)
  const lines = useMemo(() => highlightLines(code, language), [code, language])
  const onCopy = useCallback(() => {
    void writeClipboard(code).then((ok) => {
      if (!ok) return
      setCopied(true)
      window.setTimeout(() => { setCopied(false) }, 1000)
    })
  }, [code])
  return (
    <div className={clsx(cardCss.card, css.code)} data-code-wrap={wrapped}>
      <CodeToolbar lang={language} labels={{ codeLabel: t('code'), wrapLabel: t('wrap'), unwrapLabel: t('unwrap') }}
        copyLabel={t('copy')} copiedLabel={t('copied')} copied={copied} wrapped={wrapped} onCopy={onCopy} onWrap={() => { setWrapped(value => !value) }}
        actions={actions ? <>
          <Tooltip label={t('openInEditor')} side="top" portal>
            <button type="button" className={css.action} onClick={() => { emit('editor.snippet', { code, language, compare: false }) }}>{t('openInEditor')}</button>
          </Tooltip>
          <Tooltip label={t('compareWithFile')} side="top" portal>
            <button type="button" className={css.action} onClick={() => { emit('editor.snippet', { code, language, compare: true }) }}>{t('compareWithFile')}</button>
          </Tooltip>
        </> : undefined} />
      <pre className={css.pre}><code>
        {lines === undefined ? code : lines.map((line, index) => (
          <span key={index} className={css.line}>
            {line.map((span, spanIndex) => <span key={spanIndex} className={span.className}>{span.text}</span>)}
            {index < lines.length - 1 ? '\n' : ''}
          </span>
        ))}
      </code></pre>
    </div>
  )
}

/**
 * Whether a link target is a local path the editor can open.
 * @param href Link target.
 * @returns The path and line, or `undefined` for web links.
 */
export function localTarget(href: string): { path: string; line?: number | undefined } | undefined {
  if (/^(https?|mailto):/i.test(href)) return undefined
  let path = href.replace(/^file:\/\//i, '')
  let line: number | undefined
  const anchor = /#L(\d+)(?:-L?\d+)?$/.exec(path) ?? /:(\d+)(?::\d+)?$/.exec(path)
  if (anchor !== null) { line = Number(anchor[1]); path = path.slice(0, anchor.index) }
  try { path = decodeURIComponent(path) } catch (_error) { /* A malformed escape keeps the literal path. */ }
  if (path === '' || path.startsWith('#')) return undefined
  return { path, ...(line === undefined ? {} : { line }) }
}

/**
 * Resolve a link or image target written in a document against the document's directory.
 * @param directory Project-relative directory of the document, `''` for the root.
 * @param target Target as written; a leading `/` means the project root.
 * @returns A project-relative path, the target itself when it is an absolute host path, or `undefined` when it leaves the project.
 */
export function resolveDocumentPath(directory: string, target: string): string | undefined {
  let path = target.replace(/[?#].*$/u, '')
  try { path = decodeURIComponent(path) } catch (_malformedEscape) { /* Keep the literal spelling. */ }
  path = path.replaceAll('\\', '/')
  if (/^[A-Za-z]:\//u.test(path) || path.startsWith('//')) return path
  const parts = path.startsWith('/') ? [] : directory.split('/').filter(part => part !== '')
  for (const part of path.split('/')) {
    if (part === '' || part === '.') continue
    if (part === '..') { if (parts.pop() === undefined) return undefined } else parts.push(part)
  }
  return parts.length === 0 ? undefined : parts.join('/')
}

/**
 * GitHub-style heading slug.
 * @param text Heading text.
 * @returns Lowercase words joined with hyphens; letters of every script are kept.
 */
export function slugify(text: string): string {
  return text.trim().toLowerCase().replace(/[^\p{L}\p{N}\s_-]/gu, '').replace(/\s/gu, '-')
}

/** The few fields of a hast node the plugin below reads and writes. */
interface HastNode {
  type: string
  tagName?: string
  value?: string
  properties?: Record<string, unknown>
  children?: HastNode[]
  position?: { start: { line: number } }
}

function hastText(node: HastNode): string {
  return node.type === 'text' ? node.value ?? '' : (node.children ?? []).map(hastText).join('')
}

/** Heading ids get this prefix so document text can never name a global of the page. */
const ANCHOR_PREFIX = 'md-'

/**
 * Rehype plugin: prefixed heading ids for in-document links, and the source line of every top-level block for scroll sync.
 * @param lines Whether to add `data-line`.
 * @returns The plugin.
 */
function rehypeAnchors(lines: boolean) {
  return () => (tree: HastNode): void => {
    const used = new Map<string, number>()
    const visit = (node: HastNode, top: boolean): void => {
      for (const child of node.children ?? []) {
        if (child.type !== 'element') continue
        child.properties ??= {}
        if (/^h[1-6]$/u.test(child.tagName ?? '')) {
          const base = slugify(hastText(child)) || 'section'
          const count = used.get(base) ?? 0
          used.set(base, count + 1)
          child.properties.id = `${ANCHOR_PREFIX}${count === 0 ? base : `${base}-${count}`}`
        }
        if (top && lines && child.position !== undefined) child.properties.dataLine = child.position.start.line
        visit(child, false)
      }
    }
    visit(tree, true)
  }
}

function scrollToAnchor(from: HTMLElement, fragment: string): void {
  let id = fragment
  try { id = decodeURIComponent(fragment) } catch (_malformedEscape) { /* Keep the literal fragment. */ }
  const root = from.closest('[data-markdown-root]')
  const target = root?.querySelector(`#${CSS.escape(ANCHOR_PREFIX + id.toLowerCase())}`) ?? root?.querySelector(`#${CSS.escape(ANCHOR_PREFIX + slugify(id))}`)
  target?.scrollIntoView({ block: 'start' })
}

/** A web image is never fetched: it could report the reader to a third party or carry data out of a prompt-injected reply. */
function RemoteImage({ src, alt }: { src: string; alt: string | undefined }): JSX.Element {
  const t = useChatT()
  let host = src
  try { host = new URL(src).host } catch (_invalidUrl) { /* Show the raw address. */ }
  return (
    <span className={css.remoteImage} title={src}>
      <span>{alt === undefined || alt === '' ? '' : `${alt} · `}{t('remoteImage', { host })}</span>
      <a href={src} target="_blank" rel="noreferrer">{t('remoteImageOpen')}</a>
    </span>
  )
}

function LocalImageLink({ path, alt, open }: { path: string; alt: string | undefined; open: (target: { path: string }) => void }): JSX.Element {
  const t = useChatT()
  return <a href={path} className={css.localImage} onClick={(event) => { event.preventDefault(); open({ path }) }}>{alt === undefined || alt === '' ? t('localImage', { path }) : alt}</a>
}

/** Where a document's relative targets resolve and how they open. */
export interface MarkdownOptions {
  /** Project and directory of the document; relative images show project files only when it is set. */
  readonly base?: { readonly workspaceId: string; readonly rootId?: string | undefined; readonly directory: string } | undefined
  /** Open a local file; the default asks the editor through the workbench bus. */
  readonly openLocal?: ((target: { path: string; line?: number | undefined }) => void) | undefined
  /** "Open in editor" and "compare" on code blocks. */
  readonly codeActions?: boolean | undefined
  /** `data-line` on top-level blocks, for scroll sync with the source. */
  readonly sourceLines?: boolean | undefined
  /** Changes when the document should reload the project images it shows. */
  readonly revision?: string | undefined
}

const defaultOpen = (target: { path: string; line?: number | undefined }): void => { emit('editor.open', target) }

function markdownComponents(options: MarkdownOptions): Components {
  const open = options.openLocal ?? defaultOpen
  const base = options.base
  const resolve = (path: string): string => base === undefined ? path : resolveDocumentPath(base.directory, path) ?? path
  return {
    pre({ children }) {
      const child = Children.toArray(children)[0]
      if (isValidElement<{ className?: string; children?: ReactNode }>(child)) {
        const language = /language-([\w+#.-]+)/.exec(child.props.className ?? '')?.[1]
        return <CodeBlock code={textOf(child.props.children).replace(/\n$/, '')} language={language} actions={options.codeActions !== false} />
      }
      return <pre>{children}</pre>
    },
    a({ href, children }) {
      if (href?.startsWith('#') === true) {
        return <a href={href} onClick={(event) => { event.preventDefault(); scrollToAnchor(event.currentTarget, href.slice(1)) }}>{children}</a>
      }
      const target = href === undefined ? undefined : localTarget(href)
      if (target !== undefined) {
        return <a href={href} onClick={(event) => { event.preventDefault(); open({ ...target, path: resolve(target.path) }) }}>{children}</a>
      }
      return <a href={href} target="_blank" rel="noreferrer">{children}</a>
    },
    img({ src, alt }) {
      if (typeof src !== 'string' || src === '') return <span>{alt}</span>
      if (/^[a-z][a-z\d+.-]*:/iu.test(src) && !/^[A-Za-z]:[\\/]/u.test(src)) return <RemoteImage src={src} alt={alt} />
      const path = base === undefined ? undefined : resolveDocumentPath(base.directory, src)
      if (base === undefined || path === undefined || /^[A-Za-z]:\//u.test(path) || path.startsWith('//')) return <LocalImageLink path={src} alt={alt} open={open} />
      return <img className={css.image} src={ideRawUrl({ workspaceId: base.workspaceId, rootId: base.rootId, path, version: options.revision })} alt={alt ?? ''} loading="lazy" />
    },
    table({ children }) {
      return <div className={css.tableScroll}><table>{children}</table></div>
    },
  }
}

const REMARK_PLUGINS = [remarkGfm, remarkMath]
const CHAT_OPTIONS: MarkdownOptions = { codeActions: true }

/** Markdown with explicit link, image and code-block behavior; memoized because previews re-render while typing. */
export const MarkdownDocument = memo(function MarkdownDocument({ text, className, options }: {
  text: string
  className?: string | undefined
  options: MarkdownOptions
}): ReactElement {
  const components = useMemo(() => markdownComponents(options), [options])
  const rehypePlugins = useMemo(() => [rehypeKatex, rehypeAnchors(options.sourceLines === true)], [options.sourceLines])
  return (
    <div className={clsx(css.markdown, className)} data-markdown-root>
      <ReactMarkdown remarkPlugins={REMARK_PLUGINS} rehypePlugins={rehypePlugins} components={components}>{text}</ReactMarkdown>
    </div>
  )
})

/** Rendered assistant markdown; memoized because transcripts re-render while streaming. */
export const Markdown = memo(function Markdown({ text, className }: { text: string; className?: string | undefined }): ReactElement {
  return <MarkdownDocument text={text} className={className} options={CHAT_OPTIONS} />
})
