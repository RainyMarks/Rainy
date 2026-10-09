/** Presents one page from the tokenizer worker without retaining the full token list. */
class TokenizerTool extends Tool {
    constructor() { super({ id: 'tokenizer', name: 'Token可视化', icon: 'fa-layer-group', title: 'Token可视化', order: 6 }); }
    getVueData() {
        return { tokenizerInput: '', tokenizerEngine: 'byte', tokenizerTokens: [], tokenizerGroups: [], tokenizerSourceText: '', tokenizerResultEngine: '', tokenizerCharCount: 0, tokenizerWordCount: 0, tokenizerSpecialCount: 0, tokenizerSpecialBreakdown: [], tokenizerTotalCount: 0, tokenizerPage: 0, tokenizerPageCount: 1, tokenizerComputing: false, tokenizerManualRequired: false, tokenizerError: '' };
    }
    getVueMethods() {
        return {
            getTokenizerCoordinator() { return this._tokenizerCoordinator || (this._tokenizerCoordinator = new window.IceSkyComputeCoordinator()); },
            clearTokenizerResult() {
                this.tokenizerTokens = []; this.tokenizerGroups = []; this.tokenizerSourceText = ''; this.tokenizerResultEngine = '';
                this.tokenizerCharCount = 0; this.tokenizerWordCount = 0; this.tokenizerSpecialCount = 0;
                this.tokenizerSpecialBreakdown = []; this.tokenizerTotalCount = 0; this.tokenizerPage = 0; this.tokenizerPageCount = 1;
            },
            acceptTokenizerPage(result) {
                this.tokenizerTokens = result.tokens; this.tokenizerGroups = result.groups;
                this.tokenizerTotalCount = result.totalCount; this.tokenizerPage = result.page; this.tokenizerPageCount = result.pageCount;
                this.tokenizerCharCount = result.charCount; this.tokenizerWordCount = result.wordCount;
                this.tokenizerSpecialCount = result.specialCount; this.tokenizerSpecialBreakdown = result.specialBreakdown;
            },
            async runTokenizer(options = {}) {
                const automatic = options.automatic === true;
                if (automatic && this._iceSkyRestoring) return;
                const input = this.tokenizerInput || '', engine = this.tokenizerEngine;
                const run = this._tokenizerRunToken = (this._tokenizerRunToken || 0) + 1;
                const coordinator = this.getTokenizerCoordinator();
                this.tokenizerManualRequired = automatic && !window.IceSkyPermitsAutomatic(input);
                this.tokenizerError = '';
                if (!input) { coordinator.cancel(); this.tokenizerComputing = false; this.clearTokenizerResult(); return; }
                this.tokenizerComputing = !this.tokenizerManualRequired;
                try {
                    const result = await coordinator.run({ kind: 'tokenizer', input, engine }, { automatic });
                    if (!result || run !== this._tokenizerRunToken) return;
                    this.acceptTokenizerPage(result); this.tokenizerSourceText = input; this.tokenizerResultEngine = engine;
                } catch (error) {
                    if (run === this._tokenizerRunToken) this.tokenizerError = error.message || 'Token 分析失败，请重试。';
                } finally { if (run === this._tokenizerRunToken) this.tokenizerComputing = false; }
            },
            scheduleTokenizer() { return this.runTokenizer({ automatic: true }); },
            cancelTokenizer() {
                this._tokenizerRunToken = (this._tokenizerRunToken || 0) + 1;
                if (this._tokenizerCoordinator) this._tokenizerCoordinator.cancel();
                this.tokenizerComputing = false;
            },
            async changeTokenizerPage(page) {
                if (this.tokenizerComputing || this.tokenizerSourceText !== this.tokenizerInput || this.tokenizerResultEngine !== this.tokenizerEngine || page < 0 || page >= this.tokenizerPageCount) return;
                const run = this._tokenizerRunToken = (this._tokenizerRunToken || 0) + 1;
                this.tokenizerComputing = true; this.tokenizerError = '';
                try {
                    const result = await this.getTokenizerCoordinator().run({ kind: 'tokenizer-page', page });
                    if (result && run === this._tokenizerRunToken) this.acceptTokenizerPage(result);
                } catch (error) { if (run === this._tokenizerRunToken) this.tokenizerError = error.message; }
                finally { if (run === this._tokenizerRunToken) this.tokenizerComputing = false; }
            },
            tokenizerPreviousPage() { return this.changeTokenizerPage(this.tokenizerPage - 1); },
            tokenizerNextPage() { return this.changeTokenizerPage(this.tokenizerPage + 1); },
        };
    }
    getVueWatchers() { return { tokenizerInput() { this.scheduleTokenizer(); }, tokenizerEngine() { this.scheduleTokenizer(); } }; }
    getVueLifecycle() { return { beforeDestroy() { this.cancelTokenizer(); if (this._tokenizerCoordinator) this._tokenizerCoordinator.dispose(); } }; }
    onActivate() {}
    onDeactivate(vm) { vm.cancelTokenizer(); }
}
if (typeof module !== 'undefined' && module.exports) module.exports = TokenizerTool;
else globalThis.TokenizerTool = TokenizerTool;
