/** A Markdown document as one self-contained HTML page: what "Export as HTML" saves and what the desktop prints to PDF.
 *  Images are embedded, code is highlighted, math becomes MathML (no fonts to ship) and Mermaid diagrams become SVG. */
import { createElement, type ReactNode } from 'react'
import { renderToStaticMarkup } from 'react-dom/server'
import ReactMarkdown, { defaultUrlTransform, type Components } from 'react-markdown'
import rehypeKatex from 'rehype-katex'
import { grammarFor, highlightLines, loadGrammar } from '../../ui/highlight.ts'
import { ideRawUrl } from '../../../shared/ide-preview.ts'
import { rehypeAnchors, resolveDocumentPath } from '../../chat/Markdown.tsx'
import { getPrefs } from '../../prefs.ts'
import { splitFrontMatter } from './front-matter.ts'
import { DOCUMENT_REMARK_PLUGINS, isDataImage, rehypeSafeHtml, rehypeToc } from './markdown-html.ts'
import { markdownOutline } from './markdown-outline.ts'
import { isMermaid, renderMermaid } from './mermaid.ts'
import { viewerMessages } from './messages.ts'
import { detectSignature } from './signature.ts'
import { errorMessage, escapeHtml } from './viewer-utils.ts'

/** Strict CSP for exported standalone HTML: allows inline styles and embedded data/blob images, blocks all scripts and network loads. */
export const EXPORT_HTML_CSP = "default-src 'none'; img-src data: blob:; style-src 'unsafe-inline'; font-src data:;"

/** Largest image embedded into an export; a larger one keeps its link. */
const MAX_EMBEDDED_BYTES = 20 * 1024 * 1024

/** Allowed image MIME types for embedded data URLs; rejects HTML, scripts, plain text and arbitrary workspace files. */
const EMBEDDABLE_IMAGE_MIME = /^image\/(?:png|jpe?g|gif|webp|bmp|avif|x-icon|vnd\.microsoft\.icon|svg\+xml)$/iu

const STYLE = `
:root { color-scheme: light; }
body { margin: 0; background: #fff; color: #1f2328; font: 16px/1.75 "Segoe UI", "Microsoft YaHei", "PingFang SC", "Noto Sans CJK SC", system-ui, sans-serif; }
main { max-width: 860px; margin: 0 auto; padding: 40px 48px 72px; }
h1, h2, h3, h4, h5, h6 { font-weight: 600; line-height: 1.35; margin: 1.3em 0 0.6em; }
h1 { font-size: 2em; padding-bottom: 0.3em; border-bottom: 1px solid #d8dee4; }
h2 { font-size: 1.55em; padding-bottom: 0.3em; border-bottom: 1px solid #d8dee4; }
h3 { font-size: 1.3em; } h4 { font-size: 1.12em; } h5 { font-size: 1em; } h6 { font-size: 0.95em; color: #59636e; }
main > :first-child { margin-top: 0; }
p, ul, ol, table, pre, blockquote { margin: 0 0 1em; }
a { color: #0969da; text-decoration: none; } a:hover { text-decoration: underline; }
code { font-family: "Cascadia Code", Consolas, "Courier New", monospace; font-size: 0.9em; }
:not(pre) > code { background: rgba(175, 184, 193, 0.2); padding: 0.1em 0.35em; border-radius: 4px; }
pre { background: #f6f8fa; padding: 12px 16px; border-radius: 6px; overflow: auto; line-height: 1.5; }
pre code { font-size: 13.5px; }
blockquote { padding: 0 1em; color: #59636e; border-left: 0.25em solid #d0d7de; }
table { border-collapse: collapse; display: block; max-width: 100%; overflow: auto; }
th, td { border: 1px solid #d0d7de; padding: 6px 13px; } th { background: #f6f8fa; font-weight: 600; }
img { max-width: 100%; }
hr { border: 0; border-top: 1px solid #d0d7de; margin: 1.5em 0; }
li + li { margin-top: 0.25em; }
.task-list-item { list-style: none; }
.task-list-item > input[type="checkbox"] { margin: 0 0.4em 0 -1.4em; vertical-align: middle; }
math[display="block"] { margin: 1em 0; }
@media screen { math[display="block"] { overflow-x: auto; } }
.mermaid-diagram { margin: 1em 0; text-align: center; } .mermaid-diagram svg { max-width: 100%; height: auto; }
.footnotes { font-size: 0.9em; color: #59636e; border-top: 1px solid #d0d7de; margin-top: 2em; }
.footnotes > h2 { font-size: 1em; margin: 1em 0 0.5em; padding: 0; border: 0; color: inherit; }
.md-toc { margin: 0 0 1em; } .md-toc ul { margin: 0; padding-left: 1.5em; list-style: none; } .md-toc > ul { padding-left: 0; }
.md-toc li + li, .md-toc li > ul { margin-top: 0.15em; }
mark { background: rgba(247, 173, 49, 0.38); color: inherit; padding: 0 2px; border-radius: 2px; }
kbd { font: 0.85em "Cascadia Code", Consolas, monospace; padding: 1px 5px; border: 1px solid #d0d7de; border-bottom-width: 2px; border-radius: 4px; background: #f6f8fa; }
details { margin: 0 0 1em; padding: 6px 12px; border: 1px solid #d0d7de; border-radius: 6px; } summary { cursor: pointer; font-weight: 600; }
.hljs-comment, .hljs-quote { color: #6e7781; font-style: italic; }
.hljs-keyword, .hljs-selector-tag, .hljs-type, .hljs-doctag { color: #cf222e; }
.hljs-string, .hljs-regexp, .hljs-addition, .hljs-meta .hljs-string { color: #0a3069; }
.hljs-number, .hljs-literal, .hljs-variable, .hljs-template-variable, .hljs-attr, .hljs-attribute { color: #0550ae; }
.hljs-title, .hljs-section, .hljs-title.function_, .hljs-title.class_ { color: #8250df; }
.hljs-built_in, .hljs-symbol, .hljs-params { color: #953800; }
.hljs-name, .hljs-selector-id, .hljs-selector-class { color: #116329; }
.hljs-deletion { color: #82071e; background: #ffebe9; } .hljs-meta { color: #6e7781; }
.hljs-emphasis { font-style: italic; } .hljs-strong { font-weight: 600; }
@page { margin: 18mm 16mm; }
@media print {
  main { max-width: none; padding: 0; }
  pre, table, img, .mermaid-diagram, blockquote { break-inside: avoid; }
  h1, h2, h3, h4, h5, h6 { break-after: avoid; }
  pre { white-space: pre-wrap; word-break: break-word; }
}
`

/**
 * URL filter for exported standalone HTML:
 * - Images keep data:image/*, relative paths, http(s) URLs, and Windows drive paths so in-project images can be embedded
 *   (unembedded local drive/file paths are stripped afterward).
 * - Links keep only safe external protocols (`http:`, `https:`, `mailto:`), fragments, and relative paths — stripping
 *   `file://`, Windows drive paths (`C:\...`), UNC paths (`\\...`), and script URLs.
 */
function exportUrl(url: string, key: string, node: { tagName: string }): string {
  const trimmed = url.trim()
  if (key === 'src' && node.tagName === 'img') {
    if (isDataImage(trimmed)) return trimmed
    if (/^[A-Za-z]:[\\/]/u.test(trimmed)) return trimmed
    return defaultUrlTransform(trimmed)
  }
  if (/^(?:file:|\\\\|[A-Za-z]:[\\/])/iu.test(trimmed)) return ''
  return defaultUrlTransform(trimmed)
}

/**
 * @param markdown Document text.
 * @param fallback File name without its extension.
 * @returns The `title:` of the front matter, else the first heading, else the file name.
 */
export function documentTitle(markdown: string, fallback: string): string {
  markdown = markdown.replace(/\r\n?/gu, '\n')
  const front = splitFrontMatter(markdown).front
  const titled = /^title:[ \t]*["']?(.+?)["']?[ \t]*$/mu.exec(front)?.[1]
  return titled ?? markdownOutline(markdown)[0]?.text ?? fallback
}

/** Code blocks highlighted with the editor's grammars; Mermaid fences become placeholders that get their SVG afterwards. */
function codeComponent(diagrams: string[]): Components['code'] {
  return function Code({ className, children }): ReactNode {
    const language = /language-([\w#+.-]+)/u.exec(className ?? '')?.[1]
    const text = String(children ?? '')
    // Inline code has no language and no line end.
    if (language === undefined && !text.includes('\n')) return createElement('code', null, children)
    const source = text.replace(/\n$/u, '')
    if (isMermaid(language)) {
      diagrams.push(source)
      return createElement('span', { 'data-mermaid': diagrams.length - 1 })
    }
    const lines = highlightLines(source, language)
    if (lines === undefined) return createElement('code', { className }, source)
    return createElement('code', { className }, lines.flatMap((line, index) => [
      ...line.map((span, position) => createElement('span', { key: `${index}-${position}`, className: span.className }, span.text)),
      index < lines.length - 1 ? '\n' : null,
    ]))
  }
}

/**
 * Resolve an image target in an exported document against its directory (and optional Windows project root).
 * @returns Project-relative path when the target lies inside the project, or `undefined` for web/external/outside paths.
 */
export function resolveExportImagePath(directory: string, target: string, hostRoot?: string | undefined): string | undefined {
  let candidate = target.trim()
  if (/^[A-Za-z]:[\\/]/u.test(candidate)) {
    const root = hostRoot?.replaceAll('\\', '/').replace(/\/+$/u, '')
    const normalized = candidate.replaceAll('\\', '/')
    if (root === undefined || !normalized.toLowerCase().startsWith(`${root.toLowerCase()}/`)) return undefined
    candidate = `/${normalized.slice(root.length + 1)}`
  } else if (/^(?:[a-z][a-z\d+.-]*:|\/\/|\\\\)/iu.test(candidate)) {
    return undefined
  }
  return resolveDocumentPath(directory, candidate)
}

/**
 * Build the page.
 * @param options.text Document text.
 * @param options.name File name without its extension, the title when the document has none.
 * @param options.image Bytes of an image the document refers to, as a data URL, or `undefined` to keep its link.
 * @returns A complete HTML document.
 */
export async function exportHtml({ text, name, image, locale = getPrefs().locale }: {
  text: string
  name: string
  image: (target: string) => Promise<string | undefined>
  /** Language of the page, the interface language by default; the TOC and footnote labels match this locale. */
  locale?: 'zh' | 'en' | undefined
}): Promise<string> {
  text = text.replace(/\r\n?/gu, '\n')
  // Front matter is metadata: it gives the page its title and is not printed, as in Typora's exports.
  const { body } = splitFrontMatter(text)
  // Grammars load once; the page is rendered after all of them arrived.
  const fences = [...body.matchAll(/^ {0,3}(?:`{3,}|~{3,})[ \t]*([\w#+.-]+)/gmu)].map(match => grammarFor(match[1]))
  await Promise.all([...new Set(fences)].filter((value): value is string => value !== undefined).map(loadGrammar))
  const diagrams: string[] = []
  const tocLabel = viewerMessages.tFor(locale, 'toc')
  const footnoteLabel = viewerMessages.tFor(locale, 'footnotes')
  const footnoteBackLabel = viewerMessages.tFor(locale, 'footnoteBack')
  const markup = renderToStaticMarkup(createElement(ReactMarkdown, {
    // The preview's reading of the file (markdown-html.ts): its allowlisted HTML, highlights, no dollar amounts as math.
    remarkPlugins: DOCUMENT_REMARK_PLUGINS,
    // Headings get the preview's ids, which the links of a `[TOC]` point to in the page and the PDF.
    rehypePlugins: [rehypeSafeHtml, [rehypeKatex, { output: 'mathml', throwOnError: false }], rehypeAnchors(false), rehypeToc(tocLabel)],
    remarkRehypeOptions: { footnoteLabel, footnoteBackLabel, footnoteLabelProperties: {} },
    components: { code: codeComponent(diagrams) },
    urlTransform: exportUrl,
  }, body))
  const page = new DOMParser().parseFromString(`<main>${markup}</main>`, 'text/html')
  for (const [index, source] of diagrams.entries()) {
    const placeholder = page.querySelector(`[data-mermaid="${index}"]`)
    const block = placeholder?.closest('pre') ?? placeholder
    if (block === null || block === undefined) continue
    const figure = page.createElement('div')
    figure.className = 'mermaid-diagram'
    try { figure.innerHTML = await renderMermaid(source, false) } catch (error) {
      figure.textContent = errorMessage(error)
    }
    block.replaceWith(figure)
  }
  // Deduplicate image fetches so repeated logos/icons are fetched and encoded only once.
  const cache = new Map<string, Promise<string | undefined>>()
  const resolveImage = (target: string): Promise<string | undefined> => {
    let task = cache.get(target)
    if (task === undefined) {
      task = image(target).catch(() => undefined)
      cache.set(target, task)
    }
    return task
  }
  const images = [...page.querySelectorAll('img')]
  await Promise.all(images.map(async (element) => {
    const target = element.getAttribute('src')
    if (target === null || /^data:/iu.test(target)) return
    const embedded = await resolveImage(target)
    if (embedded !== undefined) element.setAttribute('src', embedded)
    else if (/^(?:file:|\\\\|[A-Za-z]:[\\/])/iu.test(target.trim())) element.removeAttribute('src')
  }))
  // Strip any local file:// or Windows drive hrefs that slipped into raw HTML anchors.
  for (const link of page.querySelectorAll('a[href]')) {
    const href = (link.getAttribute('href') ?? '').trim()
    if (href === '' || /^(?:file:|\\\\|[A-Za-z]:[\\/])/iu.test(href)) link.removeAttribute('href')
  }
  const main = page.querySelector('main')!
  return `<!DOCTYPE html>\n<html lang="${locale === 'zh' ? 'zh-CN' : 'en'}">\n<head>\n<meta charset="utf-8">\n`
    + `<meta http-equiv="Content-Security-Policy" content="${EXPORT_HTML_CSP}">\n`
    + `<meta name="viewport" content="width=device-width, initial-scale=1">\n`
    + `<meta name="generator" content="Rainy">\n<title>${escapeHtml(documentTitle(text, name))}</title>\n<style>${STYLE}</style>\n</head>\n<body>\n`
    + `${main.outerHTML}\n</body>\n</html>\n`
}

/**
 * Read an image through the Host and embed it. Rejects non-image payloads (such as text or HTML files) so
 * arbitrary project files referenced via `![](secret.txt)` can never be exfiltrated into exported HTML.
 * @param url Address the page can fetch.
 * @returns A data URL, or `undefined` when the image is missing, not an image, or too large.
 */
export async function embedImage(url: string): Promise<string | undefined> {
  const response = await fetch(url, { credentials: 'include' })
  if (!response.ok) return undefined
  const headerType = (response.headers.get('content-type') ?? '').split(';')[0]!.trim().toLowerCase()
  const blob = await response.blob()
  if (blob.size === 0 || blob.size > MAX_EMBEDDED_BYTES) return undefined
  const mime = (blob.type || headerType).split(';')[0]!.trim().toLowerCase()
  let verifiedMime = EMBEDDABLE_IMAGE_MIME.test(mime) ? mime : ''
  if (verifiedMime === '' && (mime === '' || mime === 'application/octet-stream')) {
    const head = new Uint8Array(await blob.slice(0, 64).arrayBuffer())
    const sig = detectSignature(head)
    if (sig?.family === 'image') {
      const label = sig.label.toLowerCase()
      verifiedMime = label.includes('jpeg') ? 'image/jpeg'
        : label.includes('gif') ? 'image/gif'
          : label.includes('webp') ? 'image/webp'
            : label.includes('bmp') ? 'image/bmp'
              : label.includes('avif') ? 'image/avif'
                : label.includes('ico') ? 'image/x-icon'
                  : 'image/png'
    }
  }
  if (verifiedMime === '') return undefined
  const typedBlob = blob.type === verifiedMime ? blob : new Blob([blob], { type: verifiedMime })
  return await new Promise<string>((resolve, reject) => {
    const reader = new FileReader()
    reader.onload = () => { resolve(String(reader.result)) }
    reader.onerror = () => { reject(reader.error ?? new Error(viewerMessages.t('imageReadFailed'))) }
    reader.readAsDataURL(typedBlob)
  })
}

/**
 * Export a document of the open project: build the page, then save it through the desktop's save dialog (printed to PDF
 * for `pdf`), or download the HTML in a plain browser.
 * @param options.format PDF or HTML.
 * @param options.text Document text, unsaved edits included.
 * @param options.path Project-relative path of the document.
 * @param options.workspaceId Its project.
 * @param options.rootId Its mounted folder; omitted for the primary one.
 * @param options.hostRoot Project folder on this computer, when it is a Windows folder.
 * @returns The saved file, or `null` when the user cancelled.
 */
export async function exportDocument({ format, text, path, workspaceId, rootId, hostRoot, locale = getPrefs().locale }: {
  format: 'pdf' | 'html'
  text: string
  path: string
  workspaceId: string
  rootId?: string | undefined
  hostRoot?: string | undefined
  locale?: 'zh' | 'en' | undefined
}): Promise<string | null> {
  const name = (path.split('/').at(-1) ?? path).replace(/\.[^.]+$/u, '')
  const directory = path.split('/').slice(0, -1).join('/')
  const html = await exportHtml({ text, name, locale, image: async (target) => {
    const resolved = resolveExportImagePath(directory, target, hostRoot)
    return resolved === undefined ? undefined : embedImage(ideRawUrl({ workspaceId, rootId, path: resolved }))
  } })
  const bridge = window.__RAINY_EXPORT__
  if (bridge !== undefined) return (await bridge.save({ format, html, name }))?.path ?? null
  if (format === 'pdf') throw new Error(viewerMessages.tFor(locale, 'exportPdfDesktop'))
  const link = document.createElement('a')
  link.href = URL.createObjectURL(new Blob([html], { type: 'text/html' }))
  link.download = `${name}.html`
  link.click()
  setTimeout(() => { URL.revokeObjectURL(link.href) }, 10_000)
  return `${name}.html`
}
