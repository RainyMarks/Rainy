/** Import first in tests that load KaTeX: it checks the document mode once, when it loads, and refuses to render in the
 *  quirks mode happy-dom reports. */
Object.defineProperty(document, 'compatMode', { value: 'CSS1Compat', configurable: true })
