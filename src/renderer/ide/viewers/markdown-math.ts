/** Which `$…$` are formulas: the rule the native chat applies (`is_formula` in native/crates/app/src/ui/markdown.rs), so
 *  "价格 $5 到 $10" stays prose in the rich editor, the preview and exports as it does in replies. */

/**
 * As in Pandoc, single dollars hug the formula and are not followed by a digit; `$$…$$` is always a formula.
 * @param source The math as written, delimiters included.
 * @param value Its content between the delimiters.
 * @param after The character after the closing delimiter, if any.
 * @returns Whether it is a formula.
 */
export function isFormula(source: string, value: string, after: string | undefined): boolean {
  if (value.trim() === '' || (after !== undefined && /^\d/u.test(after))) return false
  return source.startsWith('$$') || !(/^\s/u.test(value) || /\s$/u.test(value))
}

/** The few fields of an mdast node the plugin reads and writes. */
interface MdastNode {
  type: string
  value?: string
  children?: MdastNode[]
  position?: { start: { offset?: number }; end: { offset?: number } }
}

/**
 * Remark plugin: inline math that fails {@link isFormula} becomes the text it was written as.
 * @returns The transformer; it needs the parsed text as the file, as `processor.runSync(tree, text)` gives it.
 */
export function remarkDollarAmounts() {
  return (tree: MdastNode, file: { value?: unknown }): void => {
    const text = typeof file.value === 'string' ? file.value : String(file.value ?? '')
    const visit = (node: MdastNode): void => {
      const children = node.children
      if (children === undefined) return
      for (let index = 0; index < children.length; index++) {
        const child = children[index]!
        if (child.type !== 'inlineMath') { visit(child); continue }
        const start = child.position?.start.offset
        const end = child.position?.end.offset
        if (start === undefined || end === undefined) continue
        const source = text.slice(start, end)
        if (isFormula(source, child.value ?? '', text[end])) continue
        children[index] = { type: 'text', value: source, position: child.position }
      }
    }
    visit(tree)
  }
}
