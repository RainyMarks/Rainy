/** Loads pinned tokenizer encodings from the application resources in pages and workers. */
globalThis.TokenizerRuntime = (() => {
    const pending = new Map();
    const encodings = { cl100k: 'cl100k_base', o200k: 'o200k_base', p50k: 'p50k_base', r50k: 'r50k_base' };
    const scriptUrl = typeof document !== 'undefined' && document.currentScript ? document.currentScript.src : new URL('../utils/tokenizer.js', globalThis.location.href).href;
    return {
        async load(engine) {
            const encoding = encodings[engine];
            if (!encoding) throw new Error(`未知的 tokenizer：${engine}`);
            if (!pending.has(encoding)) {
                const url = new URL(`../vendor/tokenizer/encoding/${encoding}.js`, scriptUrl);
                pending.set(encoding, import(url.href).catch(error => {
                    pending.delete(encoding);
                    throw new Error(`本地 tokenizer ${engine} 加载失败，请检查安装资源。${error.message}`);
                }));
            }
            return pending.get(encoding);
        },
    };
})();
