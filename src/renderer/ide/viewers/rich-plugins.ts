/** ProseMirror plugins of the rich Markdown editor: Typora's shortcuts, find and replace, focus and typewriter modes, and
 *  Ctrl+click on links. Loaded with the editor, outside the viewer chunk. */
import { commandsCtx, type CmdKey } from '@milkdown/kit/core'
import { toggleLinkCommand } from '@milkdown/kit/component/link-tooltip'
import type { Node as ProseNode } from '@milkdown/kit/prose/model'
import { keymap } from '@milkdown/kit/prose/keymap'
import { Plugin, PluginKey, TextSelection, type Command, type EditorState, type Transaction } from '@milkdown/kit/prose/state'
import { Decoration, DecorationSet, type EditorView } from '@milkdown/kit/prose/view'
import { codeBlockSchema, createCodeBlockCommand, toggleInlineCodeCommand, wrapInBlockquoteCommand, wrapInBulletListCommand,
  wrapInHeadingCommand, wrapInOrderedListCommand } from '@milkdown/kit/preset/commonmark'
import { createTable, toggleStrikethroughCommand } from '@milkdown/kit/preset/gfm'
import { $prose } from '@milkdown/kit/utils'

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
  })
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
