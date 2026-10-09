function sanitizeVueMountRoot() {
  const e = document.getElementById("app");
  e && e.querySelectorAll("script").forEach((e2) => e2.remove());
}
function readStorage(e) {
  try {
    return localStorage.getItem(e);
  } catch (e2) {
    return null;
  }
}
function readJSONStorage(e, t) {
  try {
    const o = localStorage.getItem(e);
    if (!o) return t;
    const i = JSON.parse(o);
    return null == i ? t : i;
  } catch (e2) {
    return t;
  }
}
function normalizeHistoryLimit(e) {
  return Math.max(10, Math.min(500, Math.trunc(Number(e)) || window.CONFIG.MAX_HISTORY_ITEMS));
}
sanitizeVueMountRoot();
const baseData = { activeTab: readStorage("default-tool") || "injectiongenerator", registeredTools: [], toolSearchQuery: "", toolGroupFilter: "all", mobileNavOpen: false, pinnedToolIds: readJSONStorage("pinned-tool-ids", []), recentToolIds: readJSONStorage("recent-tool-ids", []), universalDecodeInput: "", universalDecodeResult: null, isPasteOperation: false, lastCopyTime: 0, ignoreKeyboardEvents: false, isTransformCopy: false, keyboardEventsTimeout: null, showDecoder: true, tbCarrierManual: "", copyHistory: [], copyHistorySearch: "", maxHistoryItems: normalizeHistoryLimit(readStorage("max-history-items")), showCopyHistory: false, showUnicodePanel: false, unicodePanelToggleLock: false, showDangerModal: false, dangerThresholdTokens: window.CONFIG.DANGER_THRESHOLD_TOKENS, showGlitchTokenPanel: false, showEndSequencePanel: false, showCommandPalette: false, commandQuery: "", commandActiveIndex: 0, endSequenceCategories: "undefined" != typeof window && window.END_SEQUENCE_CATEGORIES ? window.END_SEQUENCE_CATEGORIES : [], glitchTokensLoaded: false, glitchTokenBehavior: "", glitchTokenSearch: "", filteredGlitchTokens: [], allGlitchTokens: [], openaiApiKey: readStorage("openai-api-key") || readStorage("openai_api_key") || readStorage("openrouter-api-key") || readStorage("openrouter_api_key") || "", openaiBaseUrl: readStorage("openai-base-url") || readStorage("openai_base_url") || "", anthropicApiKey: readStorage("anthropic-api-key") || readStorage("anthropic_api_key") || "", anthropicBaseUrl: readStorage("anthropic-base-url") || readStorage("anthropic_base_url") || "", showApiKey: false, showAnthropicApiKey: false, apiKeySaved: false, openaiUrlSaved: false, anthropicSaved: false, defaultTool: readStorage("default-tool") || "injectiongenerator", autoCopyEnabled: "false" !== readStorage("auto-copy-enabled"), modelSearch: "", modelProviderFilter: "all", favoriteModels: readJSONStorage("favorite-models", []), openaiModelsLastUpdated: readStorage("openai-models-last-updated") || "", anthropicModelsLastUpdated: readStorage("anthropic-models-last-updated") || "", connectionTests: { openai: null, anthropic: null }, connectionTestsRunning: false, compareModalOpen: false, compareTitle: "", compareLeftLabel: "原文", compareRightLabel: "结果", compareLeftText: "", compareRightText: "", compareDiffEnabled: false, compareDiffSourceHtml: "", compareDiffOutputHtml: "", compareDiffChangedCount: 0, availableModels: "undefined" != typeof window && Array.isArray(window.AI_MODELS) ? window.AI_MODELS : [], openaiModelsLoading: false, openaiModelsError: "", anthropicModelsLoading: false, anthropicModelsError: "" }, toolData = window.toolRegistry && "function" == typeof window.toolRegistry.mergeVueData ? window.toolRegistry.mergeVueData() : {}, mergedData = Object.assign({}, baseData, toolData), toolMethods = window.toolRegistry && "function" == typeof window.toolRegistry.mergeVueMethods ? window.toolRegistry.mergeVueMethods() : {}, appModels = window.createAppModels(readStorage);
window.IceSkyRuntime.start(() => {
  window.app = new Vue(window.IceSkyRuntime.prepare({ el: "#app", data: mergedData, computed: { ...window.AppHistory.computed, ...appModels.computed, ...window.AppCommands.computed, ...window.AppNavigation.computed }, methods: Object.assign({}, toolMethods || {}, window.AppHistory.methods, appModels.methods, window.AppNavigation.methods, window.AppCommands.methods, { sanitizeFileName: (e, t = "export") => (String(e || "").trim() || t).replace(/[<>:"/\\|?*\x00-\x1F]+/g, "-").slice(0, 80), downloadTextFile(e, t, o = "text/plain;charset=utf-8") {
    const i = new Blob([(o.startsWith("application/json") ? "" : "\uFEFF") + String(t || "")], { type: o }), a = URL.createObjectURL(i);
    let n;
    try {
      n = document.createElement("a"), n.href = a, n.download = e, document.body.appendChild(n), n.click();
    } finally {
      setTimeout(() => URL.revokeObjectURL(a), 1200), n && n.parentNode && n.parentNode.removeChild(n);
    }
  }, hoistOverlayPanels() {
    const e = this.$el && "function" == typeof this.$el.querySelectorAll ? this.$el : document.getElementById("app");
    if (!e || "function" != typeof e.appendChild || "function" != typeof e.querySelectorAll) return;
    const t = e;
    [".copy-history-panel", ".glitch-token-panel", ".end-sequence-panel", "#unicode-options-panel", ".command-palette-backdrop", ".compare-modal-backdrop"].forEach((e2) => {
      t.querySelectorAll(e2).forEach((e3) => {
        e3 && e3.parentElement !== t && t.appendChild(e3);
      });
    });
  }, openComparisonModal(e = {}) {
    if (this.compareTitle = e.title || "结果对比", this.compareLeftLabel = e.leftLabel || "原文", this.compareRightLabel = e.rightLabel || "结果", this.compareLeftText = String(e.leftText || ""), this.compareRightText = String(e.rightText || ""), this.compareDiffEnabled = false, this.compareDiffSourceHtml = "", this.compareDiffOutputHtml = "", this.compareDiffChangedCount = 0, e.enableDiff && "function" == typeof this.buildDiffMarkup) try {
      const e2 = this.buildDiffMarkup(this.compareLeftText, this.compareRightText);
      this.compareDiffEnabled = Boolean(e2 && (e2.sourceHtml || e2.outputHtml)), this.compareDiffSourceHtml = e2 && e2.sourceHtml ? e2.sourceHtml : "", this.compareDiffOutputHtml = e2 && e2.outputHtml ? e2.outputHtml : "", this.compareDiffChangedCount = e2 && Number.isFinite(Number(e2.changedCount)) ? Number(e2.changedCount) : 0;
    } catch (e2) {
      this.compareDiffEnabled = false;
    }
    this.compareModalOpen = true;
  }, closeComparisonModal() {
    this.compareModalOpen = false;
  }, async ensureGlitchTokensLoaded() {
    if (window.loadGlitchTokens && window.getAllGlitchTokens) return true;
    if (!window.AssetLoader || "function" != typeof window.AssetLoader.loadScriptOnce) throw new Error("异常token加载器不可用。");
    return await window.AssetLoader.loadScriptOnce("js/utils/glitchTokens.js"), Boolean(window.loadGlitchTokens && window.getAllGlitchTokens);
  }, saveGeneralSettings() {
    this.maxHistoryItems = normalizeHistoryLimit(this.maxHistoryItems);
    try {
      localStorage.setItem("default-tool", this.defaultTool), localStorage.setItem("auto-copy-enabled", String(this.autoCopyEnabled)), localStorage.setItem("max-history-items", String(this.maxHistoryItems));
    } catch (e) {
      return void this.showNotification("浏览器存储不可用，设置仅在当前页面生效。", "warning");
    }
    this.showNotification("通用设置已保存。", "success", "fas fa-sliders-h");
  }, toggleUnicodePanel(e) {
    if (this.unicodePanelToggleLock) return;
    this.unicodePanelToggleLock = true, this.hoistOverlayPanels(), this.showUnicodePanel = !this.showUnicodePanel;
    const t = document.getElementById("unicode-options-panel");
    t && (this.showUnicodePanel ? t.classList.add("active") : t.classList.remove("active")), setTimeout(() => {
      this.unicodePanelToggleLock = false;
    }, 300);
  }, focusWithoutScroll(e) {
    window.FocusUtils.focusWithoutScroll(e);
  }, applyLocalization() {
    this.$nextTick(() => {
      window.LocalizationUtils && "function" == typeof window.LocalizationUtils.apply && window.LocalizationUtils.apply(this);
    });
  }, toggleGlitchTokenPanel(e) {
    this.hoistOverlayPanels(), this.showGlitchTokenPanel = !this.showGlitchTokenPanel, this.showGlitchTokenPanel && !this.glitchTokensLoaded && this.loadGlitchTokens();
  }, toggleEndSequencePanel() {
    this.hoistOverlayPanels(), this.showEndSequencePanel = !this.showEndSequencePanel;
  }, async copyEndSequence(e) {
    e && await this.copyToClipboard(e) && this.showNotification("已复制", "success", "fas fa-copy");
  }, async loadGlitchTokens() {
    if (!this.glitchTokensLoaded) try {
      await this.ensureGlitchTokensLoaded(), window.loadGlitchTokens && await window.loadGlitchTokens(), window.getAllGlitchTokens && (this.allGlitchTokens = window.getAllGlitchTokens(), this.filterGlitchTokens(), this.glitchTokensLoaded = true);
    } catch (e) {
      console.error("Error loading glitch tokens:", e), this.showNotification("加载异常token失败", "error", "fas fa-exclamation-triangle");
    }
  }, filterGlitchTokens() {
    let e = this.allGlitchTokens;
    if (this.glitchTokenBehavior && (e = e.filter((e2) => e2.behavior === this.glitchTokenBehavior)), this.glitchTokenSearch) {
      const t = this.glitchTokenSearch.toLowerCase();
      e = e.filter((e2) => {
        const o = (e2.token || "").toLowerCase(), i = (e2.origin || "").toLowerCase(), a = (e2.observed_output || "").toLowerCase(), n = String(e2.token_id ?? "");
        return o.includes(t) || i.includes(t) || a.includes(t) || n.includes(t);
      });
    }
    this.filteredGlitchTokens = e;
  }, async copyGlitchToken(e) {
    e && await this.copyToClipboard(e) && this.showNotification("异常token已复制", "success", "fas fa-copy");
  }, showNotification(e, t = "success", o = null) {
    const i = window.LocalizationUtils && "function" == typeof window.LocalizationUtils.translateMessage ? window.LocalizationUtils.translateMessage(e) : e;
    window.NotificationUtils.showNotification(i, t, o);
  }, showCopiedPopup() {
    window.NotificationUtils.showCopiedPopup();
  }, setupPasteHandlers() {
    document.querySelectorAll("textarea").forEach((e) => {
      e.addEventListener("paste", (e2) => {
        this.isPasteOperation = true, setTimeout(() => {
          this.isPasteOperation = false;
        }, window.CONFIG.PASTE_FLAG_RESET_DELAY_MS);
      });
    });
  } }), mounted() {
    if (window.ThemeUtils && window.ThemeUtils.initializeTheme && window.ThemeUtils.initializeTheme(), window.toolRegistry && "function" == typeof window.toolRegistry.mergeVueLifecycle) {
      const e2 = window.toolRegistry.mergeVueLifecycle();
      e2 && e2.mounted && e2.mounted.call(this);
    }
    window.toolRegistry && "function" == typeof window.toolRegistry.getAll && (this.registeredTools = window.toolRegistry.getAll());
    const e = new Set((this.registeredTools || []).filter((e2) => !e2.hidden).map((e2) => e2.id));
    e.has(this.activeTab) || (this.activeTab = e.has(this.defaultTool) ? this.defaultTool : "transforms"), e.has(this.defaultTool) ? this.activeTab = this.defaultTool : this.defaultTool = this.activeTab, "transforms" !== this.activeTab ? this.switchToTab(this.activeTab) : (this.rememberRecentTool(this.activeTab), window.toolRegistry && window.toolRegistry.activateTool(this.activeTab, this)), this.syncOpenAIModels(window.OPENAI_PROVIDER_MODELS || []), this.syncAnthropicModels(window.ANTHROPIC_PROVIDER_MODELS || []), this.applyLocalization(), (this.openaiApiKey || "").trim() && this.refreshOpenAIModels({ apiKey: this.openaiApiKey, silent: true }).catch(() => {
    }), (this.anthropicApiKey || "").trim() && this.refreshAnthropicModels({ apiKey: this.anthropicApiKey, silent: true }).catch(() => {
    }), this._boundGlobalKeydown = this.handleGlobalKeydown.bind(this), document.addEventListener("keydown", this._boundGlobalKeydown), this.$nextTick(() => {
      this.hoistOverlayPanels();
      const e2 = document.querySelector("#unicode-options-panel .close-button");
      if (e2) {
        const handleClose = (e3) => {
          e3.preventDefault(), e3.stopPropagation(), this.toggleUnicodePanel(e3);
        };
        e2.addEventListener("click", handleClose, { passive: false }), e2.addEventListener("touchend", handleClose, { passive: false });
      }
    }), document.addEventListener("click", (e2) => {
      if (e2.target.closest(".custom-tooltip")) return;
      const t = e2.target.closest(".tooltip-icon");
      if (t) {
        e2.preventDefault(), e2.stopPropagation();
        const o = t.getAttribute("data-tooltip");
        if (!o) return;
        const i = document.querySelector(".custom-tooltip.active");
        return i && i.textContent === o ? (i.classList.remove("active"), void setTimeout(() => {
          i.classList.contains("active") || i.remove();
        }, 200)) : (document.querySelectorAll(".custom-tooltip.active").forEach((e3) => {
          e3.classList.remove("active"), setTimeout(() => {
            e3.classList.contains("active") || e3.remove();
          }, 200);
        }), void setTimeout(() => {
          const e3 = document.createElement("div");
          e3.className = "custom-tooltip active", e3.textContent = o, document.body.appendChild(e3);
          const i2 = t.getBoundingClientRect();
          e3.style.left = i2.left + i2.width / 2 + "px", e3.style.top = i2.top - e3.offsetHeight - 8 + "px", e3.style.transform = "translateX(-50%)";
        }, 10));
      }
      e2.target.closest("#unicode-options-panel") || document.querySelectorAll(".custom-tooltip.active").forEach((e3) => {
        e3.classList.remove("active"), setTimeout(() => {
          e3.classList.contains("active") || e3.remove();
        }, 200);
      });
    }), this.$nextTick(() => {
      this.setupPasteHandlers(), this.applyLocalization();
    });
  }, updated() {
    this._localizationScheduled || (this._localizationScheduled = true, this.$nextTick(() => {
      this.hoistOverlayPanels(), this._localizationScheduled = false, window.LocalizationUtils && "function" == typeof window.LocalizationUtils.localizeDom && window.LocalizationUtils.localizeDom(document.body);
    }));
  }, created() {
    if (window.toolRegistry && "function" == typeof window.toolRegistry.mergeVueLifecycle) {
      const e = window.toolRegistry.mergeVueLifecycle();
      e && e.created && e.created.call(this);
    }
  }, beforeDestroy() {
    if (this._boundGlobalKeydown && (document.removeEventListener("keydown", this._boundGlobalKeydown), this._boundGlobalKeydown = null), window.toolRegistry && "function" == typeof window.toolRegistry.mergeVueLifecycle) {
      const e = window.toolRegistry.mergeVueLifecycle();
      e && e.beforeDestroy && e.beforeDestroy.call(this);
    }
  }, watch: Object.assign({}, window.toolRegistry && "function" == typeof window.toolRegistry.mergeVueWatchers ? window.toolRegistry.mergeVueWatchers() : {}, { defaultTool(e) {
    if (e) try {
      localStorage.setItem("default-tool", e);
    } catch (e2) {
    }
  }, maxHistoryItems(e) {
    const t = normalizeHistoryLimit(e);
    this.maxHistoryItems !== t && (this.maxHistoryItems = t);
    try {
      localStorage.setItem("max-history-items", String(t));
    } catch (e2) {
    }
    Array.isArray(this.copyHistory) && this.copyHistory.length > t && this.copyHistory.splice(t);
  }, commandQuery() {
    this.commandActiveIndex = 0;
  } }) }));
  return window.app;
});
