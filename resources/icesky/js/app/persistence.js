/** Versioned text drafts backed by Rainy's authenticated Host storage. */
(() => {
  const sharedKeys = ['activeTab', 'defaultTool', 'autoCopyEnabled', 'maxHistoryItems', 'copyHistory', 'pinnedToolIds', 'recentToolIds', 'favoriteModels']
  const extraFields = new Set([
    'activeTransform', 'favorites', 'lastUsedTransforms', 'transformOptionPrefs', 'transformChain', 'transformChainOutput',
    'transformOutput', 'transformCollapsedCategories', 'randomMixSeed', 'randomMixOptionsDraft',
    'encodedMessage', 'selectedEmojis', 'igResults', 'igLastSeed', 'igBatch',
    'sb', 'sbResult', 'sbSamples', 'sbSelected', 'sbSaved', 'sbSavedId', 'sbSnapshot', 'sbBatchSnapshot',
    'fuzzerOutputs', 'pcOutputs', 'pcOutput', 'pcLastInput', 'pcLastCount', 'scCandidates', 'scOutput',
    'splitMessages', 'splitterMessageMeta', 'splitterRegroupOutput', 'dttTurns', 'dttNextId', 'dttSplitParts',
    'bijectionMapping', 'bijectionOutputs', 'bijectionPreviewEncoded', 'bijectionPreviewDecoded', 'bijectionDecodedOutput',
    'gibberishOutput', 'removalOutputs', 'removalSpecificOutput', 'decoderOutput', 'decoderResult',
    'jlDrafts', 'jlText', 'jlTitle', 'jlSelectedId', 'pitSelectedSection', 'pitSelectedId', 'pitCollapsedGroups',
  ])
  const transient = /(?:Loading|Computing|Controller|Timer|Assets|Ready|RunId|RequestPayload|RequestState|RequestInfo|DataUrl|PreviewUrl|FileUrl|PdfBytes|UploadBytes|UploadedSource|ImageUrl|Rendering|Generating|Running|Searching|SpeechStatus|Error|Destroyed|StartedAt|CompletedAt|FirstDeltaAt)$/i
  const credential = /(?:api[-_]?key|authorization|password|(?:access|refresh)[-_]?token|secret|credentials)/i

  function jsonValue(value, key = '') {
    if (credential.test(key) || transient.test(key)) return undefined
    if (value === null || typeof value === 'string' || typeof value === 'boolean') return value
    if (typeof value === 'number') return Number.isFinite(value) ? value : undefined
    if (Array.isArray(value)) return value.map(item => jsonValue(item)).filter(item => item !== undefined)
    if (!value || Object.prototype.toString.call(value) !== '[object Object]') return undefined
    const result = {}
    for (const [name, item] of Object.entries(value)) {
      if (name.startsWith('_') || ['__proto__', 'constructor', 'prototype'].includes(name)) continue
      const encoded = jsonValue(item, name)
      if (encoded !== undefined) result[name] = encoded
    }
    return result
  }

  function persistentKeys(data, template) {
    const editable = new Set([...template.matchAll(/v-model(?:\.[\w-]+)*\s*=\s*"\s*([\w$]+)/g)].map(match => match[1]))
    return Object.keys(data).filter(key => !credential.test(key) && !transient.test(key)
      && !/^tokenizer(?:TotalCount|Page|PageCount|CharCount|WordCount|SpecialCount|ResultEngine|SourceText)$/.test(key)
      && (editable.has(key) || extraFields.has(key) || ['string', 'number', 'boolean'].includes(typeof data[key])))
  }

  function fields(view, keys) {
    const result = {}
    for (const key of keys) {
      const value = jsonValue(view[key], key)
      if (value !== undefined) result[key] = value
    }
    return result
  }

  function scopeKey(context) {
    if (context?.kind === 'standalone') return 'standalone'
    if (context?.kind === 'session' && typeof context.id === 'string' && context.id) return `session:${context.id}`
    throw new Error('Invalid workbench context')
  }

  function validateData(data) {
    const record = value => value !== null && typeof value === 'object' && !Array.isArray(value)
    if (!record(data) || Object.keys(data).some(key => !['shared', 'tools', 'legacyMigrated'].includes(key))) return false
    if (data.legacyMigrated !== undefined && typeof data.legacyMigrated !== 'boolean') return false
    if (data.shared !== undefined && !record(data.shared)) return false
    if (data.tools !== undefined && !record(data.tools)) return false
    for (const tool of Object.values(data.tools ?? {})) {
      if (!record(tool) || Object.keys(tool).some(key => !['fields', 'files'].includes(key))) return false
      if ((tool.fields !== undefined && !record(tool.fields)) || (tool.files !== undefined && !record(tool.files))) return false
      for (const file of Object.values(tool.files ?? {})) {
        if (!record(file) || Object.keys(file).some(key => !['name', 'type', 'size', 'lastModified'].includes(key))) return false
        if (file.name !== undefined && typeof file.name !== 'string') return false
        if (file.type !== undefined && typeof file.type !== 'string') return false
        if (file.size !== undefined && (!Number.isFinite(file.size) || file.size < 0)) return false
        if (file.lastModified !== undefined && (!Number.isFinite(file.lastModified) || file.lastModified < 0)) return false
      }
    }
    return true
  }

  class DraftStore {
    constructor(scope, status) {
      this.scope = scope
      this.status = status
      this.revision = 0
      this.data = {}
      this.dirty = 0
      this.saved = 0
      this.tail = Promise.resolve()
      this.ready = false
    }

    async load() {
      this.ready = false
      const response = await fetch(`/rainy/icesky/state?scope=${encodeURIComponent(this.scope)}`, { cache: 'no-store' })
      const value = await response.json()
      if (!response.ok) throw new Error(value.error?.message ?? value.error ?? 'Unable to read the saved draft')
      if (value.version !== 1 || !Number.isSafeInteger(value.revision) || value.revision < 0 || !validateData(value.data)) throw new Error('Unsupported saved draft format')
      this.revision = value.revision
      this.data = value.data
      this.data.tools ??= {}
      this.data.shared ??= {}
      this.ready = true
      return this.data
    }

    changed() { this.dirty++ }

    save(snapshot) {
      const run = async () => {
        if (!this.ready) throw new Error('The saved draft has not been loaded')
        if (this.dirty === this.saved) return
        const generation = this.dirty
        const data = jsonValue(snapshot())
        this.status('saving')
        const response = await fetch(`/rainy/icesky/state?scope=${encodeURIComponent(this.scope)}`, {
          method: 'PUT', headers: { 'content-type': 'application/json' },
          body: JSON.stringify({ baseRevision: this.revision, data }),
        })
        const result = await response.json()
        if (!response.ok) throw new Error(response.status === 409 ? 'The draft was changed in another window. Export your unsaved text before reloading.' : result.error?.message ?? result.error ?? 'Unable to save the draft')
        if (result.version !== 1 || result.revision !== this.revision + 1 || !validateData(result.data)) throw new Error('Invalid save confirmation')
        this.revision = result.revision
        this.saved = generation
        if (this.dirty === generation) this.data = data
        this.status(this.dirty === generation ? 'saved' : 'saving')
      }
      const result = this.tail.then(run)
      this.tail = result.catch(() => {})
      return result.catch(error => { this.status('error', error.message); throw error })
    }
  }

  window.IceSkyPersistence = { sharedKeys, persistentKeys, fields, jsonValue, scopeKey, validateData, DraftStore }
})()
