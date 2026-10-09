/** Minimal observable values read by React through `useSyncExternalStore`. */
import { useSyncExternalStore } from 'react'

/** One replaceable value with change listeners. */
export interface Store<T> {
  /** @returns The current value. */
  getSnapshot(): T
  /**
   * Replace the value; listeners run only when it is a different value (`Object.is`).
   * @param value Next value.
   */
  set(value: T): void
  /**
   * @param listener Called after each change.
   * @returns A function that removes the listener.
   */
  subscribe(listener: () => void): () => void
}

/**
 * Create a store.
 * @param initial First value.
 * @returns A store whose methods can be passed around unbound.
 */
export function createStore<T>(initial: T): Store<T> {
  let value = initial
  const listeners = new Set<() => void>()
  return {
    getSnapshot: () => value,
    set: (next) => {
      if (Object.is(next, value)) return
      value = next
      for (const listener of [...listeners]) listener()
    },
    subscribe: (listener) => {
      listeners.add(listener)
      return () => { listeners.delete(listener) }
    },
  }
}

/**
 * Read a store and re-render when it changes.
 * @param store Store to observe.
 * @returns Its current value.
 */
export function useStore<T>(store: Store<T>): T {
  return useSyncExternalStore(store.subscribe, store.getSnapshot)
}
