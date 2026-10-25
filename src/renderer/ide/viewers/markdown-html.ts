/** What Markdown files hold beyond CommonMark and GFM, for the preview and exports: raw HTML, rebuilt from an allowlist of
 *  elements and attributes as GitHub shows READMEs, and Typora's `==highlight==`. Comments are never shown (they hold
 *  notes not meant for readers), `<img style="zoom:50%">` keeps its size, and nothing a cloned repository writes can run
 *  script, style the page or load from the web. */
import type { Options } from 'react-markdown'
import remarkGfm from 'remark-gfm'
import remarkMath from 'remark-math'
import { remarkDollarAmounts } from './markdown-math.ts'

/** Ids from documents get this prefix so document text can never name a global of the page. */
export const ANCHOR_PREFIX = 'md-'

/** Images embedded in the document itself, which load nothing from anywhere. */
const DATA_IMAGE = /^data:image\/(?:png|jpe?g|gif|webp|bmp|avif|x-icon|svg\+xml)[;,]/iu

/** @param url Image target. @returns Whether it is an image embedded as a data URL. */
export function isDataImage(url: string): boolean {
  return DATA_IMAGE.test(url)
}

interface Point { line: number; column: number; offset?: number | undefined }
/** The fields of an mdast or hast node the plugins read and write. */
interface Node {
  type: string
  tagName?: string
  value?: string
  properties?: Record<string, unknown>
  children?: Node[]
  data?: Record<string, unknown>
  position?: { start: Point; end: Point } | undefined
}

type Token =
  | { kind: 'text'; text: string; at: number }
  | { kind: 'open'; name: string; attributes: Array<readonly [string, string]>; selfClosing: boolean; at: number }
  | { kind: 'close'; name: string; at: number }

/** Elements whose content is not markup: everything up to their end tag is dropped with them. */
const RAW_TEXT = new Set(['script', 'style', 'textarea', 'title', 'xmp', 'iframe', 'noembed', 'noframes', 'noscript', 'plaintext'])
/** Elements dropped with their content: active content, embedded documents, forms' option lists. */
const DROPPED = new Set([...RAW_TEXT, 'object', 'applet', 'embed', 'template', 'svg', 'math', 'select', 'datalist', 'frameset', 'frame',
  'head', 'canvas', 'audio', 'video', 'picture', 'map', 'base', 'link', 'meta', 'param', 'source', 'track', 'portal', 'slot'])
const VOID = new Set(['area', 'base', 'br', 'col', 'embed', 'frame', 'hr', 'img', 'input', 'keygen', 'link', 'meta', 'param', 'source', 'track', 'wbr'])

const CELL = ['align', 'colspan', 'rowspan', 'width', 'style']
/** Allowed elements and their attributes besides {@link GLOBAL_ATTRIBUTES}; any other element is dropped and keeps its content. */
const ALLOWED: Readonly<Record<string, readonly string[]>> = {
  a: ['href', 'name'], abbr: [], b: [], bdi: [], blockquote: [], br: [], caption: [], center: [], cite: [], code: [],
  col: ['span', 'width'], colgroup: ['span', 'width'], dd: [], del: [], details: ['open'], dfn: [], div: ['align', 'style'], dl: [], dt: [],
  em: [], figcaption: [], figure: [], font: ['color', 'size', 'face'], h1: ['align'], h2: ['align'], h3: ['align'], h4: ['align'], h5: ['align'],
  h6: ['align'], hr: [], i: [], img: ['src', 'alt', 'width', 'height', 'style', 'align'], ins: [], kbd: [], li: ['value'], mark: [],
  ol: ['start', 'type', 'reversed'], p: ['align', 'style'], pre: [], q: [], rp: [], rt: [], ruby: [], s: [], samp: [], small: [], span: ['style'],
  strike: [], strong: [], sub: [], summary: [], sup: [], table: ['align', 'width'], tbody: [], td: CELL, tfoot: [], th: CELL, thead: [],
  tr: ['align'], tt: [], u: [], ul: ['type'], var: [], wbr: [],
}
const GLOBAL_ATTRIBUTES = ['title', 'lang', 'dir', 'id']
/** hast property names of attributes whose name differs. */
const PROPERTY: Readonly<Record<string, string>> = { colspan: 'colSpan', rowspan: 'rowSpan' }

/** CSS the documents may set: sizes, colors and alignment, the things Typora and README authors write. */
const STYLE_PROPERTIES = new Set(['zoom', 'width', 'height', 'max-width', 'min-width', 'max-height', 'color', 'background-color',
  'text-align', 'vertical-align', 'font-weight', 'font-style', 'font-size', 'text-decoration'])
/** No quotes, slashes, backslashes, colons or `!`: nothing that could hold a `url()`, an escape or another declaration. */
const STYLE_VALUE = /^[#\w\s.,%()+-]{1,100}$/u
const CSS_FUNCTIONS = new Set(['rgb', 'rgba', 'hsl', 'hsla', 'calc'])
const LENGTH = /^\d{1,5}(?:\.\d{1,3})?(?:px|%)?$/u
const ALIGN = /^(?:left|right|center|justify|top|middle|bottom)$/iu
const COLOR = /^(?:#[\da-f]{3,8}|[a-z]{3,24}|(?:rgba?|hsla?)\([\d\s.,%]{1,40}\))$/iu
const ID = /^[\p{L}\p{N}_.:-]{1,128}$/u

const OPEN_TAG = /<([A-Za-z][A-Za-z\d-]*)((?:\s+[^\s"'>/=]+(?:\s*=\s*(?:"[^"]*"|'[^']*'|[^\s"'=<>`]+))?)*)\s*(\/?)>/uy
const CLOSE_TAG = /<\/([A-Za-z][A-Za-z\d-]*)\s*>/uy
const ATTRIBUTE = /([^\s"'>/=]+)(?:\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s"'=<>`]+)))?/gu

const ENTITIES: Readonly<Record<string, string>> = {
  amp: '&', lt: '<', gt: '>', quot: '"', apos: "'", nbsp: '\u00a0', ensp: '\u2002', emsp: '\u2003', thinsp: '\u2009', shy: '\u00ad',
  copy: '©', reg: '®', trade: '™', hellip: '…', mdash: '—', ndash: '–', middot: '·', bull: '•', times: '×', divide: '÷', plusmn: '±',
  deg: '°', laquo: '«', raquo: '»', ldquo: '“', rdquo: '”', lsquo: '‘', rsquo: '’', larr: '←', rarr: '→', uarr: '↑', darr: '↓', harr: '↔',
  le: '≤', ge: '≥', ne: '≠', infin: '∞', sect: '§', para: '¶', yen: '¥', euro: '€', pound: '£', cent: '¢', check: '✓',
}

function namedEntity(name: string): string | undefined {
  const known = ENTITIES[name]
  if (known !== undefined || typeof document === 'undefined') return known
  // The name is letters and digits only, so the probe parses no markup.
  const probe = document.createElement('span')
  probe.innerHTML = `&${name};`
  const decoded = probe.textContent ?? ''
  return decoded === `&${name};` ? undefined : decoded
}

/** @param text Text or an attribute value from HTML. @returns It with character references decoded. */
function decodeEntities(text: string): string {
  if (!text.includes('&')) return text
  return text.replace(/&(?:#(\d{1,7})|#[xX]([\da-fA-F]{1,6})|([A-Za-z][A-Za-z\d]{1,31}));/gu, (whole, decimal?: string, hex?: string, name?: string) => {
    if (name !== undefined) return namedEntity(name) ?? whole
    const code = decimal !== undefined ? Number(decimal) : Number.parseInt(hex ?? '', 16)
    return code > 0 && code <= 0x10ffff && (code < 0xd800 || code > 0xdfff) ? String.fromCodePoint(code) : '\ufffd'
  })
}

/**
 * Split HTML into text, start tags and end tags. Comments, doctypes, CDATA and processing instructions are dropped, and
 * the content of {@link RAW_TEXT} elements is skipped up to their end tag, as a browser reads it.
 */
function tokenize(html: string): Token[] {
  const tokens: Token[] = []
  let index = 0
  let text = 0
  while (index < html.length) {
    const lt = html.indexOf('<', index)
    if (lt < 0) break
    let end: number | undefined
    const found: Token[] = []
    const skipTo = (marker: string, from: number): number => {
      const close = html.indexOf(marker, from)
      return close < 0 ? html.length : close + marker.length
    }
    if (html.startsWith('<!--', lt)) end = skipTo('-->', lt + 4)
    else if (html.startsWith('<![CDATA[', lt)) end = skipTo(']]>', lt + 9)
    else if (html[lt + 1] === '!' || html[lt + 1] === '?') end = skipTo('>', lt + 2)
    else {
      OPEN_TAG.lastIndex = lt
      const open = OPEN_TAG.exec(html)
      if (open !== null) {
        const name = open[1]!.toLowerCase()
        const selfClosing = open[3] === '/'
        found.push({ kind: 'open', name, attributes: attributesOf(open[2] ?? ''), selfClosing, at: lt })
        end = OPEN_TAG.lastIndex
        if (RAW_TEXT.has(name) && !selfClosing) {
          const close = new RegExp(`</${name}(?=[\\s/>])[^>]*>`, 'giu')
          close.lastIndex = end
          const match = close.exec(html)
          if (match !== null) found.push({ kind: 'close', name, at: match.index })
          end = match === null ? html.length : close.lastIndex
        }
      } else {
        CLOSE_TAG.lastIndex = lt
        const close = CLOSE_TAG.exec(html)
        if (close !== null) {
          found.push({ kind: 'close', name: close[1]!.toLowerCase(), at: lt })
          end = CLOSE_TAG.lastIndex
        }
      }
    }
    // A `<` that starts no tag is text.
    if (end === undefined) { index = lt + 1; continue }
    if (lt > text) tokens.push({ kind: 'text', text: html.slice(text, lt), at: text })
    tokens.push(...found)
    index = end
    text = end
  }
  if (text < html.length) tokens.push({ kind: 'text', text: html.slice(text), at: text })
  return tokens
}

function attributesOf(source: string): Array<readonly [string, string]> {
  return [...source.matchAll(ATTRIBUTE)].map(match => [match[1]!.toLowerCase(), match[2] ?? match[3] ?? match[4] ?? ''] as const)
}

/** @returns The target, or `undefined` for scripts and data that is not an image; the Markdown URL filter runs after this. */
function safeUrl(value: string, image: boolean): string | undefined {
  // Browsers ignore tabs, line ends and other controls inside a scheme, so `java\tscript:` is still a script.
  const compact = value.replace(/[\u0000-\u0020\u007f]/gu, '')
  const scheme = /^([a-z][a-z\d+.-]*):/iu.exec(compact)?.[1]?.toLowerCase()
  if (scheme === 'data') return image && DATA_IMAGE.test(compact) ? compact : undefined
  if (scheme === 'javascript' || scheme === 'vbscript' || scheme === 'livescript') return undefined
  return value
}

/** @returns The declarations of {@link STYLE_PROPERTIES} with plain values, `''` when none is left. */
function safeStyle(value: string): string {
  const kept: string[] = []
  for (const declaration of value.split(';')) {
    const colon = declaration.indexOf(':')
    if (colon < 0) continue
    const property = declaration.slice(0, colon).trim().toLowerCase()
    const setting = declaration.slice(colon + 1).trim()
    if (!STYLE_PROPERTIES.has(property) || !STYLE_VALUE.test(setting)) continue
    if ([...setting.matchAll(/([a-z-]+)\s*\(/giu)].some(call => !CSS_FUNCTIONS.has(call[1]!.toLowerCase()))) continue
    kept.push(`${property}: ${setting}`)
  }
  return kept.join('; ')
}

function propertiesOf(tag: string, attributes: ReadonlyArray<readonly [string, string]>): Record<string, unknown> {
  const allowed = ALLOWED[tag] ?? []
  const result: Record<string, unknown> = {}
  for (const [name, raw] of attributes) {
    if (!allowed.includes(name) && !GLOBAL_ATTRIBUTES.includes(name)) continue
    const value = decodeEntities(raw).trim()
    switch (name) {
      case 'href': case 'src': {
        const url = safeUrl(value, name === 'src')
        if (url !== undefined) result[name] = url
        break
      }
      case 'style': {
        const style = safeStyle(value)
        if (style !== '') result.style = style
        break
      }
      case 'id': case 'name':
        if (ID.test(value) && result.id === undefined) result.id = `${ANCHOR_PREFIX}${value}`
        break
      case 'width': case 'height':
        if (LENGTH.test(value)) result[name] = value
        break
      case 'colspan': case 'rowspan': case 'span': case 'start': case 'value': case 'size':
        if (/^\d{1,4}$/u.test(value)) result[PROPERTY[name] ?? name] = value
        break
      case 'align':
        if (ALIGN.test(value)) result.align = value.toLowerCase()
        break
      case 'open': case 'reversed':
        result[name] = true
        break
      case 'color':
        if (COLOR.test(value)) result.color = value
        break
      case 'type':
        if (/^(?:[1aAiI]|disc|circle|square)$/u.test(value)) result.type = value
        break
      case 'dir':
        if (/^(?:ltr|rtl|auto)$/iu.test(value)) result.dir = value.toLowerCase()
        break
      default:
        // title, alt, lang, face: plain text.
        result[name] = value
    }
  }
  return result
}

/** Starting one of these closes an open `<p>`, as in a browser. */
const CLOSES_PARAGRAPH = new Set(['address', 'article', 'aside', 'blockquote', 'center', 'details', 'div', 'dl', 'figure', 'footer', 'h1', 'h2',
  'h3', 'h4', 'h5', 'h6', 'header', 'hr', 'ol', 'p', 'pre', 'section', 'table', 'ul'])
const TABLE_SECTIONS = new Set(['thead', 'tbody', 'tfoot'])

/** Close what starting `name` ends in a browser: a paragraph before a block, a list item before the next, a table cell or row. */
function impliedEnd(open: Node[], name: string): void {
  const top = (): string | undefined => open.at(-1)?.tagName
  if (CLOSES_PARAGRAPH.has(name) && top() === 'p') open.pop()
  if (name === 'li' && top() === 'li') open.pop()
  if ((name === 'dt' || name === 'dd') && (top() === 'dt' || top() === 'dd')) open.pop()
  const row = name === 'tr' || TABLE_SECTIONS.has(name)
  if ((row || name === 'td' || name === 'th') && (top() === 'td' || top() === 'th')) open.pop()
  if (row && top() === 'tr') open.pop()
  if (TABLE_SECTIONS.has(name) && TABLE_SECTIONS.has(top() ?? '')) open.pop()
}

/** Elements Markdown puts inside a paragraph. */
const PHRASING = new Set(['a', 'abbr', 'b', 'bdi', 'br', 'cite', 'code', 'del', 'dfn', 'em', 'font', 'i', 'img', 'input', 'ins', 'kbd', 'mark', 'q',
  'rp', 'rt', 'ruby', 's', 'samp', 'small', 'span', 'strike', 'strong', 'sub', 'sup', 'tt', 'u', 'var', 'wbr'])

/**
 * Rebuild the raw HTML among `parent`'s children. A start tag and its end tag may arrive in separate raw nodes with
 * Markdown between them (`H<sub>2</sub>O`, a `<details>` around paragraphs), so elements stay open across siblings and
 * close at their end tag or at the end of `parent`.
 */
function rebuild(parent: Node): void {
  const children = parent.children
  if (children === undefined) return
  for (const child of children) rebuild(child)
  if (!children.some(child => child.type === 'raw')) return
  const output: Node[] = []
  const open: Node[] = []
  const into = (): Node[] => open.at(-1)?.children ?? output
  // A dropped element takes the text and inline Markdown up to its end tag with it, but ends at the next block: an
  // unclosed `<svg>` or `<iframe>` never hides the paragraphs after it.
  let dropping: { name: string; depth: number } | undefined
  for (const child of children) {
    if (child.type !== 'raw') {
      if (dropping !== undefined && (child.type === 'text' || PHRASING.has(child.tagName ?? ''))) continue
      dropping = undefined
      if (child.type === 'element') impliedEnd(open, child.tagName ?? '')
      into().push(child)
      continue
    }
    const html = child.value ?? ''
    const start = child.position?.start
    let line = start?.line
    let counted = 0
    const lineAt = (at: number): number | undefined => {
      if (line === undefined) return undefined
      for (; counted < at; counted++) if (html.charCodeAt(counted) === 10) line++
      return line
    }
    for (const token of tokenize(html)) {
      if (dropping !== undefined) {
        if (token.kind === 'open' && token.name === dropping.name && !token.selfClosing && !VOID.has(token.name)) dropping.depth++
        if (token.kind === 'close' && token.name === dropping.name && --dropping.depth === 0) dropping = undefined
        continue
      }
      if (token.kind === 'text') {
        if (token.text !== '') into().push({ type: 'text', value: decodeEntities(token.text) })
        continue
      }
      if (token.kind === 'close') {
        const index = open.findLastIndex(element => element.tagName === token.name)
        if (index >= 0) open.length = index
        continue
      }
      if (DROPPED.has(token.name)) {
        if (!token.selfClosing && !VOID.has(token.name)) dropping = { name: token.name, depth: 1 }
        continue
      }
      if (ALLOWED[token.name] === undefined) continue
      impliedEnd(open, token.name)
      const at = lineAt(token.at)
      const position = at === undefined ? undefined : { start: { line: at, column: 1 }, end: { line: at, column: 1 } }
      if (token.name === 'tr' && open.at(-1)?.tagName === 'table') {
        const body: Node = { type: 'element', tagName: 'tbody', properties: {}, children: [], position }
        into().push(body)
        open.push(body)
      }
      const element: Node = { type: 'element', tagName: token.name, properties: propertiesOf(token.name, token.attributes), children: [], position }
      into().push(element)
      if (!token.selfClosing && !VOID.has(token.name)) open.push(element)
    }
  }
  parent.children = output
}

/**
 * Rehype plugin: raw HTML becomes elements of the allowlist, without comments, scripts, event handlers or styles beyond
 * sizes, colors and alignment. Elements keep the source line of their tag, for scroll sync.
 * @returns The transformer.
 */
export function rehypeSafeHtml() {
  return (tree: Node): void => { rebuild(tree) }
}

/** `==text==`: the text does not start or end with a space and holds no `==`. */
const MARK = /==(?![\s=])((?:[^=]|=(?!=))*?[^\s=])==/gu

function highlight(node: Node): void {
  const children = node.children
  if (children === undefined) return
  for (let index = 0; index < children.length; index++) {
    const child = children[index]!
    if (child.type !== 'text') { highlight(child); continue }
    const value = child.value ?? ''
    if (!value.includes('==')) continue
    const parts: Node[] = []
    let last = 0
    for (const match of value.matchAll(MARK)) {
      if (match.index > last) parts.push({ type: 'text', value: value.slice(last, match.index) })
      parts.push({ type: 'mark', data: { hName: 'mark' }, children: [{ type: 'text', value: match[1]! }] })
      last = match.index + match[0].length
    }
    if (parts.length === 0) continue
    if (last < value.length) parts.push({ type: 'text', value: value.slice(last) })
    children.splice(index, 1, ...parts)
    index += parts.length - 1
  }
}

/**
 * Remark plugin: Typora's `==highlight==` becomes `<mark>`. Only text is searched, so code and math keep their `==`, and a
 * highlight does not run across other formatting.
 * @returns The transformer.
 */
export function remarkMark() {
  return (tree: Node): void => { highlight(tree) }
}

type Plugins = NonNullable<Options['remarkPlugins']>

/** How Markdown files are read, in the preview and in exports: GFM without single-tilde strikethrough (`H~2~O` stays
 *  text, as the rich editor writes it), math without dollar amounts, and highlights. */
export const DOCUMENT_REMARK_PLUGINS: Plugins = [[remarkGfm, { singleTilde: false }], remarkMath, remarkDollarAmounts, remarkMark]
