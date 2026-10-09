/** Transient embedded feedback belongs to the persistent host toast surface. */
window.NotificationUtils = {
  showNotification(message, kind = 'success', icon = null) {
    const text = document.documentElement.lang === 'en' ? String(message) : window.LocalizationUtils?.translateMessage?.(message) ?? String(message)
    if (window.parent !== window && window.IceSkyRuntime) {
      window.IceSkyRuntime.post({ type: 'rainy:toast', message: text, kind: ['success', 'error', 'warning'].includes(kind) ? kind : 'success' })
      return
    }
    document.querySelector('.copy-notification')?.remove()
    const notification = document.createElement('div')
    notification.className = `copy-notification ${kind}`
    notification.setAttribute('role', kind === 'error' ? 'alert' : 'status')
    if (icon) { const item = document.createElement('i'); item.className = icon; notification.appendChild(item) }
    const content = document.createElement('span')
    content.textContent = text
    notification.appendChild(content)
    document.body.appendChild(notification)
    setTimeout(() => notification.remove(), 3000)
  },
  showCopiedPopup() { this.showNotification('已复制', 'success', 'fas fa-check') },
}
