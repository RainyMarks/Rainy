/** Bounded copy history records the originating human tool. */
window.HistoryUtils = {
  addToHistory(history, limit, source, content, sourceId) {
    if (!Array.isArray(history) || !content) return
    history.unshift({ source, sourceId, content, timestamp: new Date().toISOString(), id: Date.now() + Math.random() })
    if (history.length > limit) history.splice(limit)
  },
  clearHistory(history) { if (Array.isArray(history)) history.splice(0) },
  removeFromHistory(history, id) { const index = history.findIndex(item => item.id === id); if (index >= 0) history.splice(index, 1) },
  getHistorySource(id, details = {}) {
    const tool = window.ICE_SKY_TOOLS?.find(item => item.id === id)
    const name = tool?.name || id || 'IceSky'
    return id === 'transforms' && details.activeTransform?.name ? `${name}: ${details.activeTransform.name}` : name
  },
}
