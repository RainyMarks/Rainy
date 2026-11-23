/** Sanitize styles, DOM and object URLs produced by `docx-preview` inside a ShadowRoot. */

/** Safe data URLs allowed inside Word styles (embedded raster images and fonts, never HTML or SVG scripts). */
const SAFE_CSS_DATA_URL = /^data:(?:image\/(?:png|jpe?g|gif|webp|bmp|avif|x-icon)|font\/[\w.+-]+|application\/(?:font-[\w.+-]+|x-font-[\w.+-]+|vnd\.ms-fontobject))[;,]/iu
/** Safe data URLs allowed on image elements rendered from a Word document. */
const SAFE_IMG_DATA_URL = /^data:image\/(?:png|jpe?g|gif|webp|bmp|avif|x-icon)[;,]/iu
const DANGEROUS_TAGS = 'script, iframe, frame, frameset, object, embed, applet, base, meta, link, foreignObject, foreignobject'

/**
 * Decode ASCII CSS hex/character escapes (`\69`, `\72 `, `\:`) so obfuscated keywords such as `\69mport` or
 * `u\72l(...)` cannot slip past sanitization, while preserving non-ASCII escapes like `\2022` used in list bullets.
 */
function decodeAsciiCssEscapes(css: string): string {
  return css
    .replace(/\\([0-9a-fA-F]{1,6})[ \t\r\n\f]?/gu, (whole, hex: string) => {
      const code = Number.parseInt(hex, 16)
      return code >= 0x20 && code <= 0x7e ? String.fromCharCode(code) : whole
    })
    .replace(/\\([^\r\n\f0-9a-fA-F])/gu, (whole, char: string) => {
      const code = char.charCodeAt(0)
      return code >= 0x20 && code <= 0x7e && char !== '"' && char !== "'" && char !== '\\' ? char : whole
    })
}

/** Strip CSS comments (including an unclosed trailing comment) so tokens cannot be split across `/**\/`. */
function stripCssComments(css: string): string {
  return css.replace(/\/\*[\s\S]*?(?:\*\/|$)/gu, '')
}

/** Remove all calls to a CSS function (such as `expression(...)` or `-webkit-image-set(...)`), balancing nested parentheses. */
function stripCssFunction(css: string, name: string): string {
  const escaped = name.replace(/[.*+?^${}()|[\]\\]/gu, '\\$&')
  const matcher = new RegExp(`(?<![A-Za-z0-9_-])${escaped}\\s*\\(`, 'giu')
  let result = ''
  let cursor = 0
  for (;;) {
    matcher.lastIndex = cursor
    const match = matcher.exec(css)
    if (match === null) {
      result += css.slice(cursor)
      break
    }
    result += css.slice(cursor, match.index)
    let depth = 1
    let pos = matcher.lastIndex
    while (pos < css.length && depth > 0) {
      const ch = css[pos]!
      if (ch === '"' || ch === "'") {
        const quote = ch
        pos++
        while (pos < css.length && css[pos] !== quote) {
          if (css[pos] === '\\') pos++
          pos++
        }
        if (pos < css.length) pos++
        continue
      }
      if (ch === '(') depth++
      else if (ch === ')') depth--
      pos++
    }
    result += 'none'
    cursor = pos
  }
  return result
}

/** @returns Whether an embedded image URL is safe inside an offline Word preview (`blob:` or raster `data:image/*`). */
function isSafeImageAssetUrl(raw: string): boolean {
  const compact = raw.trim().replace(/[\u0000-\u0020\u007f]/gu, '')
  return /^blob:/iu.test(compact) || SAFE_IMG_DATA_URL.test(compact)
}

/** @returns Whether a `url(...)` target is safe inside an offline Word preview (local blob or embedded raster/font data). */
function isSafeCssUrl(raw: string): boolean {
  const unquoted = raw.trim().replace(/^(['"])([\s\S]*)\1$/u, '$2').trim()
  const compact = unquoted.replace(/[\u0000-\u0020\u007f]/gu, '')
  if (compact === '') return false
  if (compact.startsWith('#')) return true
  if (/^blob:/iu.test(compact)) return true
  return SAFE_CSS_DATA_URL.test(compact)
}

/**
 * Sanitize CSS text emitted by `docx-preview` (or inline style attributes):
 * strips comments, `@import` rules, `expression(...)`, `image-set(...)` / `-webkit-image-set(...)`,
 * legacy script bindings (`behavior`, `-moz-binding`), and any external or non-allowlisted `url(...)` targets
 * while preserving embedded `blob:` and safe `data:` assets.
 * @param cssText Raw CSS string.
 * @returns Sanitized CSS string.
 */
export function sanitizeDocxStyles(cssText: string): string {
  let clean = decodeAsciiCssEscapes(stripCssComments(cssText))
  // Drop `@import` rules (whether terminated by `;`, newline, or EOF).
  clean = clean.replace(/@import\b[^;{}]*(?:;|$)/giu, '')
  // Drop legacy script-executing and bare-string external-fetching CSS functions.
  clean = stripCssFunction(clean, 'expression')
  clean = stripCssFunction(clean, 'image-set')
  clean = stripCssFunction(clean, '-webkit-image-set')
  clean = clean.replace(/(?:^|[;{\s])(?:-moz-binding|behavior)\s*:[^;{}]*/giu, (match) => {
    const lead = match[0]
    return lead === ';' || lead === '{' ? lead : ''
  })
  // Allow only safe `blob:` / embedded `data:` / fragment targets inside `url(...)` (handles quoted `)` inside URLs).
  clean = clean.replace(/\burl\s*\(\s*("(?:\\.|[^"\\])*"|'(?:\\.|[^'\\])*'|[^)]*?)\s*\)/giu, (whole, target: string) => (isSafeCssUrl(target) ? whole : 'none'))
  return clean
}

/**
 * Sanitize styles, links, images, SVG references, forms and active elements produced by `docx-preview`.
 * @param stylesContainer Container where `docx-preview` placed `<style>` elements.
 * @param bodyContainer Container where `docx-preview` placed document markup.
 */
export function sanitizeDocxDom(stylesContainer: Element, bodyContainer: Element): void {
  for (const container of [stylesContainer, bodyContainer]) {
    for (const active of container.querySelectorAll(DANGEROUS_TAGS)) active.remove()
    for (const styleEl of container.querySelectorAll('style')) {
      styleEl.textContent = sanitizeDocxStyles(styleEl.textContent ?? '')
    }
    for (const el of container.querySelectorAll('*')) {
      const tag = el.tagName.toLowerCase()
      for (const attr of [...el.attributes]) {
        const lower = attr.name.toLowerCase()
        if (lower.startsWith('on') || lower === 'srcdoc' || lower === 'srcset' || lower === 'action' || lower === 'formaction' || lower === 'ping') {
          el.removeAttribute(attr.name)
        } else if (lower === 'style') {
          el.setAttribute('style', sanitizeDocxStyles(attr.value))
        } else if (lower === 'src' || lower === 'poster' || lower === 'background') {
          if (!isSafeImageAssetUrl(attr.value)) el.removeAttribute(attr.name)
        } else if (lower === 'href' || lower === 'xlink:href') {
          if (tag === 'a') {
            if (lower === 'xlink:href') el.removeAttribute(attr.name)
          } else if (tag === 'use') {
            if (!attr.value.trim().startsWith('#')) el.removeAttribute(attr.name)
          } else if (tag === 'image' || tag === 'feimage') {
            if (!isSafeImageAssetUrl(attr.value)) el.removeAttribute(attr.name)
          } else {
            el.removeAttribute(attr.name)
          }
        }
      }
    }
  }
  // Links leave the page only for http(s); internal fragments stay, other targets (files, scripts) are dropped.
  for (const link of bodyContainer.querySelectorAll('a[href]')) {
    const href = (link.getAttribute('href') ?? '').trim()
    if (/^https?:\/\//iu.test(href)) {
      link.setAttribute('target', '_blank')
      link.setAttribute('rel', 'noopener noreferrer')
    } else if (!href.startsWith('#')) {
      link.removeAttribute('href')
    }
  }
}

/** Extract every unique `blob:` URL present in a ShadowRoot or element subtree. */
export function collectBlobUrls(root: ShadowRoot | Element): string[] {
  const urls = new Set<string>()
  const scan = (text: string): void => {
    for (const match of text.match(/blob:[^"')\s<>]+/gu) ?? []) urls.add(match)
  }
  scan(root.innerHTML)
  for (const styleEl of root.querySelectorAll('style')) scan(styleEl.textContent ?? '')
  for (const el of root.querySelectorAll('[src], [style], [href]')) {
    for (const name of ['src', 'style', 'href', 'xlink:href']) {
      const value = el.getAttribute(name)
      if (value !== null) scan(value)
    }
  }
  return [...urls]
}

/**
 * Revoke every `blob:` URL found in `root`, skipping any already recorded in `revoked`.
 * @param root ShadowRoot or container whose object URLs should be released.
 * @param revoked Optional set tracking already-revoked URLs across async cancellation and cleanup.
 */
export function revokeBlobUrls(root: ShadowRoot | Element, revoked?: Set<string>): void {
  for (const url of collectBlobUrls(root)) {
    if (revoked?.has(url) === true) continue
    revoked?.add(url)
    URL.revokeObjectURL(url)
  }
}
