/** Compile-time nominal string types. */

declare const BRAND: unique symbol

/** A string carrying a compile-time-only brand `B`. */
export type Branded<B extends string> = string & { readonly [BRAND]: B }
