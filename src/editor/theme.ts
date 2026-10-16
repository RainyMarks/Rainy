/** Editor colors drawn from the application tokens, so light and dark follow the window theme. */
import { HighlightStyle, syntaxHighlighting } from '@codemirror/language'
import { EditorView } from '@codemirror/view'
import { tags } from '@lezer/highlight'
import type { Extension } from '@codemirror/state'

/** Chrome: gutters, selection, panels, tooltips and search matches. */
export const editorTheme: Extension = EditorView.theme({
  '&': {
    height: '100%',
    color: 'var(--rainy-editor-foreground, var(--rainy-alias-label-primary))',
    backgroundColor: 'var(--rainy-editor-background, var(--rainy-alias-bg-base))',
    fontSize: 'var(--rainy-editor-font-size, 13px)',
  },
  '&.cm-focused': { outline: 'none' },
  '.cm-scroller': { fontFamily: 'var(--rainy-font-family-code)', lineHeight: '1.55' },
  '.cm-content': { padding: '8px 0', caretColor: 'var(--rainy-alias-label-primary)' },
  '.cm-cursor, .cm-dropCursor': { borderLeftColor: 'var(--rainy-alias-label-primary)' },
  '&.cm-focused > .cm-scroller > .cm-selectionLayer .cm-selectionBackground, .cm-selectionBackground, .cm-content ::selection': {
    backgroundColor: 'color-mix(in srgb, var(--hl-constant) 22%, transparent)',
  },
  '.cm-activeLine': { backgroundColor: 'color-mix(in srgb, var(--rainy-alias-label-primary) 4%, transparent)' },
  '.cm-gutters': {
    backgroundColor: 'var(--rainy-editor-background, var(--rainy-alias-bg-base))',
    color: 'var(--rainy-alias-label-tertiary)',
    border: 'none',
  },
  '.cm-activeLineGutter': { backgroundColor: 'transparent', color: 'var(--rainy-alias-label-primary)' },
  '.cm-lineNumbers .cm-gutterElement': { padding: '0 8px 0 4px', minWidth: '32px' },
  '.cm-foldPlaceholder': { backgroundColor: 'var(--rainy-alias-bg-layer-2)', border: 'none', color: 'var(--rainy-alias-label-secondary)' },
  '.cm-matchingBracket': { backgroundColor: 'color-mix(in srgb, var(--hl-string) 22%, transparent)', outline: 'none' },
  '.cm-searchMatch': { backgroundColor: 'color-mix(in srgb, var(--rainy-alias-state-warn-primary) 30%, transparent)' },
  '.cm-searchMatch.cm-searchMatch-selected': { backgroundColor: 'color-mix(in srgb, var(--rainy-alias-state-warn-primary) 55%, transparent)' },
  '.cm-selectionMatch': { backgroundColor: 'color-mix(in srgb, var(--hl-constant) 12%, transparent)' },
  '.cm-panels': { backgroundColor: 'var(--rainy-alias-bg-layer-1)', color: 'var(--rainy-alias-label-primary)' },
  '.cm-panels.cm-panels-top': { borderBottom: '1px solid var(--rainy-alias-border-l2)' },
  '.cm-panels.cm-panels-bottom': { borderTop: '1px solid var(--rainy-alias-border-l2)' },
  '.cm-panel input, .cm-panel button, .cm-panel label': { fontSize: '12px' },
  '.cm-textfield': {
    border: '1px solid var(--rainy-alias-border-l2)', borderRadius: '4px', backgroundColor: 'var(--rainy-alias-bg-base)',
    color: 'var(--rainy-alias-label-primary)', padding: '2px 6px',
  },
  '.cm-button': {
    backgroundImage: 'none', backgroundColor: 'var(--rainy-alias-bg-layer-2)', border: '1px solid var(--rainy-alias-border-l2)',
    borderRadius: '4px', color: 'var(--rainy-alias-label-primary)',
  },
  '.cm-tooltip': {
    backgroundColor: 'var(--rainy-alias-bg-base)', color: 'var(--rainy-alias-label-primary)', border: 'none',
    borderRadius: '6px', boxShadow: 'var(--rainy-elevation-panel)',
  },
  '.cm-tooltip-autocomplete > ul > li[aria-selected]': {
    backgroundColor: 'var(--rainy-alias-interactive-bg-hover)', color: 'var(--rainy-alias-label-primary)',
  },
  '.cm-tooltip.cm-tooltip-hover, .cm-tooltip-lint': { maxWidth: '560px', maxHeight: '320px', overflow: 'auto' },
  '.cm-diagnostic': { padding: '4px 8px' },
  '.cm-diagnostic-error': { borderLeftColor: 'var(--rainy-alias-state-error-primary)' },
  '.cm-diagnostic-warning': { borderLeftColor: 'var(--rainy-alias-state-warn-primary)' },
  '.cm-changedLine, .cm-insertedLine': { backgroundColor: 'color-mix(in srgb, var(--rainy-alias-file-diff-added-marker) 10%, transparent) !important' },
  '.cm-deletedChunk': { backgroundColor: 'color-mix(in srgb, var(--rainy-alias-file-diff-deleted-marker) 10%, transparent)' },
  '.cm-changedText, .cm-insertedText': { background: 'color-mix(in srgb, var(--rainy-alias-file-diff-added-marker) 26%, transparent) !important' },
  '.cm-deletedText, .cm-deletedChunk del': { background: 'color-mix(in srgb, var(--rainy-alias-file-diff-deleted-marker) 26%, transparent)', textDecoration: 'none' },
})

/** Token colors; the palette is shared with markdown code fences (`src/renderer/theme/highlight.css`). */
export const editorHighlighting: Extension = syntaxHighlighting(HighlightStyle.define([
  { tag: [tags.comment, tags.lineComment, tags.blockComment, tags.docComment, tags.meta], color: 'var(--hl-comment)', fontStyle: 'italic' },
  { tag: [tags.keyword, tags.controlKeyword, tags.definitionKeyword, tags.moduleKeyword, tags.operatorKeyword, tags.modifier, tags.self], color: 'var(--hl-keyword)' },
  { tag: [tags.typeName, tags.className, tags.namespace, tags.standard(tags.typeName)], color: 'var(--hl-keyword)' },
  { tag: [tags.string, tags.special(tags.string), tags.regexp, tags.character, tags.inserted], color: 'var(--hl-string)' },
  { tag: [tags.number, tags.bool, tags.null, tags.atom, tags.constant(tags.name), tags.standard(tags.name), tags.escape], color: 'var(--hl-constant)' },
  { tag: [tags.function(tags.variableName), tags.function(tags.propertyName), tags.macroName, tags.labelName, tags.heading], color: 'var(--hl-function)' },
  { tag: [tags.propertyName, tags.attributeName, tags.definition(tags.variableName), tags.local(tags.variableName)], color: 'var(--hl-parameter)' },
  { tag: [tags.punctuation, tags.operator, tags.bracket, tags.separator], color: 'var(--hl-punctuation)' },
  { tag: [tags.link, tags.url], color: 'var(--hl-link)', textDecoration: 'underline' },
  { tag: tags.deleted, color: 'var(--rainy-alias-state-error-primary)' },
  { tag: tags.invalid, color: 'var(--rainy-alias-state-error-primary)' },
  { tag: tags.emphasis, fontStyle: 'italic' },
  { tag: tags.strong, fontWeight: '600' },
  { tag: tags.heading, fontWeight: '600' },
]))
