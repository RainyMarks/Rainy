/** ProseMirror plugins of the rich Markdown editor: Typora's shortcuts, find and replace, focus and typewriter modes, and
 *  Ctrl+click on links. Loaded with the editor, outside the viewer chunk. */
import { commandsCtx, editorViewCtx, parserCtx, remarkCtx, remarkStringifyOptionsCtx, serializerCtx, type CmdKey, type Editor } from '@milkdown/kit/core'
import type { Ctx, SliceType } from '@milkdown/kit/ctx'
import { toggleLinkCommand } from '@milkdown/kit/component/link-tooltip'
import type { Node as ProseNode } from '@milkdown/kit/prose/model'
import { keymap } from '@milkdown/kit/prose/keymap'
import { Plugin, PluginKey, TextSelection, type Command, type EditorState, type Transaction } from '@milkdown/kit/prose/state'
import { Decoration, DecorationSet, type EditorView } from '@milkdown/kit/prose/view'
import { codeBlockSchema, createCodeBlockCommand, remarkPreserveEmptyLinePlugin, toggleInlineCodeCommand, wrapInBlockquoteCommand, wrapInBulletListCommand,
  wrapInHeadingCommand, wrapInOrderedListCommand } from '@milkdown/kit/preset/commonmark'
import { addRowAfterCommand, createTable, remarkGFMPlugin, toggleStrikethroughCommand } from '@milkdown/kit/preset/gfm'
import { $prose, $remark, $useKeymap } from '@milkdown/kit/utils'
import { conventionalTables, type MarkdownReader } from './markdown-merge.ts'
import { remarkDollarAmounts } from './markdown-math.ts'

type StringifyOptions = typeof remarkStringifyOptionsCtx extends SliceType<infer T, string> ? T : never
type Handle = NonNullable<NonNullable<StringifyOptions['handlers']>['text']>

/** A run of document text that matches the search. */
export interface SearchMatch { readonly from: number; readonly to: number }

/** The find bar's query and where it matched. */
export interface SearchState {
  readonly query: string
  readonly caseSensitive: boolean
  readonly matches: readonly SearchMatch[]
  /** Index of the highlighted match, -1 when there is none. */
  readonly current: number
  readonly decorations: DecorationSet
}

/** Changes the find bar sends with a transaction's `searchKey` meta. */
export interface SearchChange { readonly query?: string; readonly caseSensitive?: boolean; readonly current?: number }

export const searchKey = new PluginKey<SearchState>('rainy-search')

/** Stands for an inline node in the searched text, so a match never spans one. */
const OBJECT = String.fromCharCode(0xfffc)

/**
 * Find the query in the document's text blocks, also across bold, links and other marks; code blocks, math and diagrams
 * are left out because they show their own editors.
 * @param doc Document.
 * @param query Text to find, matched literally.
 * @param caseSensitive Whether letter case must match.
 * @returns The matches in document order.
 */
export function findMatches(doc: ProseNode, query: string, caseSensitive: boolean): SearchMatch[] {
  if (query === '') return []
  const pattern = new RegExp(query.replace(/[.*+?^${}()|[\]\\]/gu, '\\$&'), caseSensitive ? 'gu' : 'giu')
  const matches: SearchMatch[] = []
  doc.descendants((node, position) => {
    if (!node.isTextblock) return true
    if (node.type.spec.code === true) return false
    let text = ''
    const positions: number[] = []
    node.forEach((child, offset) => {
      const start = position + 1 + offset
      if (child.isText) {
        for (let index = 0; index < child.text!.length; index++) positions.push(start + index)
        text += child.text
      } else {
        // An inline node (an image, inline math) is one position and never part of a match.
        positions.push(start)
        text += OBJECT
      }
    })
    for (const match of text.matchAll(pattern)) {
      if (match[0].length === 0 || match[0].includes(OBJECT)) continue
      const from = positions[match.index]!
      matches.push({ from, to: positions[match.index + match[0].length - 1]! + 1 })
    }
    return false
  })
  return matches
}

function searchState(doc: ProseNode, query: string, caseSensitive: boolean, matches: readonly SearchMatch[], current: number): SearchState {
  const index = matches.length === 0 ? -1 : Math.min(Math.max(current, 0), matches.length - 1)
  const decorations = DecorationSet.create(doc, matches.map((match, position) =>
    Decoration.inline(match.from, match.to, { class: position === index ? 'rainy-search-match rainy-search-current' : 'rainy-search-match' })))
  return { query, caseSensitive, matches, current: index, decorations }
}

/** Highlights the find bar's matches; the find bar drives it through {@link searchKey} metas. */
export const searchPlugin = $prose(() => new Plugin<SearchState>({
  key: searchKey,
  state: {
    init: (_config, state) => searchState(state.doc, '', false, [], -1),
    apply(transaction, previous, _old, state) {
      const change = transaction.getMeta(searchKey) as SearchChange | undefined
      if (change === undefined && !transaction.docChanged) return previous
      const query = change?.query ?? previous.query
      const caseSensitive = change?.caseSensitive ?? previous.caseSensitive
      const requery = transaction.docChanged || change?.query !== undefined || change?.caseSensitive !== undefined
      const matches = requery ? findMatches(state.doc, query, caseSensitive) : previous.matches
      return searchState(state.doc, query, caseSensitive, matches, change?.current ?? previous.current)
    },
  },
  props: { decorations: state => searchKey.getState(state)?.decorations },
}))

/** @param state Editor state. @returns The find bar's state, or an empty one before the plugin ran. */
export function currentSearch(state: EditorState): SearchState {
  return searchKey.getState(state) ?? searchState(state.doc, '', false, [], -1)
}

/**
 * Search for a query, highlighting the first match at or after the cursor.
 * @param view Editor.
 * @param query Text to find.
 * @param caseSensitive Whether letter case must match.
 */
export function setSearch(view: EditorView, query: string, caseSensitive: boolean): void {
  const matches = findMatches(view.state.doc, query, caseSensitive)
  const head = view.state.selection.from
  const after = matches.findIndex(match => match.from >= head)
  view.dispatch(view.state.tr.setMeta(searchKey, { query, caseSensitive, current: after < 0 ? 0 : after } satisfies SearchChange))
}

/** @param view Editor. @param delta 1 for the next match, -1 for the previous one. */
export function stepSearch(view: EditorView, delta: number): void {
  const { matches, current } = currentSearch(view.state)
  if (matches.length === 0) return
  view.dispatch(view.state.tr.setMeta(searchKey, { current: (current + delta + matches.length) % matches.length } satisfies SearchChange))
}

/** @param view Editor. @param text Replacement for the highlighted match; the next match is highlighted afterwards. */
export function replaceCurrent(view: EditorView, text: string): void {
  const { matches, current } = currentSearch(view.state)
  const match = matches[current]
  if (match === undefined) return
  view.dispatch(view.state.tr.insertText(text, match.from, match.to).setMeta(searchKey, { current } satisfies SearchChange))
}

/** @param view Editor. @param text Replacement for every match, applied as one undo step. @returns How many were replaced. */
export function replaceAll(view: EditorView, text: string): number {
  const { matches } = currentSearch(view.state)
  if (matches.length === 0) return 0
  const transaction = view.state.tr
  for (const match of [...matches].reverse()) transaction.insertText(text, match.from, match.to)
  view.dispatch(transaction)
  return matches.length
}

/** @param view Editor. Select the highlighted match, so the editor continues from it once the find bar closes. */
export function selectCurrent(view: EditorView): void {
  const { matches, current } = currentSearch(view.state)
  const match = matches[current]
  if (match !== undefined) view.dispatch(view.state.tr.setSelection(TextSelection.create(view.state.doc, match.from, match.to)))
}

/**
 * Insert a block after the text block holding the cursor, or in place of it when it is an empty paragraph, like Typora.
 * @param state Editor state.
 * @param dispatch Applies the change.
 * @param node Block to insert.
 * @returns Whether it was inserted; not inside a table or code block.
 */
function insertBlock(state: EditorState, dispatch: ((transaction: Transaction) => void) | undefined, node: ProseNode): boolean {
  const { $from } = state.selection
  for (let depth = $from.depth; depth > 0; depth--) {
    const name = $from.node(depth).type.name
    if (name === 'table' || name === 'code_block') return false
  }
  if (!$from.parent.isTextblock) return false
  const empty = $from.parent.type.name === 'paragraph' && $from.parent.content.size === 0
  const start = empty ? $from.before() : $from.after()
  const transaction = empty ? state.tr.replaceWith($from.before(), $from.after(), node) : state.tr.insert(start, node)
  dispatch?.(transaction.setSelection(TextSelection.near(transaction.doc.resolve(start + 1))).scrollIntoView())
  return true
}

/** The / menu's image icon, in the style of Crepe's others. */
export const IMAGE_ICON = '<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24"><path d="M5 21q-.825 0-1.412-.587T3 19V5q0-.825.588-1.412T5 3h14q.825 0 1.413.588T21 5v14q0 .825-.587 1.413T19 21zm0-2h14V5H5zm1-2h12l-3.75-5-3 4L9 13zm-1 2V5z"/></svg>'

/**
 * Insert an inline image without a target at the cursor; the editor then asks for a link or a file, as Typora's
 * Ctrl+Shift+I does. Crepe's image block stays off (see RichMarkdown.tsx).
 * @param ctx Editor context.
 * @returns Whether it was inserted.
 */
export function insertImage(ctx: Ctx): boolean {
  const view = ctx.get(editorViewCtx)
  const image = view.state.schema.nodes.image
  if (image === undefined || view.state.selection.$from.parent.type.spec.code === true) return false
  view.dispatch(view.state.tr.replaceSelectionWith(image.create({ src: '' })).scrollIntoView())
  return true
}

/** Typora's paragraph and format shortcuts, on top of Milkdown's own (Ctrl+B, Ctrl+I, Tab in lists…). */
export const typoraKeymap = $prose((ctx) => {
  const call = <T,>(key: CmdKey<T>, payload?: T): Command => () => ctx.get(commandsCtx).call(key, payload)
  const heading = (level: number): Command => () => ctx.get(commandsCtx).call(wrapInHeadingCommand.key, level)
  /** Ctrl+= makes a heading one level larger (a paragraph becomes a level 6 heading); Ctrl+- one level smaller. */
  const shift = (delta: number): Command => (state) => {
    const parent = state.selection.$from.parent
    const level = parent.type.name === 'heading' ? Number(parent.attrs.level) : parent.type.name === 'paragraph' ? 7 : undefined
    if (level === undefined) return false
    const next = Math.min(7, Math.max(1, level + delta))
    return next === level ? true : ctx.get(commandsCtx).call(wrapInHeadingCommand.key, next === 7 ? 0 : next)
  }
  const block = (make: () => ProseNode): Command => (state, dispatch) => insertBlock(state, dispatch, make())
  return keymap({
    'Mod-1': heading(1), 'Mod-2': heading(2), 'Mod-3': heading(3), 'Mod-4': heading(4), 'Mod-5': heading(5), 'Mod-6': heading(6),
    'Mod-0': heading(0),
    'Mod-=': shift(-1), 'Mod--': shift(1),
    'Mod-Shift-k': call(createCodeBlockCommand.key),
    'Mod-Shift-m': block(() => codeBlockSchema.type(ctx).create({ language: 'LaTeX' })),
    'Mod-t': block(() => createTable(ctx, 3, 3)),
    'Mod-Shift-q': call(wrapInBlockquoteCommand.key),
    'Mod-Shift-[': call(wrapInOrderedListCommand.key),
    'Mod-Shift-]': call(wrapInBulletListCommand.key),
    'Alt-Shift-5': call(toggleStrikethroughCommand.key),
    'Mod-Shift-`': call(toggleInlineCodeCommand.key),
    'Mod-k': call(toggleLinkCommand.key),
    'Mod-Shift-i': () => insertImage(ctx),
  })
})

/**
 * The cell holding the cursor: its table's position and its row and column.
 * @param state Editor state.
 * @returns `undefined` outside a table.
 */
function tableCell(state: EditorState): { table: number; node: ProseNode; row: number; column: number } | undefined {
  const { $head } = state.selection
  for (let depth = $head.depth; depth > 2; depth--) {
    if (!['table_cell', 'table_header'].includes($head.node(depth).type.name)) continue
    if ($head.node(depth - 2).type.name !== 'table') return undefined
    return { table: $head.before(depth - 2), node: $head.node(depth - 2), row: $head.index(depth - 2), column: $head.index(depth - 1) }
  }
  return undefined
}

/** @returns A transaction that puts the cursor at the end of cell (`row`, `column`) of the table at `table`, if it exists. */
function enterCell(state: EditorState, table: number, row: number, column: number): Transaction | undefined {
  const node = state.doc.nodeAt(table)
  if (node === null || row >= node.childCount || column >= node.child(row).childCount) return undefined
  let position = table + 1
  for (let index = 0; index < row; index++) position += node.child(index).nodeSize
  position += 1
  for (let index = 0; index < column; index++) position += node.child(row).child(index).nodeSize
  const end = position + node.child(row).child(column).nodeSize - 1
  return state.tr.setSelection(TextSelection.near(state.doc.resolve(end), -1)).scrollIntoView()
}

/**
 * Typora's Enter in tables: Enter goes to the cell below (a new row after the last), Ctrl+Enter adds a row below and
 * goes there. Milkdown's own Enter leaves the table.
 */
export const tableKeys = $useKeymap('rainyTable', {
  NextRow: {
    shortcuts: 'Enter',
    priority: 200,
    command: ctx => (state, dispatch) => {
      const cell = tableCell(state)
      if (cell === undefined) return false
      if (dispatch === undefined) return true
      if (cell.row + 1 >= cell.node.childCount && !ctx.get(commandsCtx).call(addRowAfterCommand.key)) return false
      const view = ctx.get(editorViewCtx)
      const moved = enterCell(view.state, cell.table, cell.row + 1, cell.column)
      if (moved !== undefined) view.dispatch(moved)
      return true
    },
  },
  AddRow: {
    shortcuts: 'Mod-Enter',
    priority: 200,
    command: ctx => (state, dispatch) => {
      const cell = tableCell(state)
      if (cell === undefined) return false
      if (dispatch === undefined) return true
      if (!ctx.get(commandsCtx).call(addRowAfterCommand.key)) return false
      const view = ctx.get(editorViewCtx)
      const moved = enterCell(view.state, cell.table, cell.row + 1, cell.column)
      if (moved !== undefined) view.dispatch(moved)
      return true
    },
  },
})

/** Marks the top-level block holding the cursor; focus mode fades the others. */
export const currentBlockPlugin = $prose(() => new Plugin({
  props: {
    decorations(state) {
      const { $head } = state.selection
      if ($head.depth < 1) return null
      const start = $head.before(1)
      const node = state.doc.child($head.index(0))
      return DecorationSet.create(state.doc, [Decoration.node(start, start + node.nodeSize, { class: 'rainy-current-block' })])
    },
  },
}))

/** Typewriter mode: the line being written stays in the middle of the view. */
export const typewriterPlugin = $prose(() => new Plugin({
  view: () => ({
    update(view, previous) {
      const stage = view.dom.closest<HTMLElement>('[data-rich-stage]')
      if (stage === null || stage.dataset.typewriter !== 'true' || !view.hasFocus()) return
      if (previous.selection.eq(view.state.selection) && previous.doc.eq(view.state.doc)) return
      let caret: { top: number; bottom: number }
      try { caret = view.coordsAtPos(view.state.selection.head) } catch (_hidden) { return } // A position inside a node view has no coordinates.
      const frame = stage.getBoundingClientRect()
      const offset = (caret.top + caret.bottom) / 2 - (frame.top + frame.height / 2)
      if (Math.abs(offset) > 2) stage.scrollTop += offset
    },
  }),
}))

/**
 * Ctrl+click on a link opens it: web links in the browser, files in Rainy, `#heading` in this document.
 * @param open Receives the link target as written.
 * @returns The plugin.
 */
export function linkClickPlugin(open: (href: string) => void): ReturnType<typeof $prose> {
  return $prose(() => new Plugin({
    props: {
      handleClick(view, position, event) {
        if (!(event.ctrlKey || event.metaKey) || event.button !== 0) return false
        const anchor = event.target instanceof Element ? event.target.closest('a') : null
        const resolved = view.state.doc.resolve(position)
        const mark = [...resolved.marks(), ...resolved.nodeAfter?.marks ?? []].find(entry => entry.type.name === 'link')
        const href = anchor?.getAttribute('href') ?? (typeof mark?.attrs.href === 'string' ? mark.attrs.href : '')
        if (href === null || href === '') return false
        event.preventDefault()
        open(href)
        return true
      },
    },
  }))
}

/** How Markdown is written back: the common GitHub style, so an edit does not rewrite the rest of the file. */
export const STRINGIFY = { bullet: '-', bulletOther: '*', bulletOrdered: '.', emphasis: '*', strong: '*', fence: '`', fences: true, rule: '-',
  listItemIndent: 'one', incrementListMarker: true } as const

/**
 * Write back the list markers and heading style the document itself uses, so an edited list or heading keeps them.
 * @param markdown Document text.
 * @returns Overrides of {@link STRINGIFY}.
 */
export function documentStyle(markdown: string): { bullet?: '-' | '*' | '+'; bulletOther?: '-' | '*' | '+'; bulletOrdered?: '.' | ')'; setext?: boolean } {
  const counts = new Map<string, number>()
  let fence: string | undefined
  let previous = ''
  for (const line of markdown.split('\n')) {
    const above = previous
    previous = line
    // Level 1 headings underlined with `=` against `#` and `##` ones; a `---` underline could be a rule.
    if (/^ {0,3}=+[ \t]*$/u.test(line) && above.trim() !== '' && fence === undefined) counts.set('setext', (counts.get('setext') ?? 0) + 1)
    if (/^ {0,3}#{1,2}(?:[ \t]|$)/u.test(line) && fence === undefined) counts.set('atx', (counts.get('atx') ?? 0) + 1)
    const opening = /^\s{0,3}(`{3,}|~{3,})/u.exec(line)?.[1]
    if (opening !== undefined) {
      if (fence === undefined) fence = opening
      else if (opening[0] === fence[0] && opening.length >= fence.length) fence = undefined
      continue
    }
    if (fence !== undefined) continue
    const marker = /^[ \t>]*(?:([-*+])|\d{1,9}([.)]))[ \t]+\S/u.exec(line)
    if (marker === null) continue
    // A `***` or `- - -` line is a rule, not a list.
    if (marker[1] !== undefined && /^[ \t>]*([-*_])(?:[ \t]*\1){2,}[ \t]*$/u.test(line)) continue
    const key = marker[1] ?? `ordered${marker[2]!}`
    counts.set(key, (counts.get(key) ?? 0) + 1)
  }
  const bullets = (['-', '*', '+'] as const).filter(bullet => (counts.get(bullet) ?? 0) > 0).sort((left, right) => counts.get(right)! - counts.get(left)!)
  const style: ReturnType<typeof documentStyle> = {}
  const bullet = bullets[0]
  if (bullet !== undefined && bullet !== '-') { style.bullet = bullet; style.bulletOther = bullets[1] ?? (bullet === '*' ? '-' : '*') }
  if ((counts.get('ordered)') ?? 0) > (counts.get('ordered.') ?? 0)) style.bulletOrdered = ')'
  if ((counts.get('setext') ?? 0) > (counts.get('atx') ?? 0)) style.setext = true
  return style
}

/**
 * Remove escapes the serializer adds that the file does not need: with single tildes not being strikethrough, a lone `~`
 * (as in `~/path`) is plain text; `=` only needs one where its line could underline a heading.
 * @param text Serialized text of one text node.
 * @returns The text with those escapes removed.
 */
export function unneededEscapes(text: string): string {
  return text.replace(/(?<![~\\])\\~(?!~|\\~)/gu, '~')
    .replace(/(^|\n)\\=(?=[^\n]*[^=\s])/gu, '$1=')
}

/** Text nodes keep a lone `~` and a leading `=` as written; the editor's own text handler does the rest. */
function textHandler(previous: Handle | undefined): Handle {
  return (node, parent, state, info) => unneededEscapes(previous === undefined ? state.safe((node as { value: string }).value, info) : previous(node, parent, state, info))
}

/**
 * A fenced ```latex block stays a code block. Crepe writes every code block whose language is `latex` as `$$` math,
 * and reads `$$` math as a code block with the language `LaTeX`; a fence written `LaTeX` is read as `latex`, so only
 * math has that exact spelling.
 */
export const latexFences = $remark('rainyLatexFences', () => () => (tree: unknown) => {
  const visit = (node: { type?: string; lang?: string | null; position?: unknown; children?: unknown[] }): void => {
    // Crepe's math block conversion makes code nodes without a position.
    if (node.type === 'code' && node.position !== undefined && node.lang === 'LaTeX') node.lang = 'latex'
    for (const child of node.children ?? []) visit(child as typeof node)
  }
  visit(tree as Parameters<typeof visit>[0])
})

/** Code blocks write `$$` math only for the language `LaTeX` (see {@link latexFences}). */
export const codeBlockWriter = codeBlockSchema.extendSchema(previous => (ctx) => {
  const base = previous(ctx)
  return {
    ...base,
    toMarkdown: {
      match: base.toMarkdown.match,
      runner: (state, node) => {
        if (node.attrs.language === MATH_LANGUAGE) state.addNode('math', undefined, node.content.firstChild?.text ?? '')
        else base.toMarkdown.runner(state, node)
      },
    },
  }
})

/** The language of a code block that is a `$$` formula. */
export const MATH_LANGUAGE = 'LaTeX'

/** Dollar amounts stay text (see markdown-math.ts). */
export const dollarAmounts = $remark('rainyDollarAmounts', () => remarkDollarAmounts)

/**
 * The Markdown dialect of the rich editor: how it reads and writes files. Shared with the tests.
 * @param editor Crepe's editor, before it is created.
 * @param markdown The document it opens, whose list markers it writes back.
 * @returns The editor, to configure further.
 */
export async function markdownSyntax(editor: Editor, markdown: string): Promise<Editor> {
  // Two Enters leave an empty paragraph, which Milkdown would write as a `<br />` line; Typora writes nothing.
  await editor.remove(remarkPreserveEmptyLinePlugin)
  return editor.config((ctx) => {
    ctx.update(remarkStringifyOptionsCtx, previous => ({ ...previous, ...STRINGIFY, ...documentStyle(markdown),
      handlers: { ...previous.handlers, text: textHandler(previous.handlers?.text) } }))
    // Aligned table columns would turn one edited cell into a change of every row; `H~2~O` is Typora's subscript, not
    // strikethrough, and was written back as `H~~2~~O`.
    ctx.set(remarkGFMPlugin.options.key, { tablePipeAlign: false, singleTilde: false })
  }).use(latexFences).use(codeBlockWriter).use(dollarAmounts)
}

/**
 * @param ctx The editor's context, once created.
 * @returns Its parser and serializer, as the merge reads them.
 */
export function markdownReader(ctx: Ctx): MarkdownReader {
  return {
    canonical: text => conventionalTables(ctx.get(serializerCtx)(ctx.get(parserCtx)(text.replace(/\r\n?/gu, '\n')))),
    ranges: (text) => {
      const tree = ctx.get(remarkCtx).parse(text) as { children: { position?: { start: { offset?: number }; end: { offset?: number } } }[] }
      return tree.children.flatMap(({ position }) => position?.start.offset === undefined || position.end.offset === undefined ? []
        : [[position.start.offset, position.end.offset] as const])
    },
  }
}
