/** Set standards mode in DOM test environments so KaTeX does not warn about happy-dom's default quirks mode. */
if (typeof document !== 'undefined') {
  Object.defineProperty(document, 'compatMode', { value: 'CSS1Compat', configurable: true })
}
