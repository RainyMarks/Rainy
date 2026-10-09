/** Runs the existing local text algorithms outside the page's input/render loop. */
self.window = self;
let optionsByName = {};
let transformsLoaded = false;
let tokenizerLoaded = false;
let languageDetector = null;
let tokenizerResult = null;

function loadTransforms(request) {
    if (!transformsLoaded) {
        importScripts('../utils/emoji.js', '../core/transformOptions.js', '../core/steganography.js', '../bundles/transforms-bundle.js', '../core/decoder.js', '../tools/transforms/config.js', '../tools/transforms/recommendations.js');
        const resolveOptions = self.getMergedTransformOptions;
        self.getMergedTransformOptions = (transform, preferences = optionsByName) => resolveOptions(transform, preferences);
        transformsLoaded = true;
    }
    if (request.emojiDataReady && !self.emojiData) importScripts('../data/emojiData.js');
    optionsByName = request.optionsByName || {};
}

function findTransform(name) {
    const transform = Object.values(self.transforms).find(item => item.name === name);
    if (!transform) throw new Error('未找到所选文本变换，请重新选择。');
    return transform;
}

async function compute(request) {
    if (request.kind === 'tokenizer-page') {
        if (!Number.isSafeInteger(request.page) || request.page < 0) throw new Error('页码无效。');
        if (!tokenizerResult) throw new Error('结果已释放，请重新计算。');
        return tokenizerResult.page(request.page);
    }
    if (request.kind !== 'decoder-catalog' && typeof request.input !== 'string') throw new Error('计算输入必须是文本。');
    if (request.kind === 'tokenizer') {
        if (!tokenizerLoaded) {
            importScripts('../utils/tokenizer.js', './tokenizer.js');
            tokenizerLoaded = true;
        }
        tokenizerResult = null;
        tokenizerResult = await new self.IceSkyTokenizerResult(request.input, request.engine).initialize();
        return tokenizerResult.page(0);
    }
    loadTransforms(request);
    if (request.kind === 'decoder-catalog') return Object.values(self.transforms).filter(item => typeof item.reverse === 'function').map(item => ({ name: item.name }));
    if (request.kind === 'decoder') {
        let result = null;
        if (request.decoder === 'auto') result = await self.universalDecode(request.input, { activeTab: 'decoder' });
        else {
            const transform = findTransform(request.decoder);
            if (typeof transform.reverse === 'function') {
                const text = await transform.reverse(request.input, self.getMergedTransformOptions(transform));
                if (typeof text === 'string' && text !== request.input) result = { text, method: transform.name, alternatives: [] };
            }
        }
        if (!languageDetector) {
            importScripts('../tools/Tool.js', '../tools/DecodeTool.js');
            languageDetector = new self.DecodeTool();
        }
        const language = languageDetector.detectLanguage(request.input) || (result && result.text !== request.input ? languageDetector.detectLanguage(result.text) : null);
        return { decoded: result, language };
    }
    if (request.kind === 'transform') {
        if (!Array.isArray(request.steps) || request.steps.some(step => !step || typeof step.name !== 'string')) throw new Error('文本变换列表无效。');
        let text = request.input;
        for (const step of request.steps) {
            const transform = findTransform(step.name);
            const operation = step.reverse ? transform.reverse : transform.func;
            if (typeof operation !== 'function') throw new Error('所选变换不支持此操作。');
            text = await operation.call(transform, text, step.options || {});
        }
        const randomizer = self.transforms.randomizer;
        const active = request.steps.length ? findTransform(request.steps[request.steps.length - 1].name) : null;
        const recommendations = self.TransformRecommendationEngine.analyze(text, active, Object.values(self.transforms), self.TransformWorkbenchConfig);
        return { text, recommendations, randomMixReport: randomizer && typeof randomizer.getLastReport === 'function' ? randomizer.getLastReport() : null, randomMixSamples: randomizer && typeof randomizer.getLastTransformInfo === 'function' ? randomizer.getLastTransformInfo().slice(0, 6) : [] };
    }
    throw new Error('不支持的计算请求。');
}

self.onmessage = async event => {
    const request = event.data;
    if (!request || !Number.isSafeInteger(request.id) || typeof request.kind !== 'string') return;
    try { self.postMessage({ id: request.id, result: await compute(request) }); }
    catch (error) { self.postMessage({ id: request.id, error: error && error.message ? error.message : '计算失败，请重试。' }); }
};
