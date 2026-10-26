/** Markdown the rich editor reads and writes beyond Crepe's own: reference links and images that keep their `[text][ref]`
 *  form and their definitions, Typora's `[TOC]` and `==highlight==`, and dollar signs written without an escape where a
 *  re-read would not take them for a formula. Part of the editor's dialect (rich-plugins.ts `markdownSyntax`). */
import { commandsCtx, parserCtx } from '@milkdown/kit/core'
import type { Ctx } from '@milkdown/kit/ctx'
import { imageSchema, linkSchema } from '@milkdown/kit/preset/commonmark'
import { markRule } from '@milkdown/kit/prose'
import { toggleMark } from '@milkdown/kit/prose/commands'
import type { DOMOutputSpec, Node as ProseNode } from '@milkdown/kit/prose/model'
import { Plugin, TextSelection, type Transaction } from '@milkdown/kit/prose/state'
import type { NodeView } from '@milkdown/kit/prose/view'
import { $command, $inputRule, $markSchema, $nodeSchema, $prose, $remark, $useKeymap } from '@milkdown/kit/utils'
import { nestHeadings, remarkMark, remarkToc, TOC_SOURCE, type TocEntry } from './markdown-html.ts'

/** The fields of an mdast node these plugins read and write. */
interface MdNode {
  type: string
  value?: string
  url?: string
  title?: string | null
  alt?: string | null
  label?: string | null
  identifier?: string
  referenceType?: 'full' | 'collapsed' | 'shortcut'
  children?: MdNode[]
  position?: { start: { offset?: number }; end: { offset?: number } }
}

/**
 * @param label A reference label as written.
 * @returns The identifier CommonMark matches it by, as mdast writes it: case and runs of white space do not count.
 */
export function referenceId(label: string): string {
  return label.replace(/[\t\n\r ]+/gu, ' ').trim().toLowerCase().toUpperCase().toLowerCase()
}

/**
 * Remark plugin in place of Milkdown's remark-inline-links, which turned every `[text][ref]` into `[text](url)` and
 * dropped the definitions: references get the target of their definition and stay references; definitions stay nodes.
 * @returns The transformer.
 */
export function remarkReferences() {
  return (tree: MdNode): void => {
    const definitions = new Map<string, { url: string; title: string | null }>()
    const collect = (node: MdNode): void => {
      // The first definition of a label wins, as in CommonMark.
      if (node.type === 'definition' && node.identifier !== undefined && !definitions.has(node.identifier)) {
        definitions.set(node.identifier, { url: node.url ?? '', title: node.title ?? null })
      }
      for (const child of node.children ?? []) collect(child)
    }
    collect(tree)
    const resolve = (node: MdNode): void => {
      const children = node.children
      if (children === undefined) return
      for (let index = 0; index < children.length; index++) {
        const child = children[index]!
        if (child.type === 'linkReference' || child.type === 'imageReference') {
          const found = definitions.get(child.identifier ?? '')
          if (found !== undefined) Object.assign(child, found)
          // The parser only makes references it has a definition for; keep the text of any other.
          else { children.splice(index, 1, ...child.children ?? []); index--; continue }
        }
        resolve(child)
      }
    }
    resolve(tree)
  }
}

export const references = $remark('rainyReferences', () => remarkReferences)

/** Attributes a link mark or image node keeps of the reference it was read from; the target it had then decides whether
 *  it is still written as that reference. */
const REFERENCE_ATTRS = {
  /** The label as written, `null` for an inline link. */
  reference: { default: null, validate: 'string|null' },
  referenceType: { default: null, validate: 'string|null' },
  referenceUrl: { default: null, validate: 'string|null' },
  referenceTitle: { default: null, validate: 'string|null' },
} as const

const NO_REFERENCE = { reference: null, referenceType: null, referenceUrl: null, referenceTitle: null }

/** @returns The DOM spec without the reference attributes, which are no HTML attributes. */
function withoutReference(spec: DOMOutputSpec): DOMOutputSpec {
  if (!Array.isArray(spec)) return spec
  const attributes: unknown = spec[1]
  if (typeof attributes !== 'object' || attributes === null || Array.isArray(attributes) || 'nodeType' in attributes) return spec
  const { reference: _reference, referenceType: _type, referenceUrl: _url, referenceTitle: _title, ...rest } = attributes as Record<string, unknown>
  return [spec[0] as string, rest, ...spec.slice(2)] as DOMOutputSpec
}

/** Links read from `[text][ref]`, `[ref][]` or `[ref]` are written back so while their target is the definition's. */
export const referenceLinks = linkSchema.extendSchema(previous => (ctx) => {
  const base = previous(ctx)
  return {
    ...base,
    attrs: { ...base.attrs, ...REFERENCE_ATTRS },
    toDOM: (mark, inline) => withoutReference(base.toDOM!(mark, inline)),
    parseMarkdown: {
      match: node => node.type === 'link' || node.type === 'linkReference',
      runner: (state, node, markType) => {
        if (node.type !== 'linkReference') { base.parseMarkdown.runner(state, node, markType); return }
        const { url, title, label, identifier, referenceType } = node as MdNode
        state.openMark(markType, { href: url, title: title ?? null, reference: label ?? identifier ?? '', referenceType, referenceUrl: url, referenceTitle: title ?? null })
        state.next(node.children)
        state.closeMark(markType)
      },
    },
    toMarkdown: {
      match: base.toMarkdown.match,
      runner: (state, mark, node) => {
        const { reference, referenceType, referenceUrl, referenceTitle, href, title } = mark.attrs as Record<string, string | null>
        if (reference === null || reference === undefined || href !== referenceUrl || (title ?? null) !== referenceTitle) return base.toMarkdown.runner(state, mark, node)
        state.withMark(mark, 'linkReference', undefined, { label: reference, identifier: referenceId(reference), referenceType })
        return undefined
      },
    },
  }
})

/** Images read from `![alt][ref]` and its short forms, likewise. */
export const referenceImages = imageSchema.extendSchema(previous => (ctx) => {
  const base = previous(ctx)
  return {
    ...base,
    attrs: { ...base.attrs, ...REFERENCE_ATTRS },
    toDOM: node => withoutReference(base.toDOM!(node)),
    parseMarkdown: {
      match: node => node.type === 'image' || node.type === 'imageReference',
      runner: (state, node, type) => {
        if (node.type !== 'imageReference') { base.parseMarkdown.runner(state, node, type); return }
        const { url, title, alt, label, identifier, referenceType } = node as MdNode
        state.addNode(type, { src: url ?? '', alt: alt ?? '', title: title ?? '', reference: label ?? identifier ?? '', referenceType,
          referenceUrl: url ?? '', referenceTitle: title ?? '' })
      },
    },
    toMarkdown: {
      match: base.toMarkdown.match,
      runner: (state, node) => {
        const { reference, referenceType, referenceUrl, referenceTitle, src, title, alt } = node.attrs as Record<string, string | null>
        if (reference === null || reference === undefined || src !== referenceUrl || title !== referenceTitle) { base.toMarkdown.runner(state, node); return }
        state.addNode('imageReference', undefined, undefined, { alt, label: reference, identifier: referenceId(reference), referenceType })
      },
    },
  }
})

/** A link reference definition, `[ref]: url "title"`: shown as written, where it is in the file, and written back so. */
export const definitionSchema = $nodeSchema('link_definition', () => ({
  group: 'block',
  atom: true,
  selectable: true,
  marks: '',
  attrs: {
    label: { default: '', validate: 'string' },
    identifier: { default: '', validate: 'string' },
    url: { default: '', validate: 'string' },
    title: { default: null, validate: 'string|null' },
  },
  parseDOM: [{
    tag: 'div[data-type="link-definition"]',
    getAttrs: (dom) => {
      if (!(dom instanceof HTMLElement)) return false
      const label = dom.dataset.label ?? ''
      return { label, identifier: referenceId(label), url: dom.dataset.url ?? '', title: dom.dataset.title ?? null }
    },
  }],
  toDOM: (node) => {
    const { label, url, title } = node.attrs as { label: string; url: string; title: string | null }
    const shown = `[${label}]: ${url === '' || /[\s<>]/u.test(url) ? `<${url}>` : url}${title === null ? '' : ` "${title}"`}`
    return ['div', { 'data-type': 'link-definition', class: 'rainy-link-definition', 'data-label': label, 'data-url': url,
      ...title === null ? {} : { 'data-title': title } }, shown]
  },
  parseMarkdown: {
    match: node => node.type === 'definition',
    runner: (state, node, type) => {
      const { label, identifier, url, title } = node as MdNode
      state.addNode(type, { label: label ?? identifier ?? '', identifier: identifier ?? referenceId(label ?? ''), url: url ?? '', title: title ?? null })
    },
  },
  toMarkdown: {
    match: node => node.type.name === 'link_definition',
    runner: (state, node) => {
      const { label, identifier, url, title } = node.attrs as { label: string; identifier: string; url: string; title: string | null }
      state.addNode('definition', undefined, undefined, { label, identifier, url, title })
    },
  },
}))

/** Definitions written one under another stay so, as they usually are. */
export function joinDefinitions(left: { type: string }, right: { type: string }): number | undefined {
  return left.type === 'definition' && right.type === 'definition' ? 0 : undefined
}

/** A deleted definition leaves its references as inline links to the same target, instead of text in the file. */
export const referenceKeeper = $prose(() => new Plugin({
  appendTransaction(transactions, old, state): Transaction | null {
    if (!transactions.some(transaction => transaction.docChanged)) return null
    const defined = (doc: ProseNode): Set<string> => {
      const ids = new Set<string>()
      doc.descendants((node) => {
        if (node.type.name === 'link_definition') ids.add(String(node.attrs.identifier))
        return !node.isTextblock
      })
      return ids
    }
    const before = defined(old.doc)
    if (before.size === 0) return null
    const now = defined(state.doc)
    if ([...before].every(id => now.has(id))) return null
    const orphan = (attrs: Record<string, unknown>): boolean => typeof attrs.reference === 'string' && !now.has(referenceId(attrs.reference))
    const transaction = state.tr
    state.doc.descendants((node, position) => {
      if (node.type.name === 'image' && orphan(node.attrs)) transaction.setNodeMarkup(position, undefined, { ...node.attrs, ...NO_REFERENCE })
      if (!node.isText) return
      for (const mark of node.marks) {
        if (mark.type.name !== 'link' || !orphan(mark.attrs)) continue
        const end = position + node.nodeSize
        transaction.removeMark(position, end, mark).addMark(position, end, mark.type.create({ ...mark.attrs, ...NO_REFERENCE }))
      }
    })
    return transaction.docChanged ? transaction : null
  },
}))

/** Typora's `[TOC]`: the document's headings, kept up to date, written back as it was spelled. */
export const tocSchema = $nodeSchema('toc', () => ({
  group: 'block',
  atom: true,
  selectable: true,
  marks: '',
  attrs: { source: { default: '[TOC]', validate: 'string' } },
  parseDOM: [{ tag: 'div[data-type="toc"]', getAttrs: dom => ({ source: dom instanceof HTMLElement ? dom.dataset.source ?? '[TOC]' : '[TOC]' }) }],
  toDOM: node => ['div', { 'data-type': 'toc', 'data-source': String(node.attrs.source), class: 'rainy-toc' }],
  parseMarkdown: {
    match: node => node.type === 'toc',
    runner: (state, node, type) => { state.addNode(type, { source: typeof node.value === 'string' ? node.value : '[TOC]' }) },
  },
  toMarkdown: {
    match: node => node.type.name === 'toc',
    runner: (state, node) => { state.addNode('toc', undefined, String(node.attrs.source)) },
  },
}))

/** One heading of the editor's document, as a table of contents lists it. */
interface TocHeading { readonly level: number; readonly text: string; /** Position among the document's headings. */ readonly index: number }

/** @param doc Document. @returns Its headings in document order, as the outline counts them. */
function tocHeadings(doc: ProseNode): TocHeading[] {
  const headings: TocHeading[] = []
  doc.descendants((node) => {
    if (node.type.name === 'heading') headings.push({ level: Number(node.attrs.level), text: node.textContent, index: headings.length })
    return !node.isTextblock
  })
  return headings
}

/** What the table of contents shows and does. */
export interface TocOptions {
  /** Its title. */
  readonly label: string
  /** Shown when the document has no headings. */
  readonly empty: string
  /** Scroll to heading `index` (document order). */
  readonly reveal: (index: number) => void
}

function drawToc(dom: HTMLElement, headings: readonly TocHeading[], options: TocOptions): void {
  const title = document.createElement('div')
  title.className = 'rainy-toc-title'
  title.textContent = options.label
  if (headings.length === 0) {
    const empty = document.createElement('div')
    empty.className = 'rainy-toc-empty'
    empty.textContent = options.empty
    dom.replaceChildren(title, empty)
    return
  }
  const list = (entries: readonly TocEntry<TocHeading>[]): HTMLElement => {
    const items = document.createElement('ul')
    for (const { heading, children } of entries) {
      const item = document.createElement('li')
      const link = document.createElement('a')
      link.href = '#'
      link.dataset.headingIndex = String(heading.index)
      link.textContent = heading.text === '' ? ' ' : heading.text
      item.append(link)
      if (children.length > 0) item.append(list(children))
      items.append(item)
    }
    return items
  }
  dom.replaceChildren(title, list(nestHeadings(headings)))
}

/**
 * The table of contents in the editor: a block that lists the headings, follows their changes and scrolls to the one
 * clicked. Clicking beside the entries selects the block, which Backspace removes.
 * @param options Labels and scrolling.
 * @returns The plugin.
 */
export function tocPlugin(options: TocOptions): ReturnType<typeof $prose> {
  return $prose(() => {
    const shown = new Set<HTMLElement>()
    let drawn = ''
    return new Plugin({
      props: {
        nodeViews: {
          toc: (_node, view): NodeView => {
            const dom = document.createElement('div')
            dom.className = 'rainy-toc'
            dom.dataset.type = 'toc'
            dom.contentEditable = 'false'
            drawToc(dom, tocHeadings(view.state.doc), options)
            shown.add(dom)
            const entry = (event: Event): HTMLElement | null =>
              event.target instanceof Element ? event.target.closest<HTMLElement>('a[data-heading-index]') : null
            dom.addEventListener('click', (event) => {
              const link = entry(event)
              if (link === null) return
              event.preventDefault()
              options.reveal(Number(link.dataset.headingIndex))
            })
            return {
              dom,
              update: node => node.type.name === 'toc',
              ignoreMutation: () => true,
              // The entries are links of their own; the rest of the block selects it.
              stopEvent: event => entry(event) !== null,
              destroy: () => { shown.delete(dom) },
            }
          },
        },
      },
      view: () => ({
        update(view, previous) {
          if (shown.size === 0 || previous.doc.eq(view.state.doc)) return
          const headings = tocHeadings(view.state.doc)
          const signature = JSON.stringify(headings)
          if (signature === drawn) return
          drawn = signature
          for (const dom of shown) drawToc(dom, headings, options)
        },
      }),
    })
  })
}

/** Enter after `[TOC]` alone in a paragraph makes it a table of contents, as in Typora. */
export const tocKeymap = $useKeymap('rainyToc', {
  MakeToc: {
    shortcuts: 'Enter',
    priority: 200,
    command: ctx => (state, dispatch) => {
      const { $from, empty } = state.selection
      const paragraph = $from.parent
      if (!empty || $from.depth !== 1 || paragraph.type.name !== 'paragraph' || $from.parentOffset !== paragraph.content.size) return false
      const text = paragraph.textContent
      if (paragraph.childCount !== 1 || !paragraph.firstChild!.isText || paragraph.firstChild!.marks.length > 0 || !TOC_SOURCE.test(text)) return false
      if (dispatch === undefined) return true
      const toc = tocSchema.type(ctx).create({ source: text })
      const after = state.schema.nodes.paragraph!.create()
      const start = $from.before()
      const transaction = state.tr.replaceWith(start, $from.after(), [toc, after])
      dispatch(transaction.setSelection(TextSelection.create(transaction.doc, start + toc.nodeSize + 1)).scrollIntoView())
      return true
    },
  },
})

/** `[TOC]` paragraphs, found as the preview finds them. */
export const tocParagraphs = $remark('rainyToc', () => remarkToc as never)

/** `==text==`, found as the preview finds it. */
export const highlights = $remark('rainyHighlight', () => remarkMark as never)

/** Typora's `==highlight==`, read as the preview reads it (markdown-html.ts `remarkMark`). */
export const highlightSchema = $markSchema('highlight', () => ({
  parseDOM: [{ tag: 'mark' }],
  toDOM: () => ['mark', { class: 'rainy-highlight' }],
  parseMarkdown: {
    match: node => node.type === 'mark',
    runner: (state, node, markType) => {
      state.openMark(markType)
      state.next(node.children)
      state.closeMark(markType)
    },
  },
  toMarkdown: {
    match: mark => mark.type.name === 'highlight',
    runner: (state, mark) => { state.withMark(mark, 'mark') },
  },
}))

/** Writes an mdast `mark` as `==…==`. */
export function markHandler(node: unknown, _parent: unknown, state: { createTracker: Function; containerPhrasing: Function }, info: unknown): string {
  const tracker = (state.createTracker as (info: unknown) => { move(text: string): string; current(): object })(info)
  let value = tracker.move('==')
  value += tracker.move((state.containerPhrasing as (node: unknown, info: object) => string)(node, { before: value, after: '=', ...tracker.current() }))
  value += tracker.move('==')
  return value
}

/** Typing `==text==` highlights the text. */
export const highlightInputRule = $inputRule(ctx => markRule(/(?<!=)==(?![\s=])((?:[^=]|=(?!=))*?[^\s=])==$/u, highlightSchema.type(ctx)))

export const toggleHighlightCommand = $command('ToggleHighlight', ctx => () => toggleMark(highlightSchema.type(ctx)))

/** Typora's Ctrl+Shift+H. */
export const highlightKeymap = $useKeymap('rainyHighlight', {
  ToggleHighlight: {
    shortcuts: 'Mod-Shift-h',
    command: (ctx) => {
      const commands = ctx.get(commandsCtx)
      return () => commands.call(toggleHighlightCommand.key)
    },
  },
})

/** The toolbar's highlight icon, in the style of Crepe's others. */
export const HIGHLIGHT_ICON = '<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24"><path d="M3 20h18v2H3zm11.06-15.94 3.88 3.88-7.07 7.07H7v-3.88zm1.41-1.41 1.42-1.42a1 1 0 0 1 1.41 0l2.47 2.47a1 1 0 0 1 0 1.41l-1.42 1.42z"/></svg>'

/** The fields of a parsed node the dollar pass reads. */
interface Positioned { type: string; children?: Positioned[]; position?: { start: { offset?: number }; end: { offset?: number } } }

/** Collect the offsets of the backslashes of `\$` escapes in the text of `node`; code and math hold none. */
function dollarEscapes(markdown: string, node: Positioned, into: number[]): void {
  if (node.type !== 'text') { for (const child of node.children ?? []) dollarEscapes(markdown, child, into); return }
  const start = node.position?.start.offset
  const end = node.position?.end.offset
  if (start === undefined || end === undefined) return
  let backslashes = 0
  for (let index = start; index < end; index++) {
    const character = markdown[index]
    if (character === '$' && backslashes % 2 === 1) into.push(index - 1)
    backslashes = character === '\\' ? backslashes + 1 : 0
  }
}

/** How many escapes of one block are tried one by one when they cannot all go. */
const MAX_TRIES = 16

/**
 * The serializer escapes every `$` of text. Drop the escapes a re-read does not need: the whole text is read without
 * them, else each top-level block, which keeps those whose removal would read differently (a formula, a dollar amount
 * pairing with another).
 * @param markdown Serialized document.
 * @param parse Parses Markdown into an mdast tree with offsets, as written.
 * @param read Reads Markdown into the document the editor would hold.
 * @returns The text with `\$` written `$` wherever that reads the same.
 */
export function bareDollars(markdown: string, parse: (text: string) => Positioned, read: (text: string) => { eq(other: never): boolean }): string {
  if (!markdown.includes('\\$')) return markdown
  const remove = (from: string, drop: readonly number[]): string => {
    let text = ''
    let at = 0
    for (const index of drop) { text += from.slice(at, index); at = index + 1 }
    return text + from.slice(at)
  }
  const blocks: { start: number; end: number; escapes: number[] }[] = []
  for (const block of parse(markdown).children ?? []) {
    const start = block.position?.start.offset
    const end = block.position?.end.offset
    if (start === undefined || end === undefined) continue
    const escapes: number[] = []
    dollarEscapes(markdown, block, escapes)
    if (escapes.length > 0) blocks.push({ start, end, escapes })
  }
  if (blocks.length === 0) return markdown
  // Usually every escape can go: one reading of the whole text instead of two per block.
  const all = blocks.flatMap(block => block.escapes)
  if (blocks.length > 1) {
    try {
      const candidate = remove(markdown, all)
      if (read(markdown).eq(read(candidate) as never)) return candidate
    } catch (_unreadable) { /* Block by block. */ }
  }
  const dropped: number[] = []
  for (const { start, end, escapes } of blocks) {
    const text = markdown.slice(start, end)
    let before: { eq(other: never): boolean }
    try { before = read(text) } catch (_unreadable) { continue }
    const same = (drop: readonly number[]): boolean => {
      try { return before.eq(read(remove(text, drop.map(index => index - start))) as never) } catch (_unreadable) { return false }
    }
    if (same(escapes)) { dropped.push(...escapes); continue }
    const kept: number[] = []
    for (const escape of escapes.slice(0, MAX_TRIES)) if (same([...kept, escape])) kept.push(escape)
    dropped.push(...kept)
  }
  return dropped.length === 0 ? markdown : remove(markdown, dropped)
}

/** Documents an editor read last, by text: a save reads the same texts several times (rich-plugins.ts `markdownReader`). */
const readings = new WeakMap<Ctx, Map<string, ProseNode>>()

/**
 * @param ctx The editor's context, once created.
 * @param text Markdown with LF line ends.
 * @returns The document the editor's parser makes of it; documents are immutable, so a recent reading is reused.
 */
export function readMarkdown(ctx: Ctx, text: string): ProseNode {
  let cache = readings.get(ctx)
  if (cache === undefined) { cache = new Map(); readings.set(ctx, cache) }
  const known = cache.get(text)
  if (known !== undefined) { cache.delete(text); cache.set(text, known); return known }
  const doc = ctx.get(parserCtx)(text)
  cache.set(text, doc)
  if (cache.size > 6) cache.delete(cache.keys().next().value!)
  return doc
}

/** Runs {@link bareDollars} on everything the editor writes, its own re-reads included. */
export const dollarEscapesPlugin = $remark('rainyBareDollars', ctx => function (this: { compiler?: unknown; parse(text: string): unknown }) {
  const compile = this.compiler
  if (typeof compile !== 'function') return
  const parse = (text: string): Positioned => this.parse(text) as Positioned
  this.compiler = (tree: unknown, file: unknown): unknown => {
    const written: unknown = compile(tree, file)
    if (typeof written !== 'string') return written
    return bareDollars(written, parse, text => readMarkdown(ctx, text))
  }
} as never)

