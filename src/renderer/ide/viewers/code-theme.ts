/** Code blocks of the rich Markdown editor in Rainy's colors: CodeMirror (inside Crepe) highlights with the `tok-*`
 *  classes RichMarkdown.css colors from the highlight tokens, so code follows light and dark mode and the scheme instead
 *  of Crepe's fixed One Dark. Loaded with the editor.
 *
 *  The CodeMirror packages are Crepe's own (see the aliases in vite.config.ts): one instance of each, as CodeMirror needs. */
import { syntaxHighlighting } from '@codemirror/language'
import { EditorState, Prec, type Extension } from '@codemirror/state'
import { EditorView, keymap } from '@codemirror/view'
import { classHighlighter } from '@lezer/highlight'
import type { useViewerT } from './messages.ts'

/** CodeMirror's own find, replace and go-to-line keys open English panels; the rich editor's find bar takes Ctrl+F. */
const SEARCH_KEYS = ['Mod-f', 'Mod-Alt-f', 'Mod-Alt-g', 'Mod-g', 'Shift-Mod-g', 'F3', 'Shift-F3', 'Mod-Shift-l']

/**
 * @param t Viewer strings, for CodeMirror's own words (fold markers).
 * @returns The extensions to pass as Crepe's CodeMirror theme.
 */
export function codeTheme(t: ReturnType<typeof useViewerT>): Extension {
  return [
    syntaxHighlighting(classHighlighter),
    // Handled, so CodeMirror stops; the key still reaches the editor page, which opens its find bar for Ctrl+F.
    Prec.highest(keymap.of(SEARCH_KEYS.map(key => ({ key, run: () => true })))),
    EditorState.phrases.of({
      'Fold line': t('codeFold'), 'Unfold line': t('codeUnfold'), 'folded code': t('codeFolded'), unfold: t('codeUnfold'),
      'Folded lines': t('codeFoldedLines'), 'Unfolded lines': t('codeUnfoldedLines'), to: t('codeLinesTo'),
      Completions: t('codeCompletions'),
    }),
    EditorView.theme({
      '&': { color: 'var(--hl-foreground)', backgroundColor: 'transparent' },
      '.cm-content': { caretColor: 'var(--rainy-alias-label-primary)' },
      '.cm-cursor, .cm-dropCursor': { borderLeftColor: 'var(--rainy-alias-label-primary)' },
      '&.cm-focused > .cm-scroller > .cm-selectionLayer .cm-selectionBackground, .cm-selectionBackground, .cm-content ::selection': {
        backgroundColor: 'color-mix(in srgb, var(--rainy-alias-state-business-primary) 28%, transparent)',
      },
      '.cm-gutters': { backgroundColor: 'transparent', color: 'var(--rainy-alias-label-tertiary, var(--rainy-alias-label-secondary))', border: 'none' },
      '.cm-activeLine, .cm-activeLineGutter': { backgroundColor: 'color-mix(in srgb, var(--rainy-alias-label-primary) 6%, transparent)' },
      '.cm-selectionMatch': { backgroundColor: 'color-mix(in srgb, var(--rainy-alias-state-warn-primary, #e5a50a) 25%, transparent)' },
      '&.cm-focused .cm-matchingBracket': { backgroundColor: 'color-mix(in srgb, var(--rainy-alias-state-business-primary) 22%, transparent)', outline: 'none' },
      '.cm-foldPlaceholder': { backgroundColor: 'var(--rainy-alias-interactive-bg-hover)', border: 'none', color: 'var(--rainy-alias-label-secondary)' },
      '.cm-tooltip': { backgroundColor: 'var(--rainy-alias-bg-base)', color: 'var(--rainy-alias-label-primary)', border: '1px solid var(--rainy-alias-border-l3)' },
      '.cm-tooltip-autocomplete > ul > li[aria-selected]': {
        backgroundColor: 'color-mix(in srgb, var(--rainy-alias-state-business-primary) 22%, transparent)', color: 'var(--rainy-alias-label-primary)',
      },
    }),
  ]
}
