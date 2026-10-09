/** Share in-flight local loads; failed assets can be retried. */
window.AssetLoader = (() => {
  const scripts = new Map()
  const texts = new Map()
  const styles = new Map()
  function localUrl(source) {
    const url = new URL(source, document.baseURI)
    if (url.origin !== location.origin) throw new Error('External tool assets are unavailable. Reinstall the local resource package.')
    return url.href
  }
  function loadScriptOnce(source) {
    const url = localUrl(source)
    if (scripts.has(url)) return scripts.get(url)
    const existing = Array.from(document.scripts).find(script => script.src === url)
    if (existing?.dataset.loaded === 'true') return Promise.resolve()
    let element = existing
    const pending = new Promise((resolve, reject) => {
      element ??= document.createElement('script')
      element.src = url
      element.async = true
      element.addEventListener('load', () => { element.dataset.loaded = 'true'; resolve() }, { once: true })
      element.addEventListener('error', () => reject(new Error(`Unable to load local asset: ${source}`)), { once: true })
      if (!existing) document.head.appendChild(element)
    }).catch(error => { scripts.delete(url); element?.remove(); throw error })
    scripts.set(url, pending)
    return pending
  }
  async function loadScriptsSequentially(sources) {
    for (const source of sources) await loadScriptOnce(source)
    return sources
  }
  function loadTextOnce(source, options = {}) {
    const url = localUrl(source)
    if (texts.has(url)) return texts.get(url)
    const pending = fetch(url, options).then(response => {
      if (!response.ok) throw new Error(`Unable to load local asset: ${source}`)
      return response.text()
    }).catch(error => { texts.delete(url); throw error })
    texts.set(url, pending)
    return pending
  }
  function loadStyleOnce(source) {
    const url = localUrl(source)
    if (styles.has(url)) return styles.get(url)
    const link = document.createElement('link')
    link.rel = 'stylesheet'
    link.href = url
    const pending = new Promise((resolve, reject) => {
      link.onload = resolve
      link.onerror = () => reject(new Error(`Unable to load local styles: ${source}`))
      const embedding = document.querySelector('link[href$="css/rainy-embed.css"]')
      if (embedding) document.head.insertBefore(link, embedding)
      else document.head.appendChild(link)
    }).catch(error => { styles.delete(url); link.remove(); throw error })
    styles.set(url, pending)
    return pending
  }
  return { loadScriptOnce, loadScriptsSequentially, loadTextOnce, loadStyleOnce }
})()
