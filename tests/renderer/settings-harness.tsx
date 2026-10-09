/** Fake Host connection, toast recorder and DOM helpers for the Settings tests. */
import { act } from 'react'
import type { ReactElement } from 'react'
import { createRoot } from 'react-dom/client'
import type { Root } from 'react-dom/client'
import { vi } from 'vitest'
import type { HostEvent, HostEvents, HostMethod, MethodParams, MethodResult } from '../../src/shared/rpc.ts'
import type { StrataNativeHost, StrataStatus } from '../../src/shared/strata-protocol.ts'

Reflect.set(globalThis, 'IS_REACT_ACT_ENVIRONMENT', true)

type Handler<M extends HostMethod> = (params: MethodParams<M>) => MethodResult<M> | Promise<MethodResult<M>>
type Handlers = { [M in HostMethod]?: Handler<M> }

const handlers: Handlers = {}
const listeners = new Map<HostEvent, Set<(data: never) => void>>()

/**
 * Answer one Host method in the current test.
 * @param method Method name.
 * @param handler Receives the parameters; its value or rejection becomes the response.
 */
export function handle<M extends HostMethod>(method: M, handler: Handler<M>): void {
  handlers[method] = handler as Handlers[M]
}

async function dispatch<M extends HostMethod>(method: M, params: MethodParams<M>): Promise<MethodResult<M>> {
  const handler = handlers[method] as Handler<M> | undefined
  if (handler === undefined) throw new Error(`No test handler for ${method}`)
  return handler(params)
}

/** Stand-in for `host` from `src/renderer/rpc.ts`. */
export const fakeHost = {
  state: 'open' as const,
  call: vi.fn(<M extends HostMethod>(method: M, params?: MethodParams<M>) => dispatch(method, params as MethodParams<M>)),
  on<E extends HostEvent>(event: E, listener: (data: HostEvents[E]) => void): () => void {
    let set = listeners.get(event)
    if (set === undefined) { set = new Set(); listeners.set(event, set) }
    set.add(listener as (data: never) => void)
    return () => { set.delete(listener as (data: never) => void) }
  },
  onState(_listener: () => void): () => void { return () => undefined },
}

/** Module replacing `src/renderer/rpc.ts`. */
export const rpcModule = {
  host: fakeHost,
  HostError: class HostError extends Error {},
  useConnectionState: () => 'open' as const,
}

/** Recorder replacing `toast` from `src/renderer/ui/toasts.tsx`. */
export const toast = vi.fn<(text: string, options?: { tone?: 'success' }) => void>()
/** Module replacing `src/renderer/ui/toasts.tsx`. */
export const toastsModule = { toast, ToastHost: () => null }

/**
 * Broadcast a Host event to the listeners the page registered.
 * @param event Event name.
 * @param data Payload.
 */
export async function emit<E extends HostEvent>(event: E, data: HostEvents[E]): Promise<void> {
  await act(async () => { for (const listener of listeners.get(event) ?? []) (listener as (value: HostEvents[E]) => void)(data) })
  await flush()
}

/** @returns The number of listeners registered for an event. */
export function listenerCount(event: HostEvent): number { return listeners.get(event)?.size ?? 0 }

const roots: { root: Root; host: HTMLElement }[] = []

/**
 * Render an element into the document.
 * @param element Element to render.
 * @returns Its container and an unmount function.
 */
export async function render(element: ReactElement): Promise<{ container: HTMLElement; unmount(): Promise<void>; rerender(next: ReactElement): Promise<void> }> {
  const container = document.createElement('div')
  document.body.append(container)
  const root = createRoot(container)
  roots.push({ root, host: container })
  await act(async () => { root.render(element) })
  await flush()
  return {
    container,
    unmount: async () => { await act(async () => { root.unmount() }) },
    rerender: async (next) => { await act(async () => { root.render(next) }); await flush() },
  }
}

/** Unmount everything, forget handlers and listeners and clear the recorders. */
export async function cleanup(): Promise<void> {
  for (const { root, host } of roots.splice(0)) {
    await act(async () => { root.unmount() })
    host.remove()
  }
  document.body.innerHTML = ''
  for (const key of Object.keys(handlers) as HostMethod[]) delete handlers[key]
  fakeHost.call.mockClear()
  toast.mockReset()
}

/** Let pending promises and effects settle. */
export async function flush(): Promise<void> {
  for (let index = 0; index < 3; index++) await act(async () => { await new Promise(resolve => setTimeout(resolve, 0)) })
}

/**
 * Retry an assertion until it passes.
 * @param check Throws while the expected state is not reached.
 * @param timeoutMs Give-up time.
 */
export async function waitFor(check: () => void, timeoutMs = 2000): Promise<void> {
  const end = Date.now() + timeoutMs
  for (;;) {
    try { check(); return } catch (error) {
      if (Date.now() > end) throw error
      await flush()
    }
  }
}

const accessibleName = (element: Element): string => element.getAttribute('aria-label') ?? element.textContent?.trim() ?? ''

/**
 * @param selector CSS selector.
 * @param root Search root.
 * @returns Every match.
 */
export function all<T extends Element = HTMLElement>(selector: string, root: ParentNode = document.body): T[] {
  return [...root.querySelectorAll<T>(selector)]
}

/**
 * @param name Accessible name: `aria-label` or text.
 * @param root Search root.
 * @returns The button, or `undefined`.
 */
export function findButton(name: string, root: ParentNode = document.body): HTMLButtonElement | undefined {
  return all<HTMLButtonElement>('button', root).find(button => accessibleName(button) === name)
}

/**
 * @param name Accessible name: `aria-label` or text.
 * @param root Search root.
 * @returns The button; throws when absent.
 */
export function button(name: string, root: ParentNode = document.body): HTMLButtonElement {
  const found = findButton(name, root)
  if (found === undefined) throw new Error(`No button named ${name}; buttons: ${all('button', root).map(accessibleName).join(' | ')}`)
  return found
}

/**
 * @param label `aria-label` of an input, textarea or control.
 * @param root Search root.
 * @returns The control; throws when absent.
 */
export function control<T extends HTMLElement = HTMLInputElement>(label: string, root: ParentNode = document.body): T {
  const found = all<T>('[aria-label]', root).find(element => element.getAttribute('aria-label') === label && element.tagName !== 'BUTTON')
  if (found === undefined) throw new Error(`No control labelled ${label}`)
  return found
}

/**
 * @param text Exact trimmed text.
 * @param root Search root.
 * @returns Whether an element whose own text equals `text` exists.
 */
export function hasText(text: string, root: ParentNode = document.body): boolean {
  return all('*', root).some(element => element.textContent?.trim() === text)
}

/** @param element Element to click. */
export async function click(element: HTMLElement): Promise<void> {
  await act(async () => { element.click() })
  await flush()
}

/**
 * Replace an input's value the way typing does.
 * @param element Input or textarea.
 * @param value New value.
 */
export async function type(element: HTMLInputElement | HTMLTextAreaElement, value: string): Promise<void> {
  const prototype = element instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype
  await act(async () => {
    Object.getOwnPropertyDescriptor(prototype, 'value')?.set?.call(element, value)
    element.dispatchEvent(new Event('input', { bubbles: true }))
  })
  await flush()
}

/**
 * Open a dropdown and pick one option.
 * @param label Accessible name of the dropdown trigger.
 * @param option Option text.
 * @param root Search root for the trigger.
 */
export async function choose(label: string, option: string, root: ParentNode = document.body): Promise<void> {
  await click(button(label, root))
  const item = all<HTMLButtonElement>('[role="menuitem"]').find(element => element.textContent?.trim() === option)
  if (item === undefined) throw new Error(`No option ${option}; options: ${all('[role="menuitem"]').map(element => element.textContent).join(' | ')}`)
  await click(item)
}

/** @returns A promise and the functions that settle it. */
export function deferred<T>(): { promise: Promise<T>; resolve(value: T): void; reject(error: Error): void } {
  let resolve!: (value: T) => void
  let reject!: (error: Error) => void
  const promise = new Promise<T>((done, fail) => { resolve = done; reject = fail })
  return { promise, resolve, reject }
}

/** @returns A stopped bundled engine with user-owned model and MTP files. */
export function strataStatus(): StrataStatus {
  return {
    phase: 'stopped', settings: { modelPath: 'C:\\models\\main.gguf', mtpPath: 'C:\\models\\mtp.gguf',
      contextWindow: 32768, port: 8081, kvCache: 'int8', vramReserveMiB: 700, residentBudgetGiB: null },
    runtime: { available: true, version: '0.1.39', root: 'C:\\RainyAgent\\resources\\strata', missing: [] }, profiles: [],
    model: { sourcePath: 'C:\\models\\main.gguf', model: 'Qwen3.8-Flash-Next', ggufPath: 'C:\\models\\main.gguf',
      packPath: null, tokenizerPath: null, mtpPath: 'C:\\models\\mtp.gguf', needsPreparation: true },
    server: null, progress: null, error: null,
  }
}

/** @returns Desktop Strata methods backed by `status`; each is a mock. */
export function strataBridge(status = strataStatus()) {
  return {
    status: vi.fn<StrataNativeHost['status']>(async () => status),
    save: vi.fn<StrataNativeHost['save']>(async settings => ({ ...status, settings })),
    start: vi.fn<StrataNativeHost['start']>(async () => ({ ...status, phase: 'preparing', progress: 'Preparing local model files' })),
    stop: vi.fn<StrataNativeHost['stop']>(async () => status),
    selectModel: vi.fn<StrataNativeHost['selectModel']>(async () => null),
    connect: vi.fn<StrataNativeHost['connect']>(async () => ({ provider: 'rainy-strata', model: 'served-model' })),
  }
}

/**
 * Expose or remove a preload bridge on the window.
 * @param name Global name.
 * @param bridge Bridge object, or `undefined` to remove it.
 */
export function setBridge(name: '__RAINY_STRATA_NATIVE__' | '__RAINY_RUNTIME_NATIVE__' | '__RAINY_MODULES__', bridge: object | undefined): void {
  if (bridge === undefined) Reflect.deleteProperty(globalThis, name)
  else Reflect.set(globalThis, name, bridge)
}
