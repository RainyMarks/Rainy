/** Shares the locally packaged text-transform runtime without mounting another tool. */
globalThis.IceSkyTransformRuntime = (() => {
    let loading = null;

    function current() {
        return Object.values(globalThis.transforms || {}).filter(transform => transform && transform.name && transform.func).map(transform => ({
            name: transform.name,
            category: transform.category || 'special',
            func: transform.func.bind(transform),
            reverse: transform.reverse ? transform.reverse.bind(transform) : null,
            preview: transform.preview ? transform.preview.bind(transform) : () => '[preview]',
            configurableOptions: transform.configurableOptions || [],
            hasConfigurableOptions: Array.isArray(transform.configurableOptions) && transform.configurableOptions.length > 0,
            inputKind: transform.inputKind === 'text' ? 'text' : 'textarea',
        }));
    }

    async function ensure() {
        if (current().length && globalThis.TransformWorkbenchConfig) return current();
        if (!loading) {
            if (!globalThis.AssetLoader) throw new Error('文本变换资源加载器不可用。');
            loading = globalThis.AssetLoader.loadScriptsSequentially(['js/utils/emoji.js', 'js/core/transformOptions.js', 'js/tools/transforms/config.js', 'js/bundles/transforms-bundle.js']).then(() => {
                const transforms = current();
                if (!transforms.length) throw new Error('本地文本变换资源不完整，请检查安装。');
                return transforms;
            }).finally(() => { loading = null; });
        }
        return loading;
    }

    /** Applies the existing workbench filters to a supplied context's transform state. */
    function matchesFilters(transform, state = {}) {
        if (!transform) return false;
        const query = String(state.transformSearchQuery || '').trim().toLowerCase();
        const category = String(state.transformCategoryFilter || 'all');
        const tag = String(state.transformTagFilter || 'all');
        if (category !== 'all' && transform.category !== category) return false;
        const config = globalThis.TransformWorkbenchConfig || {};
        const keywords = config.transformKeywords?.[transform.name] || [];
        const label = config.categoryLabels?.[transform.category] || transform.category;
        if (query && ![transform.name, transform.category, label, keywords.join(' ')].join(' ').toLowerCase().includes(query)) return false;
        if (tag === 'reversible') return typeof transform.reverse === 'function';
        if (tag === 'configurable') return !!transform.hasConfigurableOptions;
        if (tag === 'encoding') return transform.category === 'encoding';
        if (tag === 'cleanup') return transform.category === 'format' || keywords.includes('清洗');
        if (tag === 'visual') return ['visual', 'unicode', 'fantasy'].includes(transform.category);
        if (tag === 'security') return keywords.includes('攻防语法') || transform.name.includes('Steganography');
        return true;
    }

    /** Preserves transform and translation display entries without initializing their tools. */
    function displayItems(items, transforms, state = {}) {
        const result = [];
        for (const [index, item] of (Array.isArray(items) ? items : []).entries()) {
            if (typeof item === 'string') {
                const transform = transforms.find(transform => transform.name === item);
                if (transform && matchesFilters(transform, state)) result.push({ type: 'transform', key: `tr-${item}-${index}`, transform });
            } else if (item && item.kind === 'translate') {
                const query = String(state.transformSearchQuery || '').trim().toLowerCase();
                if (!query || String(item.lang || '').toLowerCase().includes(query)) result.push({ type: 'translate', key: `tx-${item.lang}-${index}`, langName: item.lang, custom: !!item.custom });
            } else if (item && item.name) {
                const transform = transforms.find(transform => transform.name === item.name);
                if (transform && matchesFilters(transform, state)) result.push({ type: 'transform', key: `tr-${item.name}-${index}`, transform });
            }
        }
        return result;
    }

    function byCategory(transforms, category, state = {}) {
        return transforms.filter(transform => transform.category === category && matchesFilters(transform, state));
    }

    return { current, ensure, displayItems, byCategory };
})();
