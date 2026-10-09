/** Page-level toast queue: call {@link toast} from anywhere, render {@link ToastHost} once. */
import { useSyncExternalStore } from 'react'
import { Toast } from './Toast.tsx'

interface Entry { id: number; text: string; tone?: 'success' | undefined; actions?: readonly { label: string; onClick: () => void }[] | undefined }

let entries: Entry[] = []
let nextId = 1
const listeners = new Set<() => void>()

function publish(next: Entry[]): void {
  entries = next
  for (const listener of listeners) listener()
}

/**
 * Show a short message at the top of the window.
 * @param text Message.
 * @param options `success` adds a check mark; actions add inline buttons.
 */
export function toast(text: string, options: { tone?: 'success'; actions?: readonly { label: string; onClick: () => void }[] } = {}): void {
  publish([...entries.slice(-2), { id: nextId++, text, tone: options.tone, actions: options.actions }])
}

/** @returns The toasts currently shown; mount once near the root. */
export function ToastHost(): JSX.Element {
  const current = useSyncExternalStore(listener => { listeners.add(listener); return () => { listeners.delete(listener) } }, () => entries)
  return (
    <>
      {current.map(entry => (
        <Toast key={entry.id} text={entry.text} tone={entry.tone} actions={entry.actions}
          onDone={() => { publish(entries.filter(item => item.id !== entry.id)) }} />
      ))}
    </>
  )
}
