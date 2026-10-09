/** Compile-time nominal string types and exhaustiveness checks. */

declare const BRAND: unique symbol

/** A string carrying a compile-time-only brand `B`. */
export type Branded<B extends string> = string & { readonly [BRAND]: B }

/**
 * Apply a compile-time string brand without changing the value.
 * @param value String admitted by the domain that owns the brand.
 * @returns The same string with the requested brand.
 */
export function brandString<T extends Branded<string>>(value: string | T): T {
  return value as T
}

/**
 * Mark a switch over a closed union as exhaustive.
 * @param value The value no case matched.
 * @returns Never; it always throws.
 */
export function assertNever(value: never): never {
  throw new Error(`Unexpected value: ${JSON.stringify(value)}`)
}
