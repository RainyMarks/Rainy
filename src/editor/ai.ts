/**
 * Editor AI: ghost-text suggestions while typing (Tab accepts) and rewriting a selection for an instruction (Ctrl+K),
 * shown in place as removed and added lines until the user accepts or rejects it. The page supplies the model calls.
 */
import { Prec, StateEffect, StateField, type EditorState, type Extension, type Transaction } from '@codemirror/state'
import { Decoration, EditorView, keymap, ViewPlugin, WidgetType, type DecorationSet, type ViewUpdate } from '@codemirror/view'
import { completionStatus } from '@codemirror/autocomplete'
import type { EditorActionLabels } from '../renderer/ide/editor-types.ts'

/** Pause in typing before a suggestion is requested. */
const SUGGEST_DELAY_MS = 450

/** What the AI features need from the editor that owns the view. */
export interface AiHost {
  /** Whether suggestions are turned on. */
  suggestions(): boolean
  /** Whether the document can be changed: not read-only and not shown as a comparison. */
  editable(view: EditorView): boolean
  /** Whether Ctrl+K opens the prompt; documents with a preview keep it for the Ctrl+K V chord. */
  ctrlK(view: EditorView): boolean
  /** @returns A suggestion to insert at `pos`, or '' for none. */
  suggest(view: EditorView, pos: number, signal: AbortSignal): Promise<string>
  /** @returns The replacement of `[from, to)`; `progress` receives the text so far. Rejects with a message to show. */
  rewrite(view: EditorView, from: number, to: number, instruction: string, progress: (text: string) => void, signal: AbortSignal): Promise<string>
  labels(): EditorActionLabels
}

// ── Ghost-text suggestions ──

interface Suggestion { readonly from: number; readonly text: string }

const setSuggestion = StateEffect.define<Suggestion | null>()

class GhostText extends WidgetType {
  constructor(readonly text: string) { super() }
  override eq(other: GhostText): boolean { return other.text === this.text }
  override toDOM(): HTMLElement {
    const span = document.createElement('span')
    span.className = 'rainy-ghost-text'
    span.setAttribute('aria-hidden', 'true')
    span.textContent = this.text
    return span
  }
  override ignoreEvent(): boolean { return true }
}

/** Typing the start of a suggestion keeps the rest of it; any other change ends it. */
function advance(value: Suggestion, transaction: Transaction): Suggestion | null {
  let next: Suggestion | null = null
  let changes = 0
  transaction.changes.iterChanges((fromA, toA, _fromB, toB, inserted) => {
    changes++
    const typed = inserted.toString()
    if (fromA === value.from && toA === fromA && typed !== '' && typed.length < value.text.length && value.text.startsWith(typed)) {
      next = { from: toB, text: value.text.slice(typed.length) }
    }
  })
  const main = transaction.state.selection.main
  return changes === 1 && next !== null && main.empty && main.head === (next as Suggestion).from ? next : null
}

const suggestionField = StateField.define<Suggestion | null>({
  create: () => null,
  update(value, transaction) {
    for (const effect of transaction.effects) if (effect.is(setSuggestion)) return effect.value
    if (value === null) return null
    if (transaction.docChanged) return advance(value, transaction)
    if (transaction.selection !== undefined) {
      const main = transaction.state.selection.main
      return main.empty && main.head === value.from ? value : null
    }
    return value
  },
  provide: field => EditorView.decorations.from(field, value => value === null ? Decoration.none
    : Decoration.set([Decoration.widget({ widget: new GhostText(value.text), side: 1 }).range(value.from)])),
})

/** Insert the whole suggestion, or its next word. */
function acceptSuggestion(view: EditorView, part: 'all' | 'word'): boolean {
  const value = view.state.field(suggestionField, false)
  if (value === null || value === undefined) return false
  const text = part === 'all' ? value.text : /^(?:\s*[\w$]+|\s*[^\w\s]+|\s+)/u.exec(value.text)?.[0] ?? value.text
  view.dispatch({
    changes: { from: value.from, insert: text },
    selection: { anchor: value.from + text.length },
    userEvent: 'input.complete',
  })
  return true
}

function suggestionRequests(host: AiHost): Extension {
  return ViewPlugin.fromClass(class {
    private timer = 0
    private controller: AbortController | undefined

    constructor(private readonly view: EditorView) {}

    update(update: ViewUpdate): void {
      if (!update.docChanged && !update.selectionSet) return
      this.cancel()
      if (!update.docChanged || !host.suggestions()) return
      if (!update.transactions.some(transaction => transaction.isUserEvent('input') || transaction.isUserEvent('delete'))) return
      if (update.state.field(suggestionField) !== null) return
      this.timer = window.setTimeout(() => { this.request() }, SUGGEST_DELAY_MS)
    }

    request(): void {
      const view = this.view
      const state = view.state
      const main = state.selection.main
      if (!main.empty || state.selection.ranges.length > 1 || !host.editable(view) || completionStatus(state) === 'active') return
      if (state.field(aiEditField, false) !== null) return
      // Only where the rest of the line is closing punctuation, so a suggestion never splits code the user wrote.
      const rest = state.sliceDoc(main.head, state.doc.lineAt(main.head).to)
      if (!/^[\s)\]}>'"`;,:]*$/u.test(rest)) return
      const controller = new AbortController()
      this.controller = controller
      const doc = state.doc
      void host.suggest(view, main.head, controller.signal).then((text) => {
        if (controller.signal.aborted || text === '' || view.state.doc !== doc || view.state.selection.main.head !== main.head) return
        view.dispatch({ effects: setSuggestion.of({ from: main.head, text }) })
      }, () => undefined)
    }

    cancel(): void {
      window.clearTimeout(this.timer)
      this.controller?.abort()
      this.controller = undefined
    }

    destroy(): void { this.cancel() }
  })
}

// ── Rewriting a selection (Ctrl+K) ──

/** One rewrite, from the prompt until it is accepted, rejected or closed. */
interface AiEdit {
  readonly session: AiEditSession
  /** The region: the selection while prompting, the new text while it is reviewed. */
  readonly from: number
  readonly to: number
  /** Text the new text replaced, while it is reviewed. */
  readonly original: string | undefined
}

const setAiEdit = StateEffect.define<AiEdit | null>()

const addedLine = Decoration.line({ class: 'rainy-ai-added' })

class PromptWidget extends WidgetType {
  constructor(readonly session: AiEditSession) { super() }
  override eq(other: PromptWidget): boolean { return other.session === this.session }
  override toDOM(): HTMLElement { return this.session.dom }
  override ignoreEvent(): boolean { return true }
}

class RemovedLines extends WidgetType {
  constructor(readonly text: string) { super() }
  override eq(other: RemovedLines): boolean { return other.text === this.text }
  override toDOM(): HTMLElement {
    const block = document.createElement('div')
    block.className = 'rainy-ai-removed'
    block.setAttribute('aria-hidden', 'true')
    block.textContent = this.text
    return block
  }
  override ignoreEvent(): boolean { return true }
}

/** Lines of the new text, `[at, end)`, that replaced the `removed` lines of the original. */
interface LineHunk {
  readonly at: number
  readonly end: number
  readonly removed: readonly string[]
}

/** Above this many line pairs, the differing middle counts as one change instead of being compared line by line. */
const DIFF_LIMIT = 250_000

/**
 * Compare two texts line by line.
 * @returns The changed runs of lines, in order; empty when the texts are the same.
 */
export function lineHunks(original: string, text: string): LineHunk[] {
  const before = original.split('\n')
  const after = text.split('\n')
  let head = 0
  while (head < before.length && head < after.length && before[head] === after[head]) head++
  let tail = 0
  while (tail < before.length - head && tail < after.length - head && before[before.length - 1 - tail] === after[after.length - 1 - tail]) tail++
  const old = before.slice(head, before.length - tail)
  const next = after.slice(head, after.length - tail)
  if (old.length === 0 && next.length === 0) return []
  if (old.length === 0 || next.length === 0 || old.length * next.length > DIFF_LIMIT) return [{ at: head, end: head + next.length, removed: old }]
  // Longest common subsequence of lines, filled from the end so the walk below can go forwards.
  const width = next.length + 1
  const common = new Uint32Array((old.length + 1) * width)
  for (let i = old.length - 1; i >= 0; i--) {
    for (let j = next.length - 1; j >= 0; j--) {
      common[i * width + j] = old[i] === next[j] ? common[(i + 1) * width + j + 1]! + 1 : Math.max(common[(i + 1) * width + j]!, common[i * width + j + 1]!)
    }
  }
  const hunks: LineHunk[] = []
  let removed: string[] = []
  let start = -1
  const flush = (at: number): void => {
    if (start >= 0 || removed.length > 0) hunks.push({ at: head + (start >= 0 ? start : at), end: head + at, removed })
    removed = []
    start = -1
  }
  let i = 0
  let j = 0
  while (i < old.length || j < next.length) {
    if (i < old.length && j < next.length && old[i] === next[j]) { flush(j); i++; j++ }
    else if (j < next.length && (i === old.length || common[i * width + j + 1]! >= common[(i + 1) * width + j]!)) { if (start < 0) start = j; j++ }
    else removed.push(old[i++]!)
  }
  flush(j)
  return hunks
}

function aiEditDecorations(value: AiEdit, state: EditorState): DecorationSet {
  const ranges = [Decoration.widget({ widget: new PromptWidget(value.session), block: true, side: -2 }).range(state.doc.lineAt(value.from).from)]
  if (value.original === '') {
    // Inserted at the cursor: every line it touches is new.
    for (let line = state.doc.lineAt(value.from).number; line <= state.doc.lineAt(value.to).number && value.to > value.from; line++) ranges.push(addedLine.range(state.doc.line(line).from))
  } else if (value.original !== undefined) {
    // Whole lines were replaced: mark only the lines that changed, with what they replaced shown above them.
    const first = state.doc.lineAt(value.from).number
    const last = state.doc.lineAt(value.to).number
    for (const hunk of lineHunks(value.original, state.sliceDoc(value.from, value.to))) {
      if (hunk.removed.length > 0) {
        // Removed lines go above the line now in their place, or below the region when they were its last lines.
        const widget = new RemovedLines(hunk.removed.join('\n'))
        ranges.push(first + hunk.at <= last
          ? Decoration.widget({ widget, block: true, side: -1 }).range(state.doc.line(first + hunk.at).from)
          : Decoration.widget({ widget, block: true, side: 1 }).range(value.to))
      }
      for (let index = hunk.at; index < hunk.end; index++) ranges.push(addedLine.range(state.doc.line(first + index).from))
    }
  }
  return Decoration.set(ranges, true)
}

const aiEditField = StateField.define<AiEdit | null>({
  create: () => null,
  update(value, transaction) {
    for (const effect of transaction.effects) if (effect.is(setAiEdit)) return effect.value
    if (value === null || !transaction.docChanged) return value
    const from = transaction.changes.mapPos(value.from, -1)
    return { ...value, from, to: Math.max(from, transaction.changes.mapPos(value.to, 1)) }
  },
  // Block widgets change the layout, so the decorations come straight from the state rather than from a view function.
  provide: field => EditorView.decorations.compute([field], (state) => {
    const value = state.field(field)
    return value === null ? Decoration.none : aiEditDecorations(value, state)
  }),
})

/** The prompt box above the region and the request it runs. */
class AiEditSession {
  readonly dom = document.createElement('div')
  private readonly card = document.createElement('div')
  private readonly input = document.createElement('input')
  private readonly status = document.createElement('div')
  private readonly preview = document.createElement('pre')
  private readonly actions = document.createElement('div')
  private controller: AbortController | undefined

  constructor(private readonly view: EditorView, private readonly host: AiHost) {
    const labels = host.labels()
    // Block widgets must not have vertical margins (the gutter would misalign), so the spacing is the wrapper's padding.
    this.dom.className = 'rainy-ai-prompt'
    this.card.className = 'rainy-ai-card'
    this.input.className = 'rainy-ai-input'
    this.input.placeholder = labels.aiEditPlaceholder
    this.input.setAttribute('aria-label', labels.aiEdit)
    this.input.spellcheck = false
    this.status.className = 'rainy-ai-status'
    this.status.setAttribute('role', 'status')
    this.preview.className = 'rainy-ai-preview'
    this.preview.hidden = true
    this.actions.className = 'rainy-ai-actions'
    this.card.append(this.input, this.actions, this.status, this.preview)
    this.dom.append(this.card)
    this.input.addEventListener('keydown', (event) => {
      if (event.key === 'Enter' && (event.ctrlKey || event.metaKey)) { event.preventDefault(); this.accept() }
      else if (event.key === 'Enter') { event.preventDefault(); void this.generate() }
      else if (event.key === 'Escape') { event.preventDefault(); this.dismiss() }
      event.stopPropagation()
    })
    // Clicks inside the box stay in it instead of moving the editor's cursor.
    this.dom.addEventListener('mousedown', (event) => { event.stopPropagation() })
    this.renderActions()
  }

  focus(): void {
    requestAnimationFrame(() => { this.input.focus() })
  }

  private current(): AiEdit | null {
    const value = this.view.state.field(aiEditField, false) ?? null
    return value?.session === this ? value : null
  }

  private renderActions(): void {
    const labels = this.host.labels()
    const reviewing = this.current()?.original !== undefined
    const button = (label: string, run: () => void, primary = false): HTMLButtonElement => {
      const element = document.createElement('button')
      element.type = 'button'
      element.className = primary ? 'rainy-ai-button rainy-ai-primary' : 'rainy-ai-button'
      element.textContent = label
      element.addEventListener('click', run)
      return element
    }
    this.actions.replaceChildren(...this.controller !== undefined
      ? [button(labels.aiEditStop, () => { this.controller?.abort() })]
      : reviewing
        ? [button(labels.aiEditAccept, () => { this.accept() }, true), button(labels.aiEditReject, () => { this.reject() })]
        : [button(labels.aiEditGenerate, () => { void this.generate() }, true)])
  }

  private setStatus(text: string, error = false): void {
    this.status.textContent = text
    this.status.classList.toggle('rainy-ai-error', error)
  }

  /** Generate, or generate again from the original text with the current instruction. */
  async generate(): Promise<void> {
    const instruction = this.input.value.trim()
    if (instruction === '' || this.controller !== undefined) return
    let value = this.current()
    if (value === null) return
    if (value.original !== undefined) {
      // A new instruction starts over from the text the last result replaced.
      this.view.dispatch({ changes: { from: value.from, to: value.to, insert: value.original }, userEvent: 'input.ai' })
      value = this.current()
      if (value === null) return
      this.view.dispatch({ effects: setAiEdit.of({ session: this, from: value.from, to: value.from + (value.original ?? '').length, original: undefined }) })
      value = this.current()
      if (value === null) return
    }
    const { from, to } = value
    const labels = this.host.labels()
    const controller = new AbortController()
    this.controller = controller
    this.setStatus(labels.aiEditGenerating)
    this.preview.textContent = ''
    this.preview.hidden = true
    this.renderActions()
    try {
      const text = await this.host.rewrite(this.view, from, to, instruction, (partial) => {
        if (controller.signal.aborted) return
        this.preview.textContent = partial
        this.preview.hidden = partial === ''
        this.preview.scrollTop = this.preview.scrollHeight
      }, controller.signal)
      if (controller.signal.aborted) return
      const latest = this.current()
      if (latest === null) return
      const original = this.view.state.sliceDoc(latest.from, latest.to)
      this.view.dispatch({
        changes: { from: latest.from, to: latest.to, insert: text },
        effects: setAiEdit.of({ session: this, from: latest.from, to: latest.from + text.length, original }),
        userEvent: 'input.ai',
      })
      this.setStatus(text === original ? labels.aiEditUnchanged : labels.aiEditReview)
    } catch (error) {
      this.setStatus(controller.signal.aborted ? '' : error instanceof Error ? error.message : String(error), !controller.signal.aborted)
    } finally {
      if (this.controller === controller) this.controller = undefined
      this.preview.hidden = true
      this.renderActions()
      if (this.current() !== null) this.input.focus()
    }
  }

  /** Keep the new text. */
  accept(): void {
    const value = this.current()
    if (value === null || this.controller !== undefined) return
    if (value.original === undefined) { this.close(); return }
    this.close(value.to)
  }

  /** Put the original text back. */
  reject(): void {
    const value = this.current()
    if (value === null) return
    this.controller?.abort()
    if (value.original === undefined) { this.close(); return }
    this.view.dispatch({ changes: { from: value.from, to: value.to, insert: value.original }, userEvent: 'input.ai' })
    this.close()
  }

  /** Escape: stop a running request, otherwise reject a result or close the prompt. */
  dismiss(): void {
    if (this.controller !== undefined) { this.controller.abort(); return }
    this.reject()
  }

  close(cursor?: number): void {
    this.controller?.abort()
    if (this.current() === null) return
    this.view.dispatch({ effects: setAiEdit.of(null), ...(cursor === undefined ? {} : { selection: { anchor: cursor } }) })
    this.view.focus()
  }

  refreshActions(): void { this.renderActions() }
}

/** @returns The region Ctrl+K rewrites: the selected lines, or the cursor to insert there. */
function regionOf(state: EditorState): { from: number; to: number } {
  const main = state.selection.main
  if (main.empty) return { from: main.head, to: main.head }
  const first = state.doc.lineAt(main.from)
  const end = main.to > main.from && state.doc.lineAt(main.to).from === main.to ? main.to - 1 : main.to
  return { from: first.from, to: state.doc.lineAt(end).to }
}

/**
 * Open the Ctrl+K prompt over the selection, or move focus to the one already open.
 * @returns Whether the command applied.
 */
export function startAiEdit(view: EditorView, host: AiHost): boolean {
  const existing = view.state.field(aiEditField, false)
  if (existing === undefined) return false
  if (existing !== null) { existing.session.focus(); return true }
  if (!host.editable(view)) return false
  const session = new AiEditSession(view, host)
  const { from, to } = regionOf(view.state)
  view.dispatch({ effects: [setAiEdit.of({ session, from, to, original: undefined }), setSuggestion.of(null)] })
  session.focus()
  return true
}

/** Accept or reject a reviewed rewrite from the editor itself. */
function reviewKey(action: 'accept' | 'reject') {
  return (view: EditorView): boolean => {
    const value = view.state.field(aiEditField, false)
    if (value === null || value === undefined || value.original === undefined) return false
    if (action === 'accept') value.session.accept()
    else value.session.reject()
    return true
  }
}

/**
 * @param host Model calls and settings.
 * @returns The suggestion and rewrite extensions of one view.
 */
export function aiExtensions(host: AiHost): Extension {
  return [
    suggestionField,
    aiEditField,
    suggestionRequests(host),
    Prec.highest(keymap.of([
      { key: 'Tab', run: view => acceptSuggestion(view, 'all') },
      { key: 'Mod-ArrowRight', run: view => acceptSuggestion(view, 'word') },
      { key: 'Escape', run: (view) => { if (view.state.field(suggestionField) === null) return false; view.dispatch({ effects: setSuggestion.of(null) }); return true } },
      { key: 'Mod-k', run: view => host.ctrlK(view) && startAiEdit(view, host) },
      { key: 'Mod-Enter', run: reviewKey('accept') },
      { key: 'Mod-Backspace', run: reviewKey('reject') },
    ])),
    // The buttons depend on whether the session reviews a result.
    EditorView.updateListener.of((update) => {
      const value = update.state.field(aiEditField)
      if (value !== update.startState.field(aiEditField) && value !== null) value.session.refreshActions()
    }),
  ]
}

/** @returns Whether a suggestion is showing; for tests. */
export function suggestionOf(state: EditorState): string | undefined {
  return state.field(suggestionField, false)?.text
}
