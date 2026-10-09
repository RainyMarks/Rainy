/** Clipboard success is the only point at which copy history is published. */
window.AppHistory = {
  computed: {
    filteredCopyHistory() {
      const query = String(this.copyHistorySearch || '').trim().toLowerCase()
      return query ? this.copyHistory.filter(item => [item.source, item.content, this.formatHistoryTime(item.timestamp)].join('\n').toLowerCase().includes(query)) : this.copyHistory
    },
    isCopyHistoryFiltering() { return !!String(this.copyHistorySearch || '').trim() },
  },
  methods: {
    toggleCopyHistory() {
      this.showCopyHistory = !this.showCopyHistory
      if (this.showCopyHistory) this.$nextTick(() => document.querySelector('.copy-history-panel .history-search-input')?.focus())
    },
    addToCopyHistory(source, content) { window.HistoryUtils.addToHistory(this.copyHistory, this.maxHistoryItems, source, content, this._iceSkyId || this.activeTab) },
    clearCopyHistory() { window.HistoryUtils.clearHistory(this.copyHistory); this.showNotification('历史已清空', 'success') },
    removeFromCopyHistory(id) { window.HistoryUtils.removeFromHistory(this.copyHistory, id); this.showNotification('已从历史中移除', 'success') },
    formatHistoryTime(value) { return value ? new Date(value).toLocaleString() : '' },
    exportCopyHistory() {
      const items = this.filteredCopyHistory
      if (!items.length) { this.showNotification('没有可导出的历史记录', 'warning'); return }
      const text = items.map((item, index) => `# ${index + 1}\n来源: ${item.source}\n时间: ${this.formatHistoryTime(item.timestamp)}\n内容:\n${item.content}`).join('\n\n' + '='.repeat(64) + '\n\n')
      this.downloadTextFile(`copy-history-${new Date().toISOString().replace(/[:.]/g, '-')}.txt`, text)
    },
    async copyToClipboard(content, fromHistory = false) {
      if (!content || !window.ClipboardUtils) return false
      const history = this.copyHistory
      const sourceId = this._iceSkyId || this.activeTab
      const source = window.HistoryUtils.getHistorySource(sourceId, { activeTransform: this.activeTransform })
      const limit = this.maxHistoryItems
      const operation = window.ClipboardUtils.copy(String(content), { onSuccess: () => {
        if (!fromHistory && !history.some(item => item.content === content)) window.HistoryUtils.addToHistory(history, limit, source, String(content), sourceId)
      } })
      return window.IceSkyRuntime ? window.IceSkyRuntime.ownCopy(operation) : operation
    },
    async forceCopyToClipboard(content) {
      if (!content || !this.autoCopyEnabled) return false
      if (this.isPasteOperation) { this.isPasteOperation = false; return false }
      if (!this.isTransformCopy && this.ignoreKeyboardEvents) return false
      const copied = await this.copyToClipboard(content)
      this.isTransformCopy = false
      return copied
    },
  },
}
