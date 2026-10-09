/** Tool metadata is eager; implementations and Vue components load on first selection. */
(() => {
  const dependencies = {
    transforms: ['js/utils/emoji.js', 'js/core/transformOptions.js', 'js/utils/computeCoordinator.js', 'js/tools/TranslateTool.js'],
    decoder: ['js/core/transformOptions.js', 'js/core/decoder.js', 'js/utils/computeCoordinator.js'],
    tokenizer: ['js/utils/tokenizer.js', 'js/utils/computeCoordinator.js'],
    steganography: ['js/utils/emoji.js', 'js/core/steganography.js'],
    asciismuggler: ['js/utils/asciiSmuggler.js'],
    tokenade: ['js/utils/emoji.js', 'js/core/steganography.js', 'js/utils/tokenizer.js'],
    splitter: ['js/utils/tokenizer.js', 'js/utils/transformRuntime.js'],
    fuzzer: ['js/utils/transformRuntime.js', 'js/utils/diff.js'],
    imageinject: ['js/utils/diff.js'],
    pdfinject: ['js/tools/pdfinject/config.js', 'js/tools/pdfinject/library.js'],
    samplebuilder: ['js/utils/emoji.js', 'js/core/steganography.js', 'js/utils/asciiSmuggler.js'],
  }
  class ToolRegistry {
    constructor() {
      this.catalog = window.ICE_SKY_TOOLS
      this.implementations = new Map()
      this.loading = new Map()
    }
    get(id) { return this.implementations.get(id) ?? this.catalog.find(tool => tool.id === id) ?? null }
    getAll() { return this.catalog }
    getEnabled() { return this.catalog.filter(tool => tool.enabled) }
    // Shared UI lives in the shell; each tool's Vue options stay in its component.
    mergeVueData() { return {} }
    mergeVueMethods() { return {} }
    mergeVueWatchers() { return {} }
    mergeVueLifecycle() { return {} }
    async load(id) {
      const descriptor = this.catalog.find(tool => tool.id === id && !tool.hidden)
      if (!descriptor) throw new Error(`Unknown tool: ${id}`)
      if (this.loading.has(id)) return this.loading.get(id)
      const pending = (async () => {
        await window.AssetLoader.loadScriptsSequentially([...(dependencies[id] ?? []), descriptor.script])
        const Constructor = window[descriptor.className]
        if (typeof Constructor !== 'function') throw new Error(`Tool module did not register: ${id}`)
        const tool = new Constructor()
        const template = await window.AssetLoader.loadTextOnce(descriptor.template)
        this.implementations.set(id, tool)
        return { tool, descriptor, template }
      })().catch(error => { this.loading.delete(id); throw error })
      this.loading.set(id, pending)
      return pending
    }
    activateTool(id, shell) { return window.IceSkyRuntime.activate(id, shell) }
    deactivateTool(id) { window.IceSkyRuntime.deactivate(id) }
  }
  window.ToolRegistry = ToolRegistry
  window.toolRegistry = new ToolRegistry()
})()
