class DecodeTool extends Tool {
    constructor() { super({ id: "decoder", name: "解码", icon: "fa-key", title: "解码", order: 10 }), this.scriptRanges = [{ name: "Arabic", re: /[\u0600-\u06FF\u0750-\u077F\u08A0-\u08FF\uFB50-\uFDFF\uFE70-\uFEFF]/ }, { name: "Chinese", re: /[\u4E00-\u9FFF\u3400-\u4DBF]/ }, { name: "Japanese", re: /[\u3040-\u309F\u30A0-\u30FF\u31F0-\u31FF]/ }, { name: "Korean", re: /[\uAC00-\uD7AF\u1100-\u11FF\u3130-\u318F]/ }, { name: "Cyrillic", re: /[\u0400-\u04FF\u0500-\u052F]/ }, { name: "Devanagari", re: /[\u0900-\u097F]/ }, { name: "Thai", re: /[\u0E00-\u0E7F]/ }, { name: "Hebrew", re: /[\u0590-\u05FF\uFB1D-\uFB4F]/ }, { name: "Greek", re: /[\u0370-\u03FF\u1F00-\u1FFF]/ }, { name: "Tamil", re: /[\u0B80-\u0BFF]/ }, { name: "Bengali", re: /[\u0980-\u09FF]/ }, { name: "Georgian", re: /[\u10A0-\u10FF\u2D00-\u2D2F]/ }, { name: "Armenian", re: /[\u0530-\u058F]/ }, { name: "Ethiopic", re: /[\u1200-\u137F]/ }, { name: "Tibetan", re: /[\u0F00-\u0FFF]/ }, { name: "Khmer", re: /[\u1780-\u17FF]/ }, { name: "Lao", re: /[\u0E80-\u0EFF]/ }, { name: "Myanmar", re: /[\u1000-\u109F]/ }, { name: "Sinhala", re: /[\u0D80-\u0DFF]/ }, { name: "Telugu", re: /[\u0C00-\u0C7F]/ }, { name: "Kannada", re: /[\u0C80-\u0CFF]/ }, { name: "Malayalam", re: /[\u0D00-\u0D7F]/ }, { name: "Gujarati", re: /[\u0A80-\u0AFF]/ }, { name: "Gurmukhi", re: /[\u0A00-\u0A7F]/ }], this.latinLangMarkers = [{ name: "Spanish", markers: /\b(el|la|los|las|de|del|en|con|por|para|que|una?|es|está|son|como|pero|más|tiene|esta|puede|este|cada|desde|según|también|porque|entre|ya|muy|otro|otra|sobre|después|mismo|donde|cuando|hasta|aquí|ser|hacer|tiene|todas?|todos?|nos|nuestro|hemos)\b/i }, { name: "French", markers: /\b(le|la|les|des|une?|est|sont|avec|dans|pour|sur|pas|que|qui|cette?|mais|nous|vous|leur|très|être|avoir|faire|tout|comme|ses|aux|peut|aussi|plus|encore|même|entre|après|sans|ici|notre|autre|deux|bien)\b/i }, { name: "German", markers: /\b(der|die|das|ein|eine|ist|sind|und|oder|für|mit|auf|nicht|von|den|dem|des|sich|kann|werden|wird|haben|sein|auch|nach|über|wie|noch|aber|wenn|nur|mehr|schon|hier|sehr|alle|diese[rms]?|jede[rms]?|mein|dein)\b/i }, { name: "Portuguese", markers: /\b(o|os|uma?|uns|umas|é|são|com|em|para|por|que|não|como|mas|mais|tem|está|pode|este|esta|cada|desde|também|porque|entre|muito|outro|outra|sobre|depois|mesmo|onde|quando|até|aqui|ser|fazer|nosso|nossa|todos|todas)\b/i }, { name: "Italian", markers: /\b(il|lo|la|gli|le|un|una|è|sono|di|del|della|in|con|per|che|non|come|ma|più|ha|sta|può|questo|questa|ogni|anche|perché|tra|fra|molto|altro|altra|dopo|stesso|dove|quando|fino|qui|essere|fare|nostro|nostra|tutti|tutte)\b/i }, { name: "Dutch", markers: /\b(de|het|een|is|zijn|en|of|voor|met|op|niet|van|dat|die|maar|ook|als|kan|worden|wordt|heeft|nog|naar|bij|uit|tot|wel|veel|meer|deze|alle|dit|wat|hoe|waar|hier|zeer|ons|onze|hun)\b/i }, { name: "Turkish", markers: /\b(bir|ve|bu|için|ile|var|olan|gibi|daha|çok|ama|ancak|sonra|değil|olarak|kadar|hem|her|bütün|hiç|nasıl|neden|nere[dy]e|şimdi|zaman|büyük|küçük|iyi|kötü|yeni|eski)\b/i }, { name: "Polish", markers: /\b(jest|nie|się|na|to|za|ale|jak|już|tak|czy|może|tylko|jeszcze|bardzo|jego|jej|ich|ten|tego|więc|przez|pod|nad|między|tutaj|teraz|zawsze|nigdy|każdy|wszystko)\b/i }, { name: "Vietnamese", markers: /\b(là|và|của|có|được|không|một|những|các|này|cho|đã|với|người|trong|từ|đến|về|theo|như|khi|nếu|nhưng|cũng|rất|nhiều|hay|bởi|tại|đây|nào)\b/i }, { name: "Indonesian", markers: /\b(dan|yang|di|ini|itu|untuk|dengan|dari|tidak|adalah|pada|ke|juga|akan|sudah|ada|oleh|karena|mereka|kami|bisa|harus|lebih|sangat|satu|dua|banyak|semua|setiap|atau)\b/i }, { name: "Swahili", markers: /\b(na|ya|wa|ni|kwa|katika|hii|hiyo|lakini|pia|sana|mtu|watu|nyumba|kazi|nchi|jambo|mambo|habari|rafiki|asante|karibu|kwamba|ambaye|kila|yote)\b/i }, { name: "Romanian", markers: /\b(este|sunt|și|sau|pentru|cu|în|din|la|pe|nu|care|acest|această|dar|mai|poate|aici|acolo|foarte|toate|fiecare|nostru|noastră|după|când|unde|cum|despre|între)\b/i }]; }
    detectLanguage(e) { if (!e || e.length < 8)
        return null; var t = e.replace(/[\x00-\x1F\x7F-\x9F]/g, "").trim(); if (!t)
        return null; for (var a = 0; a < this.scriptRanges.length; a++) {
        var r = this.scriptRanges[a], n = t.match(new RegExp(r.re.source, "g"));
        if (n && n.length >= 3)
            if (n.length / t.replace(/\s/g, "").length > .3)
                return { detected: !0, language: r.name, confidence: "high" };
    } var s = t.match(/[a-zA-ZÀ-ÿ]/g); if (!s || s.length / t.replace(/\s/g, "").length < .5)
        return null; var o = t.split(/\s+/).filter(function (e) { return e.length > 0; }); if (o.length < 3)
        return null; var i = t.match(/\b(the|is|are|was|were|have|has|had|will|would|could|should|can|do|does|did|this|that|these|those|with|from|they|their|them|been|being|which|where|when|what|who|how|but|and|not|for|all|any|our|your|its|his|her|some|into|very|just|about|then|than|more|also|here|each|every|only|most|both|such|much|many|other|after|before|between|under|over|again|once|during|without)\b/gi); if ((i ? i.length / o.length : 0) > .15)
        return null; for (var d = null, u = 0, c = 0; c < this.latinLangMarkers.length; c++) {
        var l = this.latinLangMarkers[c], h = t.match(new RegExp(l.markers.source, "gi"));
        if (h) {
            var m = h.length / o.length;
            m > u && (u = m, d = l.name);
        }
    } return d && u > .1 ? { detected: !0, language: d, confidence: u > .25 ? "high" : "medium" } : null; }
    getVueData() { return { decoderAssetsReady: false, decoderAssetsLoading: false, decoderRunId: 0, decoderAssetsError: '', decoderInput: '', decoderOutput: '', decoderResult: null, selectedDecoder: 'auto', decoderLangDetected: null, decoderTranslating: false, decoderTranslationRunId: 0, decoderTranslationController: null, decoderTranslateError: '', decoderTransforms: [], decoderComputing: false, decoderManualRequired: false, decoderError: '' }; }
    getVueMethods() { return {
    getDecoderCoordinator() { return this._decoderCoordinator || (this._decoderCoordinator = new window.IceSkyComputeCoordinator()); },
    async ensureDecoderAssetsLoaded() {
        if (this.decoderAssetsReady) return true;
        if (this._decoderAssetsPromise) return this._decoderAssetsPromise;
        this.decoderAssetsLoading = true; this.decoderAssetsError = '';
        const catalogCoordinator = new window.IceSkyComputeCoordinator();
        this._decoderCatalogCoordinator = catalogCoordinator;
        this._decoderAssetsPromise = catalogCoordinator.run({kind: 'decoder-catalog'}).then(catalog => {
            if (!catalog) return false;
            this.decoderTransforms = catalog; this.decoderAssetsReady = true; return true;
        }).catch(error => { this.decoderAssetsError = error.message; return false; }).finally(() => { catalogCoordinator.dispose(); this._decoderCatalogCoordinator = null; this.decoderAssetsLoading = false; this._decoderAssetsPromise = null; });
        return this._decoderAssetsPromise;
    },
    getAllTransformsWithReverse() { return this.decoderTransforms; },
    async runUniversalDecode(options = {}) {
        const automatic = options.automatic === true;
        if (automatic && this._iceSkyRestoring) return;
        this.cancelDecoderTranslation();
        const run = ++this.decoderRunId, input = this.decoderInput || '', decoder = this.selectedDecoder;
        const coordinator = this.getDecoderCoordinator();
        this.decoderLangDetected = null; this.decoderTranslateError = ''; this.decoderError = '';
        this.decoderManualRequired = automatic && !window.IceSkyPermitsAutomatic(input);
        if (!input) { coordinator.cancel(); this.decoderOutput = ''; this.decoderResult = null; this.decoderComputing = false; return; }
        this.decoderComputing = !this.decoderManualRequired;
        const preferences = this.getToolState('transforms').transformOptionPrefs || {};
        try {
            const result = await coordinator.run({ kind: 'decoder', input, decoder, optionsByName: preferences, emojiDataReady: Boolean(window.emojiData) }, { automatic });
            if (!result || run !== this.decoderRunId) return;
            this.decoderResult = result.decoded; this.decoderOutput = result.decoded ? result.decoded.text : ''; this.decoderLangDetected = result.language;
        } catch (error) { if (run === this.decoderRunId) this.decoderError = error.message || '解码失败，请重试。'; }
        finally { if (run === this.decoderRunId) this.decoderComputing = false; }
    },
    scheduleUniversalDecode() { return this.runUniversalDecode({ automatic: true }); },
    cancelDecoderTranslation() {
    ++this.decoderTranslationRunId;
    const controller = this.decoderTranslationController;
    this.decoderTranslationController = null;
    if (controller) controller.abort();
    this.decoderTranslating = false;
},
cancelDecoder() { ++this.decoderRunId; this.cancelDecoderTranslation(); if (this._decoderCoordinator) this._decoderCoordinator.cancel(); this.decoderComputing = false; },
    async decoderTranslateToEnglish() {
    if (this.decoderTranslating || this._iceSkyDisposed || this._isDestroyed) return;
    let apiKey = '';
    try {
        const storage = window.IceSkyStorage || localStorage;
        apiKey = (storage.getItem('openai-api-key') || storage.getItem('openai_api_key') || storage.getItem('openrouter-api-key') || storage.getItem('plinyos-api-key') || storage.getItem('openrouter_api_key') || '').trim();
    } catch (error) { /* Host-provided credentials remain available when browser storage is unavailable. */ }
    if (!apiKey && this.openaiApiKey) apiKey = this.openaiApiKey.trim();
    const text = this.decoderResult?.text || this.decoderInput;
    const language = this.decoderLangDetected ? this.decoderLangDetected.language : '未知语言';
    const model = this.getToolState('transforms').translateModel || (Array.isArray(window.OPENAI_MODELS) && window.OPENAI_MODELS[0] ? window.OPENAI_MODELS[0].id : '');
    this.decoderTranslateError = '';
    if (this.availableModelsLoading) { this.decoderTranslateError = '可用模型仍在获取中，请稍后再试。'; return; }
    if (!model) { this.decoderTranslateError = '请先在设置中连接模型，并获取可用模型。'; return; }
    if (!text.trim()) return;
    const input = this.decoderInput, previousResult = this.decoderResult;
    const run = ++this.decoderTranslationRunId;
    const controller = new AbortController();
    this.decoderTranslationController = controller;
    this.decoderTranslating = true;
    const current = () => run === this.decoderTranslationRunId && !controller.signal.aborted && !this._iceSkyDisposed && !this._isDestroyed;
    const sameSource = () => current() && input === this.decoderInput && previousResult === this.decoderResult;
    try {
        if (typeof this.ensureOpenAIClientLoaded === 'function') await this.ensureOpenAIClientLoaded();
        if (!sameSource()) return;
        if (!window.OpenAIClient || typeof window.OpenAIClient.chatCompletion !== 'function') throw new Error('AI客户端尚未就绪。');
        const response = await window.OpenAIClient.chatCompletion({
            model,
            messages: [
                {role: 'system', content: 'You are a professional translator. Translate the following text to English. Output ONLY the English translation. No explanations, notes, or alternatives. Preserve formatting, line breaks, and structure.'},
                {role: 'user', content: 'Translate this ' + language + ' text to English:\n\n' + text},
            ],
            temperature: .2, max_completion_tokens: 4096,
        }, {apiKey, context: this, signal: controller.signal});
        if (!sameSource()) return;
        const translated = window.OpenAIClient.extractMessage(response);
        if (!translated) { this.decoderTranslateError = '模型未返回翻译结果。'; return; }
        this.decoderOutput = translated;
        this.decoderResult = {text: translated, method: language + ' → English (AI)', alternatives: previousResult?.alternatives || []};
        if (current() && input === this.decoderInput) await this.copyToClipboard(translated);
    } catch (error) {
        if (sameSource()) this.decoderTranslateError = '翻译失败：' + error.message;
    } finally {
        if (run === this.decoderTranslationRunId) { this.decoderTranslating = false; this.decoderTranslationController = null; }
    }
},
useAlternative: function (e) { e && e.text && (this.decoderOutput = e.text, this.decoderResult = { method: e.method, text: e.text, alternatives: (this.decoderResult && this.decoderResult.alternatives || []).filter(t => t.method !== e.method) }); }
}; }
    getVueWatchers() { return { decoderInput() { this.scheduleUniversalDecode(); }, selectedDecoder() { this.scheduleUniversalDecode(); } }; }
getVueLifecycle() { return { beforeDestroy() { this.cancelDecoder(); if (this._decoderCoordinator) this._decoderCoordinator.dispose(); if (this._decoderCatalogCoordinator) this._decoderCatalogCoordinator.dispose(); } }; }
    onActivate(vm) { vm.ensureDecoderAssetsLoaded(); }
onDeactivate(vm) { vm.cancelDecoder(); }
}
"undefined" != typeof module && module.exports ? module.exports = DecodeTool : window.DecodeTool = DecodeTool;
