/** Minimal DOM driving for renderer tests: render through `act`, role queries, and user events. */
import { act, type ReactElement } from 'react'
import { createRoot, type Root } from 'react-dom/client'

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true

const mounted = new Set<{ root: Root; container: HTMLElement }>()

/** A rendered tree. */
export interface Rendered {
  readonly container: HTMLElement
  rerender(element: ReactElement): void
  unmount(): void
}

/**
 * Mount an element into a fresh container.
 * @param element Element to render.
 * @returns Handle for rerendering and unmounting.
 */
export function render(element: ReactElement): Rendered {
  const container = document.createElement('div')
  document.body.append(container)
  const root = createRoot(container)
  const entry = { root, container }
  mounted.add(entry)
  act(() => { root.render(element) })
  return {
    container,
    rerender: (next) => { act(() => { root.render(next) }) },
    unmount: () => {
      if (!mounted.delete(entry)) return
      act(() => { root.unmount() })
      container.remove()
    },
  }
}

/** Unmount everything rendered by {@link render} and clear the body. */
export function cleanup(): void {
  for (const entry of [...mounted]) {
    mounted.delete(entry)
    act(() => { entry.root.unmount() })
    entry.container.remove()
  }
  document.body.replaceChildren()
}

const implicitRoles: Readonly<Record<string, string>> = {
  BUTTON: 'button', TEXTAREA: 'textbox', UL: 'list', OL: 'list', LI: 'listitem', ASIDE: 'complementary', MAIN: 'main',
  PROGRESS: 'progressbar', H1: 'heading', H2: 'heading', H3: 'heading', H4: 'heading', NAV: 'navigation', SELECT: 'combobox',
}

function roleOf(element: Element): string | undefined {
  const explicit = element.getAttribute('role')
  if (explicit !== null) return explicit
  if (element.tagName === 'INPUT') {
    const type = (element as HTMLInputElement).type
    return type === 'checkbox' ? 'checkbox' : type === 'radio' ? 'radio' : 'textbox'
  }
  if (element.tagName === 'SECTION') return element.hasAttribute('aria-label') ? 'region' : undefined
  return implicitRoles[element.tagName]
}

/**
 * @param element Element.
 * @returns Its accessible name: `aria-label`, a label element's text, or its own text.
 */
export function nameOf(element: Element): string {
  const label = element.getAttribute('aria-label')
  if (label !== null) return label
  const labelledBy = element.getAttribute('aria-labelledby')
  if (labelledBy !== null) return labelledBy.split(' ').map(id => document.getElementById(id)?.textContent ?? '').join(' ').trim()
  if (element instanceof HTMLInputElement || element instanceof HTMLTextAreaElement) {
    const wrapping = element.closest('label')
    if (wrapping !== null) return [...wrapping.childNodes].filter(node => node.nodeType === Node.TEXT_NODE).map(node => node.textContent).join('').trim()
    return ''
  }
  return (element.textContent ?? '').trim()
}

function visible(element: Element): boolean {
  return element.closest('[hidden]') === null
}

/**
 * Find elements by role and optional accessible name, skipping hidden subtrees.
 * @param role ARIA role.
 * @param name Exact name or pattern.
 * @param within Search root; defaults to the body.
 * @returns Matches in document order.
 */
export function allByRole(role: string, name?: string | RegExp, within: ParentNode = document.body): HTMLElement[] {
  return [...within.querySelectorAll<HTMLElement>('*')].filter(element => roleOf(element) === role && visible(element)
    && (name === undefined || (typeof name === 'string' ? nameOf(element) === name : name.test(nameOf(element)))))
}

/** @returns The single match, or `null`. Throws when several match. */
export function queryByRole(role: string, name?: string | RegExp, within?: ParentNode): HTMLElement | null {
  const found = allByRole(role, name, within)
  if (found.length > 1) throw new Error(`Several ${role} elements named ${String(name)}`)
  return found[0] ?? null
}

/** @returns The single match. Throws when there is none or several. */
export function byRole(role: string, name?: string | RegExp, within?: ParentNode): HTMLElement {
  const found = queryByRole(role, name, within)
  if (found === null) {
    const names = allByRole(role, undefined, within).map(nameOf)
    throw new Error(`No ${role} named ${String(name)}; found: ${JSON.stringify(names)}`)
  }
  return found
}

/** @returns Elements whose own trimmed text equals the given text, skipping hidden subtrees. */
export function allByText(text: string, within: ParentNode = document.body): HTMLElement[] {
  return [...within.querySelectorAll<HTMLElement>('*')].filter(element => visible(element)
    && [...element.childNodes].some(node => node.nodeType === Node.TEXT_NODE && node.textContent?.trim() === text))
}

/**
 * Retry an assertion until it passes, flushing React work between attempts.
 * @param assertion Throws while the expected state is not reached.
 * @param timeout Milliseconds before the last failure is rethrown.
 * @returns The assertion's result.
 */
export async function waitFor<T>(assertion: () => T, timeout = 2000): Promise<T> {
  const deadline = Date.now() + timeout
  for (;;) {
    try { return assertion() }
    catch (error) {
      if (Date.now() > deadline) throw error
      await act(async () => { await new Promise(resolve => setTimeout(resolve, 10)) })
    }
  }
}

/** @returns The first match once one appears. */
export function findByRole(role: string, name?: string | RegExp): Promise<HTMLElement> {
  return waitFor(() => byRole(role, name))
}

/** Click an element. */
export function click(element: Element): void {
  act(() => { (element as HTMLElement).click() })
}

/**
 * Type a new value into a text field.
 * @param element Input or textarea.
 * @param value New value.
 */
export function change(element: Element, value: string): void {
  const prototype = element instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype
  act(() => {
    Object.getOwnPropertyDescriptor(prototype, 'value')?.set?.call(element, value)
    element.dispatchEvent(new Event('input', { bubbles: true }))
  })
}

/**
 * Press a key on an element.
 * @param element Target.
 * @param init Key event fields.
 * @returns Whether the default action was not prevented.
 */
export function keyDown(element: EventTarget, init: KeyboardEventInit): boolean {
  let result = true
  act(() => { result = element.dispatchEvent(new KeyboardEvent('keydown', { bubbles: true, cancelable: true, ...init })) })
  return result
}

/** Settle pending promises and React updates. */
export async function flush(): Promise<void> {
  await act(async () => { await new Promise(resolve => setTimeout(resolve, 0)) })
}
