/** Markdown of assistant replies and file previews: GFM, math, highlighted code blocks, local links and guarded images;
 *  file previews also show the allowlisted HTML of {@link rehypeSafeHtml}. */
import { Children, isValidElement, memo, useCallback, useMemo, useState, type ReactElement, type ReactNode } from 'react'
import ReactMarkdown, { defaultUrlTransform, type Components } from 'react-markdown'
import remarkGfm from 'remark-gfm'
import remarkMath from 'remark-math'
import clsx from 'clsx'
import { ideRawUrl } from '../../shared/ide-preview.ts'
import { emit } from '../app/bus.ts'
import { CodeToolbar, highlightLines, Tooltip, useHighlightRevision, writeClipboard } from '../ui/index.ts'
import cardCss from '../ui/CodeCard.module.css'
import { ANCHOR_PREFIX, DOCUMENT_REMARK_PLUGINS, isDataImage, rehypeSafeHtml } from '../ide/viewers/markdown-html.ts'
import { remarkDollarAmounts } from '../ide/viewers/markdown-math.ts'
import { mayContainMath, useMathPlugin } from './math.ts'
import { useChatT } from './messages.ts'
import css from './Markdown.module.css'

function textOf(node: ReactNode): string {
  if (typeof node === 'string' || typeof node === 'number') return String(node)
  if (Array.isArray(node)) return node.map(textOf).join('')
  if (isValidElement<{ children?: ReactNode }>(node)) return textOf(node.props.children)
  return ''
}

/** A fenced code block with copy, wrap, and the "open in editor" / "compare with current file" actions. */
export function CodeBlock({ code, language, actions = true, line }: {
  code: string
  language: string | undefined
  actions?: boolean
  /** Source line, for scroll sync. */
  line?: number | undefined
}): JSX.Element {
  const t = useChatT()
  const [copied, setCopied] = useState(false)
  const [wrapped, setWrapped] = useState(false)
  const grammars = useHighlightRevision()
  const lines = useMemo(() => highlightLines(code, language), [code, language, grammars])
  const onCopy = useCallback(() => {
    void writeClipboard(code).then((ok) => {
      if (!ok) return
      setCopied(true)
      window.setTimeout(() => { setCopied(false) }, 1000)
    })
  }, [code])
  return (
    <div className={clsx(cardCss.card, css.code)} data-code-wrap={wrapped} data-line={line}>
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
  // `file:///C:/x` names the Windows path `C:/x`.
  let path = href.replace(/^file:\/\//i, '').replace(/^\/([A-Za-z]:[\\/])/u, '$1')
  let line: number | undefined
  const anchor = /#L(\d+)(?:-L?\d+)?$/.exec(path) ?? /:(\d+)(?::\d+)?$/.exec(path)
  if (anchor !== null) { line = Number(anchor[1]); path = path.slice(0, anchor.index) }
  try { path = decodeURIComponent(path) } catch (_error) { /* A malformed escape keeps the literal path. */ }
  if (path === '' || path.startsWith('#')) return undefined
  return { path, ...(line === undefined ? {} : { line }) }
}

/**
 * Keep the local targets the page opens itself, which the default filter reads as unknown protocols: Windows drive paths,
 * `file:` URLs and `name:line` references. Everything else goes through the default filter, which blanks scripts.
 * @param url Link or image target as written.
 * @returns The target to render.
 */
export function markdownUrl(url: string): string {
  if (/^[A-Za-z]:[\\/]/u.test(url) || /^file:\/\//iu.test(url) || /^[^:/?#\s]+:\d+(?::\d+)?$/u.test(url)) return url
  return defaultUrlTransform(url)
}

/**
 * {@link markdownUrl} for project files, which may also embed images as data URLs: they load nothing from anywhere.
 * @param url Link or image target as written.
 * @param key Attribute that holds it.
 * @param node Element that holds it.
 * @returns The target to render.
 */
export function documentUrl(url: string, key: string, node: { tagName: string }): string {
  if (key === 'src' && node.tagName === 'img' && isDataImage(url)) return url
  return markdownUrl(url)
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

/**
 * Rehype plugin: prefixed heading ids for in-document links, and the source line of every top-level block for scroll sync.
 * A heading that already has an id keeps it: an HTML heading's own (prefixed) id, the footnote heading's.
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
        if (/^h[1-6]$/u.test(child.tagName ?? '') && (typeof child.properties.id !== 'string' || child.properties.id === '')) {
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
  if (root === null) return
  // Headings and HTML anchors carry the prefix; footnotes and their references keep remark's `user-content-` ids.
  const target = [ANCHOR_PREFIX + id, ANCHOR_PREFIX + id.toLowerCase(), ANCHOR_PREFIX + slugify(id), id]
    .map(candidate => root.querySelector(`#${CSS.escape(candidate)}`)).find(element => element !== null)
  target?.scrollIntoView({ block: 'start' })
}

/** @returns The source line rehypeAnchors gave a top-level block. */
function sourceLine(node: { properties?: Record<string, unknown> } | undefined): number | undefined {
  const line = node?.properties?.dataLine
  return typeof line === 'number' ? line : undefined
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
  /** Set for a file of the project rather than a reply. */
  readonly file?: MarkdownFileOptions | undefined
}

/** How a project file renders: its allowlisted HTML, `==highlights==`, single tildes as text and the fences it draws. */
export interface MarkdownFileOptions {
  /** Heading of the footnotes. */
  readonly footnoteLabel: string
  /** Label of the links from a footnote back to the text. */
  readonly footnoteBackLabel: string
  /** Draw a fenced block, such as a Mermaid diagram; `undefined` keeps the code block. */
  readonly fence?: ((code: string, language: string | undefined, line: number | undefined) => ReactNode) | undefined
}

const defaultOpen = (target: { path: string; line?: number | undefined }): void => { emit('editor.open', target) }

function markdownComponents(options: MarkdownOptions): Components {
  const open = options.openLocal ?? defaultOpen
  const base = options.base
  return {
    pre({ children, node }) {
      const line = sourceLine(node)
      const child = Children.toArray(children)[0]
      if (isValidElement<{ className?: string; children?: ReactNode }>(child)) {
        const language = /language-([\w+#.-]+)/.exec(child.props.className ?? '')?.[1]
        const code = textOf(child.props.children).replace(/\n$/, '')
        const drawn = options.file?.fence?.(code, language, line)
        if (drawn !== undefined) return drawn
        return <CodeBlock code={code} language={language} actions={options.codeActions !== false} line={line} />
      }
      return <pre data-line={line}>{children}</pre>
    },
    // Ids, titles and the footnote attributes pass through; the HTML allowlist and remark are their only sources.
    a({ href, children, node: _node, ...rest }) {
      // A target the filter blanked would open the workbench address in the browser.
      if (href === undefined || href === '') return <span id={rest.id} title={rest.title}>{children}</span>
      if (href.startsWith('#')) {
        return <a {...rest} href={href} onClick={(event) => { event.preventDefault(); scrollToAnchor(event.currentTarget, href.slice(1)) }}>{children}</a>
      }
      const target = localTarget(href)
      if (target !== undefined) {
        return <a {...rest} href={href} onClick={(event) => {
          event.preventDefault()
          // A document's link that leaves its project opens nothing, as in the rich editor.
          const path = base === undefined ? target.path : resolveDocumentPath(base.directory, target.path)
          if (path !== undefined) open({ ...target, path })
        }}>{children}</a>
      }
      return <a {...rest} href={href} target="_blank" rel="noreferrer">{children}</a>
    },
    // An HTML image keeps its sanitized size and style: Typora writes `<img src="a.png" style="zoom:50%">`.
    img({ src, alt, title, width, height, style, node }) {
      if (typeof src !== 'string' || src === '') return <span>{alt}</span>
      const shown = { title, width, height, style, 'data-line': sourceLine(node) }
      if (isDataImage(src)) return <img className={css.image} src={src} alt={alt ?? ''} {...shown} />
      if (/^[a-z][a-z\d+.-]*:/iu.test(src) && !/^[A-Za-z]:[\\/]/u.test(src)) return <RemoteImage src={src} alt={alt} />
      const path = base === undefined ? undefined : resolveDocumentPath(base.directory, src)
      if (base === undefined || path === undefined || /^[A-Za-z]:\//u.test(path) || path.startsWith('//')) return <LocalImageLink path={src} alt={alt} open={open} />
      return <img className={css.image} src={ideRawUrl({ workspaceId: base.workspaceId, rootId: base.rootId, path, version: options.revision })} alt={alt ?? ''} loading="lazy" {...shown} />
    },
    table({ children, align, width, node }) {
      return <div className={css.tableScroll} data-line={sourceLine(node)}><table align={align} width={width}>{children}</table></div>
    },
  }
}

const REMARK_PLUGINS = [remarkGfm, remarkMath, remarkDollarAmounts]
const CHAT_OPTIONS: MarkdownOptions = { codeActions: true }

/** Markdown with explicit link, image and code-block behavior; memoized because previews re-render while typing. */
export const MarkdownDocument = memo(function MarkdownDocument({ text, className, options }: {
  text: string
  className?: string | undefined
  options: MarkdownOptions
}): ReactElement {
  const components = useMemo(() => markdownComponents(options), [options])
  // Math renders as plain text for the moment KaTeX takes to load the first time.
  const katex = useMathPlugin(text)
  const file = options.file
  const rehypePlugins = useMemo(() => [
    ...(file === undefined ? [] : [rehypeSafeHtml]), ...(katex === undefined ? [] : [katex]), rehypeAnchors(options.sourceLines === true),
  ], [katex, file, options.sourceLines])
  const remarkRehypeOptions = useMemo(() => file === undefined ? undefined
    : { footnoteLabel: file.footnoteLabel, footnoteBackLabel: file.footnoteBackLabel, footnoteLabelProperties: {} }, [file])
  return (
    <div className={clsx(css.markdown, className)} data-markdown-root>
      <ReactMarkdown remarkPlugins={file === undefined ? REMARK_PLUGINS : DOCUMENT_REMARK_PLUGINS} rehypePlugins={rehypePlugins}
        remarkRehypeOptions={remarkRehypeOptions} components={components} urlTransform={file === undefined ? markdownUrl : documentUrl}>{text}</ReactMarkdown>
    </div>
  )
})

/** Rendered assistant markdown; memoized because transcripts re-render while streaming. */
export const Markdown = memo(function Markdown({ text, className }: { text: string; className?: string | undefined }): ReactElement {
  return <MarkdownDocument text={text} className={className} options={CHAT_OPTIONS} />
})

/** Link reference and footnote definitions apply to the whole document, so text holding one is not split. */
const DEFINITION = /^ {0,3}\[[^\]\n]+\]:/mu
const FENCE = /^ {0,3}(`{3,}|~{3,})/u
/** A line after a blank one that may continue the block before it: indented content, list items. */
const CONTINUATION = /^(?:[ \t]|[*+-](?:[ \t]|$)|\d{1,9}[.)](?:[ \t]|$))/u

/**
 * Split Markdown into top-level blocks that render the same alone as together: at blank lines outside code fences and
 * math blocks, before a line that cannot continue the block above it.
 * @param text Markdown source.
 * @returns The blocks; joined, they give back the text.
 */
export function markdownBlocks(text: string): string[] {
  if (DEFINITION.test(text)) return [text]
  const lines = text.split('\n')
  const blocks: string[] = []
  let start = 0
  let fence: string | undefined
  let math = false
  let blank = false
  for (let index = 0; index < lines.length; index++) {
    const line = lines[index]!
    if (fence !== undefined) {
      const close = FENCE.exec(line)
      if (close !== null && close[1]![0] === fence[0] && close[1]!.length >= fence.length && line.trim() === close[1]) fence = undefined
      continue
    }
    if (math) {
      if (line.trim() === '$$') math = false
      continue
    }
    if (line.trim() === '') { blank = true; continue }
    if (blank && index > start && !CONTINUATION.test(line)) {
      blocks.push(lines.slice(start, index).join('\n'))
      start = index
    }
    blank = false
    const open = FENCE.exec(line)
    if (open !== null) fence = open[1]!
    else if (line.trim() === '$$') math = true
  }
  blocks.push(lines.slice(start).join('\n'))
  return blocks
}

const CHAT_COMPONENTS = markdownComponents(CHAT_OPTIONS)
const STREAMING_REHYPE = [rehypeAnchors(false)]

/** One block of a streaming reply; a finished block keeps its rendering while the reply grows below it. */
const MarkdownBlock = memo(function MarkdownBlock({ text, katex }: { text: string; katex: ReturnType<typeof useMathPlugin> }): ReactElement {
  const rehypePlugins = useMemo(() => katex === undefined || !mayContainMath(text) ? STREAMING_REHYPE : [katex, ...STREAMING_REHYPE], [katex, text])
  return <ReactMarkdown remarkPlugins={REMARK_PLUGINS} rehypePlugins={rehypePlugins} components={CHAT_COMPONENTS} urlTransform={markdownUrl}>{text}</ReactMarkdown>
})

/**
 * A reply while it streams: only its last block is parsed again on each update, instead of the whole text. The finished
 * reply is rendered whole by {@link Markdown}.
 */
export const StreamingMarkdown = memo(function StreamingMarkdown({ text, className }: { text: string; className?: string | undefined }): ReactElement {
  const katex = useMathPlugin(text)
  const blocks = useMemo(() => markdownBlocks(text), [text])
  return (
    <div className={clsx(css.markdown, className)} data-markdown-root>
      {blocks.map((block, index) => <MarkdownBlock key={index} text={block} katex={katex} />)}
    </div>
  )
})
