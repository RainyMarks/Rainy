/** Real Vue/runtime save and scope handoff behavior with isolated Host/Worker transports. */
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import test from 'node:test';

const requireRoot = createRequire(new URL('../../../package.json', import.meta.url));
const { JSDOM, VirtualConsole } = requireRoot('jsdom');
const resources = new URL('../resources/icesky/', import.meta.url);
const clone = value => JSON.parse(JSON.stringify(value));
const deferred = () => Promise.withResolvers();
const draft = input => ({ legacyMigrated: true, shared: { activeTab: 'tokenizer' }, tools: { tokenizer: { fields: { tokenizerInput: input, tokenizerEngine: 'byte' }, files: {} } } });

async function harness() {
    const dom = new JSDOM('<div id="app"></div>', { url: 'https://icesky-fixture.invalid/rainy/icesky/index.html', runScripts: 'outside-only', virtualConsole: new VirtualConsole() });
    const window = dom.window;
    const pendingConfigurations = new Map(), requests = [], workers = [];
    const records = new Map([['session:A', { version: 1, revision: 0, data: draft('saved A') }], ['session:B', { version: 1, revision: 0, data: draft('saved B') }]]);
    let revision = 0, root, gate = null;
    const renderErrors = [];
    const parent = { postMessage(message) {
        if (message.type === 'rainy:loaded') pendingConfigurations.get(message.revision)?.resolve();
        if (message.type === 'rainy:error') for (const pending of pendingConfigurations.values()) pending.reject(new Error(`${message.message}; ${renderErrors.join('; ')}; ${JSON.stringify({epoch: root?.contextEpoch, children: root?.$children.map(child => ({id: child._iceSkyId, epoch: child._iceSkyEpoch, input: child.tokenizerInput?.slice(0, 40), disposed: child._iceSkyDisposed, destroyed: child._isDestroyed, type: child.$el?.nodeType})), hasView: Boolean(root?.getToolView('tokenizer'))})}`));
    } };
    Object.defineProperty(window, 'parent', { value: parent, configurable: true });
    window.CONFIG = { DRAFT_SAVE_DELAY_MS: 60000 };
    window.TextEncoder = TextEncoder; window.TextDecoder = TextDecoder;
    window.NotificationUtils = { showNotification() {} };
    window.Worker = class {
        constructor(url) { this.url = url; this.messages = []; this.terminated = false; workers.push(this); }
        postMessage(message) { this.messages.push(message); }
        terminate() { this.terminated = true; }
    };
    window.fetch = async (url, options = {}) => {
        const scope = new URL(url, window.location.href).searchParams.get('scope');
        const request = { scope, method: options.method || 'GET', body: options.body ? JSON.parse(options.body) : null };
        requests.push(request);
        if (gate) await gate(request);
        if (request.method === 'PUT') {
            const record = records.get(scope);
            assert.equal(request.body.baseRevision, record.revision);
            records.set(scope, { version: 1, revision: record.revision + 1, data: clone(request.body.data) });
        }
        const value = clone(records.get(scope) || { version: 1, revision: 0, data: draft('') });
        return { ok: true, status: 200, async json() { return value; } };
    };
    const loaded = new Map();
    function load(path) {
        if (!loaded.has(path)) loaded.set(path, Promise.resolve().then(() => window.eval(readFileSync(new URL(path, resources), 'utf8'))));
        return loaded.get(path);
    }
    window.AssetLoader = { loadScriptOnce: load, async loadScriptsSequentially(paths) { for (const path of paths) await load(path); }, async loadStyleOnce() {}, async loadTextOnce(path) { return readFileSync(new URL(path, resources), 'utf8'); } };
    for (const path of ['js/vendor/vue.min.js', 'js/tools/Tool.js', 'js/config/toolCatalog.js', 'js/core/toolRegistry.js', 'js/app/persistence.js', 'js/app/runtime.js']) await load(path);
    window.Vue.config.errorHandler = (error, _view, info) => renderErrors.push(`${info}: ${error.stack}`);
    const shell = new window.DOMParser().parseFromString(readFileSync(new URL('index.html', resources), 'utf8'), 'text/html').getElementById('tool-content-container').outerHTML;
    window.IceSkyRuntime.start(() => {
        root = new window.Vue(window.IceSkyRuntime.prepare({
            el: '#app',
            template: `<div id="app">${shell}</div>`,
            data: { activeTab: 'tokenizer', defaultTool: 'tokenizer', autoCopyEnabled: false, maxHistoryItems: 100, copyHistory: [], pinnedToolIds: [], recentToolIds: [], favoriteModels: [], mobileNavOpen: false },
            computed: {},
            methods: { rememberRecentTool() {}, handleGlobalKeydown() {}, downloadTextFile() {} },
        }));
        return root;
    });
    async function configure(id) {
        const requestRevision = ++revision, pending = deferred(); pendingConfigurations.set(requestRevision, pending);
        window.dispatchEvent(new window.MessageEvent('message', { source: parent, origin: window.location.origin, data: { type: 'rainy:configure', revision: requestRevision, context: { kind: 'session', id }, appearance: { locale: 'en' }, visible: true } }));
        try { await pending.promise; await window.Vue.nextTick(); }
        finally { pendingConfigurations.delete(requestRevision); }
    }
    const close = () => { window.dispatchEvent(new window.Event('pagehide')); window.close(); };
    try { await configure('A'); await window.IceSkyRuntime.flush(); }
    catch (error) { close(); throw error; }
    return { window, root, records, requests, workers, configure, close, setGate(value) { gate = value; }, get view() { return root.getToolView('tokenizer'); } };
}

test('final flush saves an edit that arrives during an earlier Host write', async () => {
    const h = await harness();
    try {
        const entered = deferred(), release = deferred(); let writes = 0;
        h.setGate(async request => { if (request.method === 'PUT' && ++writes === 1) { entered.resolve(); await release.promise; } });
        h.view.tokenizerInput = 'first edit'; await h.window.Vue.nextTick();
        const saving = h.window.IceSkyRuntime.flush(); await entered.promise;
        h.view.tokenizerInput = 'final edit'; await h.window.Vue.nextTick();
        release.resolve(); await saving;
        assert.equal(h.records.get('session:A').data.tools.tokenizer.fields.tokenizerInput, 'final edit');
        assert.equal(writes, 2);
    } finally { h.close(); }
});

test('scope handoff terminates pending computation before saving and ignores its late response', async () => {
    const h = await harness();
    try {
        h.view.tokenizerInput = 'pending A'; await h.window.Vue.nextTick();
        h.view.cancelTokenizer();
        const operation = h.view.runTokenizer();
        const worker = h.workers.at(-1), staleHandler = worker.onmessage, message = worker.messages.at(-1);
        const stoppedBeforeSave = [];
        h.setGate(request => { if (request.method === 'PUT' && request.scope === 'session:A') stoppedBeforeSave.push(worker.terminated); });
        await h.configure('B'); await operation;
        staleHandler({ data: { id: message.id, result: { tokens: [], groups: [], totalCount: 99, page: 0, pageCount: 1, charCount: 99, wordCount: 1, specialCount: 0, specialBreakdown: [] } } });
        await h.window.Vue.nextTick();
        assert.ok(stoppedBeforeSave.length); assert.ok(stoppedBeforeSave.every(Boolean));
        assert.equal(h.records.get('session:A').data.tools.tokenizer.fields.tokenizerInput, 'pending A');
        assert.equal(h.view.tokenizerInput, 'saved B'); assert.equal(h.view.tokenizerTotalCount, 0);
    } finally { h.close(); }
});

test('restoring a large saved input mounts without computation and large edits settle to manual mode', async () => {
    const h = await harness();
    try {
        assert.equal(h.workers.length, 0);
        h.view.tokenizerInput = 'Ordinary Unicode 示例🙂\n'.repeat(6000);
        await h.window.Vue.nextTick(); await Promise.resolve();
        assert.equal(h.workers.length, 0); assert.equal(h.view.tokenizerManualRequired, true); assert.equal(h.view.tokenizerComputing, false);
        assert.equal(h.window.document.getElementById('tokenizer-input').value, h.view.tokenizerInput);
        await h.window.IceSkyRuntime.flush(); await h.configure('B'); await h.configure('A');
        assert.equal(h.workers.length, 0); assert.equal(h.view.tokenizerInput, 'Ordinary Unicode 示例🙂\n'.repeat(6000));
    } finally { h.close(); }
});
