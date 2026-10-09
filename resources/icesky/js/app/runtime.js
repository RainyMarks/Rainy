/** One workbench renderer with independently persisted chat drafts and owned tool lifetimes. */
(() => {
  const persistence = window.IceSkyPersistence
  const words = {
    zh: { loading: '正在加载工具…', retry: '重试', saved: '已保存', saving: '正在保存…', unsaved: '未保存', export: '导出草稿',
      missingFiles: '此草稿使用了本地文件。请重新选择文件，或清除附件后继续。', clearFiles: '清除缺失附件',
      pending: '输入已变化，结果待重新计算。', manual: '文本超过 64 KiB，请点击计算。', computing: '正在计算…', calculate: '计算',
      previous: '上一页', next: '下一页', directory: '全部工具', search: '搜索工具', recent: '最近使用', pinned: '收藏',
      pin: '收藏工具', unpin: '取消收藏', loadFailed: '工具未能加载，请重试。', savedFailed: '草稿未能保存，内容仍保留在此页面。' },
    en: { loading: 'Loading tool…', retry: 'Retry', saved: 'Saved', saving: 'Saving…', unsaved: 'Not saved', export: 'Export draft',
      missingFiles: 'This draft used local files. Select them again, or clear the attachments to continue.', clearFiles: 'Clear missing attachments',
      pending: 'Input changed. Recalculate to update the result.', manual: 'Text exceeds 64 KiB. Select Calculate.', computing: 'Calculating…', calculate: 'Calculate',
      previous: 'Previous', next: 'Next', directory: 'All tools', search: 'Find a tool', recent: 'Recent', pinned: 'Pinned',
      pin: 'Pin tool', unpin: 'Unpin tool', loadFailed: 'Unable to load this tool. Retry to continue.', savedFailed: 'The draft was not saved. Your text remains on this page.' },
  }
  const mediaTools = new Set(['audioinject', 'docxinject', 'pdfinject', 'imageinject', 'richtextinject'])
  const legacyToolKeys = new Set(['transformCategoryOrder', 'transformLastUsed', 'transformFavorites', 'transformOptionPrefs',
    'pc-temperature', 'pc-draft-v2', 'pc-panel-state-v2', 'pc-model', 'pc-count', 'pc-streaming-enabled',
    'stylecraft-lite-mode', 'stylecraft-lite-ui-mode', 'stylecraft-lite-use-case', 'stylecraft-lite-wenyan-template',
    'stylecraft-lite-poetry-template', 'stylecraft-lite-poetry-form', 'stylecraft-lite-style-level', 'stylecraft-lite-auto-protect',
    'stylecraft-lite-locked-terms', 'translate-model', 'dialog-template-tool-v1', 'imageinject-preset', 'icesky.sample.recipes'])
  const moduleOptions = new Map()
  const views = new Map()
  const sourceFiles = new Map()
  const cleanupTasks = new Set()
  const clipboardTasks = new Set()
  let root
  let factory
  let commonMethods
  let commonComputed
  let commonKeys
  let defaults
  let store
  let currentContext = { kind: 'standalone' }
  let configureTail = Promise.resolve()
  let configuration
  let activation = 0
  let contextEpoch = 0
  let saveTimer
  let visible = true
  let hydrating = false
  let disposed = false
  let sharedWatchStops = []
  let legacyKeys = []

  function post(value) {
    if (window.parent !== window) window.parent.postMessage(value, location.origin)
  }

  function status(state, message) {
    if (root) { root.saveState = state; root.saveMessage = message ?? '' }
    post({ type: 'rainy:status', state, ...(message ? { message } : {}) })
  }

  function currentSnapshot() {
    if (!store) return {}
    const data = store.data
    data.tools ??= {}
    data.shared ??= {}
    for (const [id, view] of views) {
      if (!view._iceSkyDisposed) data.tools[id] = { fields: persistence.fields(view, view._iceSkyKeys), files: view._iceSkyFiles }
    }
    if (root) for (const key of persistence.sharedKeys) data.shared[key] = persistence.jsonValue(root[key], key)
    return data
  }

  function changed(epoch = contextEpoch) {
    if (hydrating || disposed || !store?.ready || epoch !== contextEpoch) return
    store.changed()
    status('saving')
    clearTimeout(saveTimer)
    saveTimer = setTimeout(() => { void flush().catch(() => {}) }, window.CONFIG.DRAFT_SAVE_DELAY_MS)
  }

  async function flush(record = store) {
    clearTimeout(saveTimer)
    await Promise.all([...clipboardTasks])
    if (!record?.ready) {
      if (record) throw new Error('The saved draft could not be read. Export your text before closing.')
      return
    }
    while (record.dirty !== record.saved) {
      await record.save(() => record === store ? currentSnapshot() : record.data)
      await root?.$nextTick()
    }
    if (record === store && record.dirty === record.saved && legacyKeys.length) {
      for (const key of legacyKeys) try { localStorage.removeItem(key) } catch (_unavailableStorage) { /* Host already owns the imported text. */ }
      legacyKeys = []
    }
  }

  function migrateLegacyDraft(data) {
    if (store.scope !== 'standalone' || data.legacyMigrated) return
    data.shared.toolStorage ??= {}
    for (const key of legacyToolKeys) {
      try {
        const value = localStorage.getItem(key)
        if (value !== null) { data.shared.toolStorage[key] ??= value; legacyKeys.push(key) }
      } catch (_unavailableStorage) { /* An unavailable browser store leaves the Host draft unchanged. */ }
    }
    let raw
    try { raw = localStorage.getItem('pc-draft-v2') } catch (_unavailableStorage) { raw = null }
    if (raw) {
      try {
        const draft = JSON.parse(raw)
        if (draft && typeof draft === 'object') {
          const fields = {}
          for (const [oldKey, newKey] of [['input', 'pcInput'], ['strategy', 'pcStrategy'], ['customInstruction', 'pcCustomInstruction']]) {
            if (typeof draft[oldKey] === 'string') fields[newKey] = draft[oldKey]
          }
          data.tools.promptcraft ??= { fields, files: {} }
        }
      } catch (_invalidLegacyDraft) { /* Keep the original browser record for manual recovery. */ }
    }
    data.legacyMigrated = true
    store.changed()
  }

  function applyAppearance(appearance = {}) {
    const dark = appearance.dark === true
    document.body.classList.toggle('dark-theme', dark)
    document.body.classList.toggle('light-theme', !dark)
    document.documentElement.style.colorScheme = dark ? 'dark' : 'light'
    const colorNames = ['background', 'panel', 'input', 'hover', 'text', 'secondary', 'border', 'accent', 'accentText', 'error', 'warning', 'success']
    for (const name of colorNames) {
      const color = appearance.colors?.[name]
      if (typeof color === 'string' && CSS.supports('color', color)) {
        document.body.style.setProperty(`--rainy-${name}`, color)
        document.documentElement.style.setProperty(`--rainy-${name}`, color)
      }
    }
    document.documentElement.style.backgroundColor = appearance.colors?.background ?? (dark ? '#181818' : '#ffffff')
    const font = Number(appearance.fontSize)
    const code = Number(appearance.codeFontSize)
    if (Number.isFinite(font) && font >= 8 && font <= 48) document.documentElement.style.setProperty('--rainy-font-size', `${font}px`)
    if (Number.isFinite(code) && code >= 8 && code <= 48) document.documentElement.style.setProperty('--rainy-code-font-size', `${code}px`)
    const locale = appearance.locale === 'en' ? 'en' : 'zh'
    document.documentElement.lang = locale === 'en' ? 'en' : 'zh-CN'
    if (root) root.uiText = words[locale]
  }

  function localizeTemplate(template) {
    const holder = document.createElement('div')
    holder.innerHTML = template
    if (configuration?.appearance?.locale === 'en') return holder.innerHTML
    const translate = window.LocalizationUtils?.translateMessage
    if (!translate) return holder.innerHTML
    const walker = document.createTreeWalker(holder, NodeFilter.SHOW_TEXT)
    let node
    while ((node = walker.nextNode())) {
      if (node.parentElement?.closest('textarea,pre,code,[v-html],[v-text]') || node.textContent.includes('{{')) continue
      node.textContent = translate(node.textContent)
    }
    for (const element of holder.querySelectorAll('[title],[placeholder],[aria-label]')) {
      for (const name of ['title', 'placeholder', 'aria-label']) if (element.hasAttribute(name)) element.setAttribute(name, translate(element.getAttribute(name)))
    }
    return holder.innerHTML
  }

  function ownCleanup(result) {
    if (!result || typeof result.then !== 'function') return
    const pending = Promise.resolve(result).catch(error => { status('error', error.message) }).finally(() => cleanupTasks.delete(pending))
    cleanupTasks.add(pending)
  }

  function stopView(view) {
    if (!view || view._iceSkyDisposed) return
    view._iceSkyVisible = false
    const implementation = view._iceSkyTool
    try { ownCleanup(implementation.onDeactivate?.(view)) } catch (error) { status('error', error.message) }
    for (const name of ['cancelTransform', 'cancelDecoder', 'cancelTokenizer', 'audioInjectStopSpeaking', 'pdfiStopOcr']) {
      if (typeof view[name] === 'function') try { ownCleanup(view[name]()) } catch (error) { status('error', error.message) }
    }
    for (const value of Object.values(view.$data)) if (value instanceof AbortController) value.abort()
    for (const element of view.$el?.querySelectorAll?.('audio,video') ?? []) element.pause()
    for (const key of Object.keys(view)) if (/^_.*(?:Timer|Timeout)$/.test(key)) { clearTimeout(view[key]); view[key] = null }
    if (view._iceSkyId === 'imageinject' && !view.imagePreviewDataUrl) {
      view._iceSkyGenerationInputs.delete('imageQueueRender')
      view._iceSkyGenerationInputs.delete('imageRenderPreview')
    }
  }

  function releaseView(view) {
    stopView(view)
    view._iceSkyDisposed = true
    for (const unwatch of view._iceSkyWatchStops ?? []) unwatch()
    if (view._iceSkyFileListener) view.$el?.removeEventListener('change', view._iceSkyFileListener)
    for (const [key, value] of Object.entries(view.$data)) {
      if (typeof value === 'string' && value.startsWith('blob:')) URL.revokeObjectURL(value)
      if (value instanceof Blob || ArrayBuffer.isView(value) || value instanceof ArrayBuffer) view[key] = null
    }
    for (const key of Object.keys(view)) {
      if (!key.startsWith('_') || key.startsWith('_iceSky')) continue
      const value = view[key]
      if (value instanceof AbortController) value.abort()
      if (typeof AudioContext !== 'undefined' && value instanceof AudioContext) ownCleanup(value.close())
      if (/Timer$|Timeout$/.test(key)) clearTimeout(value)
    }
    if (views.get(view._iceSkyId) === view) views.delete(view._iceSkyId)
  }

  function wrapToolMethod(name, method) {
    return function (...args) {
      if (this._iceSkyDisposed) return
      const imageRender = ['imageQueueRender', 'imageRenderPreview', 'imageRenderUploadedPreview', 'imageRenderHybridPreview'].includes(name)
      const generating = /Generate|GeneratePdf|RenderImage/.test(name) || imageRender
      const automatic = args[0] === false || (imageRender && args.length === 0)
      if (generating && (this._iceSkyRestoring || (automatic && !this._iceSkyVisible))) return
      if (this.iceSkyMissingFiles && /Generate|Download|Analyze|Ocr|BuildPackage/.test(name)) {
        this.showNotification(this.uiText.missingFiles, 'warning')
        return
      }
      if (generating && automatic) {
        const fingerprint = JSON.stringify(persistence.fields(this, this._iceSkyEditableKeys))
        if (this._iceSkyGenerationInputs.get(name) === fingerprint) return this._iceSkyGenerationResults.get(name)
        this._iceSkyGenerationInputs.set(name, fingerprint)
      }
      const copying = /copy/i.test(name)
      if (copying) this._iceSkyCopySilence = (this._iceSkyCopySilence ?? 0) + 1
      let result
      try { result = method.apply(this, args) }
      catch (error) { if (copying) this._iceSkyCopySilence--; throw error }
      if (copying) {
        if (result && typeof result.then === 'function') void Promise.resolve(result).then(() => this._iceSkyCopySilence--, () => this._iceSkyCopySilence--)
        else this._iceSkyCopySilence--
      }
      if (generating) this._iceSkyGenerationResults.set(name, result)
      if (result && typeof result.then === 'function') {
        void Promise.resolve(result).then(() => {
          if (this._iceSkyDisposed) releaseLateResources(this)
        }, () => {
          this._iceSkyGenerationInputs.delete(name)
          if (this._iceSkyDisposed) releaseLateResources(this)
        })
      }
      return result
    }
  }

  function releaseLateResources(view) {
    for (const value of Object.values(view.$data)) if (typeof value === 'string' && value.startsWith('blob:')) URL.revokeObjectURL(value)
  }

  function componentOptions(loaded) {
    const { tool, descriptor } = loaded
    const mixins = descriptor.id === 'transforms' && window.TranslateTool ? [new window.TranslateTool(), tool] : [tool]
    const data = Object.assign({}, ...mixins.map(item => item.getVueData()))
    const toolMethods = Object.assign({}, ...mixins.map(item => item.getVueMethods()))
    const toolWatchers = Object.assign({}, ...mixins.map(item => item.getVueWatchers()))
    const hooks = mixins.map(item => item.getVueLifecycle())
    const keys = persistence.persistentKeys(data, loaded.template)
    const savedFields = store.data.tools?.[descriptor.id]?.fields ?? {}
    for (const key of keys) {
      if (!Object.hasOwn(savedFields, key) || data[key] === null) continue
      const saved = savedFields[key]
      if (typeof saved !== typeof data[key] || Array.isArray(saved) !== Array.isArray(data[key])) throw new Error(`Invalid saved tool field: ${descriptor.id}.${key}`)
    }
    const editableKeys = [...new Set([...loaded.template.matchAll(/v-model(?:\.[\w-]+)*\s*=\s*"\s*([\w$]+)/g)].map(match => match[1]))].filter(key => key in data)
    const computed = {}
    for (const key of [...commonKeys, ...Object.keys(commonComputed)]) {
      if (!(key in data) && !(key in toolMethods)) computed[key] = { get() { return root[key] }, set(value) { root[key] = value } }
    }
    const holder = document.createElement('div')
    holder.innerHTML = localizeTemplate(loaded.template)
    const toolRoot = holder.firstElementChild
    toolRoot.classList.add('rainy-tool-component')
    toolRoot.insertAdjacentHTML('afterbegin', '<div v-if="iceSkyMissingFiles" class="state-panel rainy-file-restore" role="status"><p>{{ uiText.missingFiles }}</p><button class="action-button" @click="clearMissingFiles">{{ uiText.clearFiles }}</button></div>')
    const template = toolRoot.outerHTML
    const methods = { ...commonMethods }
    for (const [name, method] of Object.entries(toolMethods)) methods[name] = wrapToolMethod(name, method)
    methods.clearMissingFiles = function () {
      this._iceSkyFiles = {}
      this.iceSkyMissingFiles = false
      for (const key of Object.keys(this.$data)) if (/Upload(?:Name|Type|Size|Ext)$/.test(key)) this[key] = data[key]
      changed(this._iceSkyEpoch)
    }
    return {
      name: `IceSky-${descriptor.id}`,
      template,
      computed,
      methods,
      data() {
        const saved = store.data.tools?.[descriptor.id]
        const value = Object.assign({}, data)
        if (saved?.fields) for (const key of keys) if (Object.hasOwn(saved.fields, key)) value[key] = saved.fields[key]
        return { ...value, iceSkyMissingFiles: Object.keys(saved?.files ?? {}).length > 0 }
      },
      beforeCreate() {
        this._iceSkyId = descriptor.id
        this._iceSkyTool = tool
        this._iceSkyEpoch = contextEpoch
        this._iceSkyKeys = keys
        this._iceSkyEditableKeys = editableKeys
        this._iceSkyFiles = { ...(store.data.tools?.[descriptor.id]?.files ?? {}) }
        this._iceSkyRestoring = !!store.data.tools?.[descriptor.id]
        this._iceSkyVisible = visible
        this._iceSkyGenerationInputs = new Map()
        this._iceSkyGenerationResults = new Map()
        this._iceSkyWatchStops = []
        this._iceSkyDisposed = false
      },
      created() {
        views.set(descriptor.id, this)
        for (const hook of hooks) hook.created?.call(this)
        for (const key of keys) this._iceSkyWatchStops.push(this.$watch(key, () => changed(this._iceSkyEpoch), { deep: typeof data[key] === 'object' }))
      },
      mounted() {
        for (const hook of hooks) hook.mounted?.call(this)
        this._iceSkyFileListener = event => {
          const input = event.target
          if (!(input instanceof HTMLInputElement) || input.type !== 'file') return
          const id = input.id || input.name || 'attachment'
          const files = [...(input.files ?? [])]
          sourceFiles.set(`${store.scope}/${descriptor.id}/${id}`, files)
          for (const key of Object.keys(this._iceSkyFiles)) if (key === id || key.startsWith(`${id}:`)) delete this._iceSkyFiles[key]
          files.forEach((file, index) => { this._iceSkyFiles[`${id}:${index}`] = { name: file.name, type: file.type, size: file.size, lastModified: file.lastModified } })
          this.iceSkyMissingFiles = false
          changed(this._iceSkyEpoch)
        }
        this.$el.addEventListener('change', this._iceSkyFileListener)
        this.$nextTick(() => {
          this._iceSkyRestoring = false
          for (const input of this.$el.querySelectorAll('input[type="file"]')) {
            const id = input.id || input.name || 'attachment'
            const files = sourceFiles.get(`${store.scope}/${descriptor.id}/${id}`)
            if (!files?.length) continue
            const transfer = new DataTransfer()
            files.forEach(file => transfer.items.add(file))
            input.files = transfer.files
            input.dispatchEvent(new Event('change', { bubbles: true }))
          }
        })
      },
      activated() {
        this._iceSkyVisible = visible
        root.toolInstance = this
        if (!visible) return
        if (mediaTools.has(descriptor.id) && this._iceSkyRestoring) return
        tool.onActivate?.(this)
      },
      deactivated() { stopView(this) },
      beforeDestroy() {
        for (const hook of hooks) {
          try { ownCleanup(hook.beforeDestroy?.call(this)) } catch (error) { status('error', error.message) }
        }
        releaseView(this)
      },
      watch: toolWatchers,
    }
  }

  async function activate(id, shell = root) {
    if (!shell || disposed) return null
    root = shell
    if (!store?.ready) return null
    const request = ++activation
    const epoch = contextEpoch
    root.toolLoading = true
    root.toolError = ''
    try {
      await window.AssetLoader.loadStyleOnce('css/tools.css')
      const loaded = await window.toolRegistry.load(id)
      if (request !== activation || epoch !== contextEpoch || disposed) return null
      let options = moduleOptions.get(`${epoch}/${id}`)
      if (!options) { options = Vue.extend(componentOptions(loaded)); moduleOptions.set(`${epoch}/${id}`, options) }
      root.toolComponent = options
      await root.$nextTick()
      if (request !== activation || epoch !== contextEpoch) return null
      root.toolLoading = false
      const view = views.get(id) ?? null
      if (!view || !(view.$el instanceof HTMLElement)) throw new Error(`Unable to render tool: ${loaded.descriptor.name}`)
      root.toolInstance = view
      changed(epoch)
      return view
    } catch (error) {
      if (request === activation && epoch === contextEpoch) {
        root.toolLoading = false
        root.toolError = error.message || root.uiText.loadFailed
      }
      throw error
    }
  }

  async function openTool(id, fields = {}) {
    const epoch = contextEpoch
    const previous = views.get(root.activeTab)
    if (root.activeTab !== id) { stopView(previous); root.activeTab = id; root.toolComponent = null }
    root.mobileNavOpen = false
    root.rememberRecentTool(id)
    const target = await activate(id)
    if (!target || epoch !== contextEpoch) return null
    target._iceSkyRestoring = true
    for (const [key, value] of Object.entries(fields)) if (Object.hasOwn(target.$data, key)) target[key] = value
    await target.$nextTick()
    target._iceSkyRestoring = false
    changed(epoch)
    return target
  }

  function prepare(options) {
    options.data = { ...options.data, toolComponent: null, toolInstance: null, toolLoading: true, toolError: '', contextEpoch,
      saveState: 'saved', saveMessage: '', uiText: words[configuration?.appearance?.locale === 'en' ? 'en' : 'zh'] }
    defaults = persistence.fields(options.data, persistence.sharedKeys)
    for (const key of persistence.sharedKeys) {
      if (!Object.hasOwn(store.data.shared, key)) continue
      const value = store.data.shared[key]
      if (typeof value !== typeof options.data[key] || Array.isArray(value) !== Array.isArray(options.data[key])) throw new Error(`Invalid saved workbench field: ${key}`)
      options.data[key] = value
    }
    options.data.registeredTools = window.toolRegistry.getAll()
    if (!options.data.registeredTools.some(tool => tool.id === options.data.activeTab && !tool.hidden)) options.data.activeTab = 'transforms'
    options.computed = { ...options.computed,
      currentToolName() { return this.registeredTools.find(tool => tool.id === this.activeTab)?.name ?? 'IceSky' },
      quickTools() {
        const ids = [...new Set([...this.pinnedToolIds, ...this.recentToolIds.slice(0, 3)])]
        return ids.map(id => this.registeredTools.find(tool => tool.id === id)).filter(Boolean)
      },
    }
    commonKeys = Object.keys(options.data)
    commonComputed = options.computed
    options.methods = {
      ...options.methods,
      openTool,
      async bringEntryToSampleBuilder(entry) { const target = await openTool('samplebuilder'); target?.sbBringEntry(entry) },
      getToolState(id) { const view = views.get(id); return view ? persistence.fields(view, view._iceSkyKeys) : store.data.tools?.[id]?.fields ?? {} },
      getToolView(id) { return views.get(id) ?? null },
      switchToTab(id) { void openTool(id).catch(error => this.showNotification(error.message, 'error')) },
      retryTool() { void activate(root.activeTab).catch(() => {}) },
      retrySave() { void flush().catch(() => {}) },
      exportDraft() { this.downloadTextFile('icesky-draft.json', JSON.stringify(currentSnapshot(), null, 2), 'application/json;charset=utf-8') },
      toggleToolPin(id) {
        const existing = this.pinnedToolIds.indexOf(id)
        if (existing < 0) this.pinnedToolIds.push(id)
        else this.pinnedToolIds.splice(existing, 1)
      },
      showNotification(message, kind = 'success', icon) {
        if (this._iceSkyDisposed || (this._iceSkyCopySilence > 0 && /复制|copied|copy/i.test(message) && kind === 'success')) return
        window.NotificationUtils.showNotification(message, kind, icon)
      },
      async copyEndSequence(text) { return this.copyToClipboard(text) },
      async copyGlitchToken(text) { return this.copyToClipboard(text) },
      applyLocalization() {},
      hoistOverlayPanels() {},
      setupPasteHandlers() {},
      buildDiffMarkup(...args) { return window.IceSkyDiffMethods?.buildDiffMarkup.apply(window.IceSkyDiffMethods, args) },
      triggerTransformPrimaryAction() {
        const view = views.get(root.activeTab)
        if (root.activeTab === 'decoder') { view?.runUniversalDecode(); return true }
        if (root.activeTab === 'tokenizer') { view?.runTokenizer(); return true }
        return view?.triggerTransformPrimaryAction?.() ?? false
      },
      triggerTransformCopyResult() { return views.get(root.activeTab)?.triggerTransformCopyResult?.() ?? false },
    }
    commonMethods = options.methods
    options.updated = undefined
    options.mounted = function () {
      root = this
      applyAppearance(configuration?.appearance)
      this._boundGlobalKeydown = event => {
        if (event.key === 'F5' || ((event.ctrlKey || event.metaKey) && String(event.key).toLowerCase() === 'r')) {
          event.preventDefault()
          void flush().then(() => location.reload()).catch(() => {})
          return
        }
        this.handleGlobalKeydown(event)
      }
      this._boundPaste = () => {
        this.isPasteOperation = true
        clearTimeout(this._pasteTimer)
        this._pasteTimer = setTimeout(() => { this.isPasteOperation = false }, window.CONFIG.PASTE_FLAG_RESET_DELAY_MS)
      }
      document.addEventListener('keydown', this._boundGlobalKeydown)
      this.$el.addEventListener('paste', this._boundPaste)
      for (const key of persistence.sharedKeys) sharedWatchStops.push(this.$watch(key, () => changed(), { deep: true }))
      void activate(this.activeTab).catch(() => {})
    }
    options.beforeDestroy = function () {
      document.removeEventListener('keydown', this._boundGlobalKeydown)
      this.$el.removeEventListener('paste', this._boundPaste)
      clearTimeout(this._pasteTimer)
      sharedWatchStops.forEach(stop => stop())
      sharedWatchStops = []
      for (const view of views.values()) releaseView(view)
      views.clear()
    }
    return options
  }

  async function configure(next) {
    configuration = next
    visible = next.visible !== false
    applyAppearance(next.appearance)
    const requestedScope = persistence.scopeKey(next.context)
    if (!store || store.scope !== requestedScope) {
      for (const view of views.values()) stopView(view)
      await Promise.all([...cleanupTasks])
      await root?.$nextTick()
      await flush(store)
      hydrating = true
      try {
        const candidate = new persistence.DraftStore(requestedScope, status)
        await candidate.load()
        ++contextEpoch
        ++activation
        if (root) {
          root.toolComponent = null
          root.contextEpoch = contextEpoch
          await root.$nextTick()
        }
        views.clear()
        moduleOptions.clear()
        store = candidate
        currentContext = next.context
        migrateLegacyDraft(store.data)
        if (!root) root = factory()
        else {
          for (const key of persistence.sharedKeys) root[key] = persistence.jsonValue(store.data.shared[key] ?? defaults[key], key)
          if (!window.toolRegistry.get(root.activeTab) || window.toolRegistry.get(root.activeTab).hidden) root.activeTab = 'transforms'
          await activate(root.activeTab)
        }
      } finally { hydrating = false }
    }
    visible = next.visible !== false
    const active = views.get(root?.activeTab)
    if (visible) {
      if (active && !active._iceSkyVisible) { active._iceSkyVisible = true; active._iceSkyTool.onActivate?.(active) }
    } else stopView(active)
    if (root?.toolLoading) await activate(root.activeTab)
    await root?.$nextTick()
    post({ type: 'rainy:loaded', revision: next.revision })
    if (store.dirty !== store.saved) void flush().catch(() => {})
  }

  function start(createApp) {
    factory = createApp
    window.addEventListener('message', onMessage)
    window.addEventListener('pagehide', dispose, { once: true })
    if (window.parent === window) {
      const appearance = { dark: window.matchMedia('(prefers-color-scheme: dark)').matches, fontSize: 14, codeFontSize: 13, locale: 'zh' }
      configureTail = configure({ revision: 1, context: { kind: 'standalone' }, appearance, visible: true }).catch(showStartupError)
    } else post({ type: 'rainy:ready' })
  }

  function showStartupError(error) {
    status('error', error.message)
    if (!root) {
      const app = document.getElementById('app')
      app.removeAttribute('v-cloak')
      app.replaceChildren()
      const message = document.createElement('p')
      message.setAttribute('role', 'alert')
      message.textContent = error.message
      app.appendChild(message)
    }
    post({ type: 'rainy:error', message: error.message })
  }

  function onMessage(event) {
    if (event.source !== window.parent || event.origin !== location.origin || !event.data || typeof event.data !== 'object') return
    const message = event.data
    if (message.type === 'rainy:configure' && Number.isSafeInteger(message.revision)) {
      configureTail = configureTail.catch(() => {}).then(() => configure(message)).catch(showStartupError)
    } else if (message.type === 'rainy:flush' && typeof message.id === 'string') {
      void configureTail.then(flush).then(() => post({ type: 'rainy:flushed', id: message.id, ok: true }),
        error => post({ type: 'rainy:flushed', id: message.id, ok: false, error: error.message }))
    }
  }

  function dispose() {
    disposed = true
    clearTimeout(saveTimer)
    window.removeEventListener('message', onMessage)
    root?.$destroy()
    for (const view of views.values()) releaseView(view)
    views.clear()
    sourceFiles.clear()
  }

  // These bounded legacy preference keys now share the current Host draft's ownership.
  window.IceSkyStorage = {
    getItem(key) { return legacyToolKeys.has(key) ? store?.data.shared?.toolStorage?.[key] ?? null : null },
    setItem(key, value) {
      if (!store?.ready || !legacyToolKeys.has(key)) return
      store.data.shared.toolStorage ??= {}
      store.data.shared.toolStorage[key] = String(value)
      changed()
    },
    removeItem(key) {
      if (!store?.ready || !legacyToolKeys.has(key) || !store.data.shared.toolStorage) return
      delete store.data.shared.toolStorage[key]
      changed()
    },
  }
  window.IceSkyRuntime = { prepare, start, activate, deactivate: id => stopView(views.get(id)), flush, changed, openTool,
    ownCopy(operation) {
      const task = Promise.resolve(operation).finally(() => clipboardTasks.delete(task))
      clipboardTasks.add(task)
      return task
    },
    snapshot: currentSnapshot, get context() { return currentContext }, get epoch() { return contextEpoch }, post }
})()
