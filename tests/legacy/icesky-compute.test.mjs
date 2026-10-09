/** Worker protocol and output regressions for the human-operated IceSky tools. */
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import vm from 'node:vm';
import test from 'node:test';

const resourceRoot = fileURLToPath(new URL('../resources/icesky/', import.meta.url));
const workerPath = resolve(resourceRoot, 'js/workers/compute.js');
const fixture = name => JSON.parse(readFileSync(new URL(`fixtures/icesky/${name}-before.json`, import.meta.url), 'utf8'));
const plain = value => JSON.parse(JSON.stringify(value));

function scriptContext() {
    const values = new Map();
    const scope = vm.createContext({ TextEncoder, TextDecoder, URL, Uint8Array, Uint32Array, console, btoa, atob, setTimeout, clearTimeout, location: { href: pathToFileURL(workerPath).href }, localStorage: { getItem: key => values.get(key) || null, setItem: (key, value) => values.set(key, value) } });
    scope.window = scope; scope.self = scope;
    const load = path => vm.runInContext(readFileSync(path, 'utf8'), scope, { filename: path, importModuleDynamically: vm.constants.USE_MAIN_CONTEXT_DEFAULT_LOADER });
    scope.importScripts = (...paths) => { for (const path of paths) load(resolve(dirname(workerPath), path)); };
    return { scope, load };
}

function workerHarness() {
    const { scope, load } = scriptContext();
    const responses = [];
    scope.postMessage = result => responses.push(plain(result));
    load(workerPath);
    let id = 0;
    return async request => {
        const requestId = ++id;
        await scope.onmessage({ data: { id: requestId, ...request } });
        const response = responses.find(item => item.id === requestId);
        assert.ok(response, 'worker must answer each valid request');
        if (response.error) throw new Error(response.error);
        return response.result;
    };
}

function coordinatorHarness() {
    const { scope, load } = scriptContext();
    load(resolve(resourceRoot, 'js/utils/computeCoordinator.js'));
    const workers = [], timers = new Map();
    let timerId = 0;
    const coordinator = new scope.IceSkyComputeCoordinator({
        createWorker() {
            const worker = { messages: [], terminated: false, postMessage(message) { this.messages.push(message); }, terminate() { this.terminated = true; } };
            workers.push(worker); return worker;
        },
        setTimer(callback, delay) { const id = ++timerId; timers.set(id, { callback, delay }); return id; },
        clearTimer(id) { timers.delete(id); },
    });
    return { coordinator, workers, timers, permitsAutomatic: scope.IceSkyPermitsAutomatic, flush() { const pending = [...timers.values()]; timers.clear(); for (const timer of pending) timer.callback(); } };
}

test('automatic computation uses the UTF-8 byte threshold including surrogate replacement', () => {
    const { coordinator, permitsAutomatic } = coordinatorHarness();
    try {
        assert.equal(permitsAutomatic('a'.repeat(65536)), true);
        assert.equal(permitsAutomatic('a'.repeat(65537)), false);
        assert.equal(permitsAutomatic('😀'.repeat(16384)), true);
        assert.equal(permitsAutomatic('😀'.repeat(16384) + 'a'), false);
        assert.equal(permitsAutomatic('\ud800'.repeat(21845) + 'a'), true);
        assert.equal(permitsAutomatic('\ud800'.repeat(21846)), false);
    } finally { coordinator.dispose(); }
});

test('duplicate input events share one 160 ms debounce and one worker request', async () => {
    const harness = coordinatorHarness();
    try {
        const payload = { kind: 'transform', input: 'abc', steps: [] };
        const first = harness.coordinator.run(payload, { automatic: true });
        const second = harness.coordinator.run(payload, { automatic: true });
        assert.equal(first, second); assert.equal(harness.timers.size, 1);
        assert.equal([...harness.timers.values()][0].delay, 160); assert.equal(harness.workers.length, 0);
        harness.flush();
        const worker = harness.workers[0]; assert.equal(worker.messages.length, 1);
        worker.onmessage({ data: { id: worker.messages[0].id, result: { text: 'abc' } } });
        assert.deepEqual(plain(await first), { text: 'abc' });
        assert.deepEqual(plain(await harness.coordinator.run(payload)), { text: 'abc' });
        assert.equal(worker.messages.length, 1, 'same input reuses the completed result');
    } finally { harness.coordinator.dispose(); }
});

test('default timers keep their browser global receiver for scheduling and cancellation', async () => {
    const { scope, load } = scriptContext();
    vm.runInContext(`
        globalThis.timerCallbacks = new Map();
        globalThis.nextTimer = 0;
        globalThis.setTimeout = function (callback, delay) {
            if (this !== globalThis) throw new TypeError('Illegal timer receiver');
            const timer = ++globalThis.nextTimer;
            globalThis.timerCallbacks.set(timer, { callback, delay });
            return timer;
        };
        globalThis.clearTimeout = function (timer) {
            if (this !== globalThis) throw new TypeError('Illegal timer receiver');
            globalThis.timerCallbacks.delete(timer);
        };
    `, scope);
    load(resolve(resourceRoot, 'js/utils/computeCoordinator.js'));
    let worker;
    const coordinator = new scope.IceSkyComputeCoordinator({ createWorker() { worker = { messages: [], postMessage(message) { this.messages.push(message); }, terminate() {} }; return worker; } });
    try {
        const cancelled = coordinator.run({ kind: 'decoder', input: 'first' }, { automatic: true });
        coordinator.cancel();
        assert.equal(await cancelled, null); assert.equal(scope.timerCallbacks.size, 0);
        const operation = coordinator.run({ kind: 'decoder', input: 'SGVsbG8=' }, { automatic: true });
        const timer = [...scope.timerCallbacks.values()][0];
        assert.equal(timer.delay, 160); timer.callback();
        worker.onmessage({ data: { id: worker.messages[0].id, result: { text: 'Hello' } } });
        assert.deepEqual(plain(await operation), { text: 'Hello' });
    } finally { coordinator.dispose(); }
});

test('a failed debounce setup rejects and leaves the same input available for manual retry', async () => {
    const { scope, load } = scriptContext();
    load(resolve(resourceRoot, 'js/utils/computeCoordinator.js'));
    let worker;
    const coordinator = new scope.IceSkyComputeCoordinator({
        setTimer() { throw new TypeError('timer unavailable'); },
        createWorker() { worker = { messages: [], postMessage(message) { this.messages.push(message); }, terminate() {} }; return worker; },
    });
    try {
        const payload = { kind: 'decoder', input: 'SGVsbG8=' };
        await assert.rejects(coordinator.run(payload, { automatic: true }), /timer unavailable/);
        const operation = coordinator.run(payload);
        worker.onmessage({ data: { id: worker.messages[0].id, result: { text: 'Hello' } } });
        assert.deepEqual(plain(await operation), { text: 'Hello' });
    } finally { coordinator.dispose(); }
});

test('changed input cancels executing code by terminating its worker and ignores late output', async () => {
    const { coordinator, workers } = coordinatorHarness();
    try {
        const first = coordinator.run({ kind: 'tokenizer', input: 'first', engine: 'word' });
        const oldWorker = workers[0], oldHandler = oldWorker.onmessage, oldId = oldWorker.messages[0].id;
        const second = coordinator.run({ kind: 'tokenizer', input: 'second', engine: 'word' });
        assert.equal(await first, null); assert.equal(oldWorker.terminated, true); assert.equal(workers.length, 2);
        oldHandler({ data: { id: oldId, result: { text: 'stale' } } });
        const worker = workers[1]; worker.onmessage({ data: { id: worker.messages[0].id, result: { text: 'fresh' } } });
        assert.deepEqual(plain(await second), { text: 'fresh' });
    } finally { coordinator.dispose(); }
});

test('large automatic input cancels the previous run and waits for a manual action', async () => {
    const { coordinator, workers } = coordinatorHarness();
    try {
        const pending = coordinator.run({ kind: 'tokenizer', input: 'old', engine: 'word' });
        const payload = { kind: 'tokenizer', input: 'x'.repeat(65537), engine: 'word' };
        assert.equal(await coordinator.run(payload, { automatic: true }), null);
        assert.equal(await pending, null); assert.equal(workers[0].terminated, true); assert.equal(workers.length, 1);
        const manual = coordinator.run(payload);
        assert.equal(workers.length, 2);
        coordinator.dispose(); assert.equal(await manual, null); assert.equal(workers[1].terminated, true);
        assert.equal(await coordinator.run(payload), null);
    } finally { coordinator.dispose(); }
});

test('worker load failures reject the current operation and permit an explicit retry', async () => {
    const { coordinator, workers } = coordinatorHarness();
    try {
        const operation = coordinator.run({ kind: 'decoder', input: 'hello' });
        const rejected = assert.rejects(operation, /missing worker/);
        workers[0].onerror({ message: 'missing worker' }); await rejected;
        assert.equal(workers[0].terminated, true);
        const retry = coordinator.run({ kind: 'decoder', input: 'hello' });
        assert.equal(workers.length, 2); coordinator.cancel(); assert.equal(await retry, null);
    } finally { coordinator.dispose(); }
});

test('byte and word results retain pre-worker Unicode, display, and count semantics', async () => {
    const run = workerHarness();
    for (const expected of fixture('tokenizer')) {
        const actual = await run({ kind: 'tokenizer', input: expected.input, engine: expected.engine });
        const withoutIndex = item => { const { index, ...rest } = item; return rest; };
        const tokens = expected.engine === 'byte' ? actual.groups.flatMap(group => group.tokens).map(withoutIndex) : actual.tokens.map(withoutIndex);
        const groups = actual.groups.map(group => { const { index, ...rest } = group; return { ...rest, tokens: rest.tokens.map(withoutIndex) }; });
        assert.deepEqual(tokens, expected.tokens); assert.deepEqual(groups, expected.groups);
        for (const key of ['charCount', 'wordCount', 'specialCount', 'specialBreakdown']) assert.deepEqual(actual[key], expected[key]);
        assert.equal(actual.totalCount, expected.tokens.length);
    }
});

test('token pages preserve global indexes and are bounded to 100 Unicode code point groups', async () => {
    const run = workerHarness();
    const first = await run({ kind: 'tokenizer', engine: 'byte', input: '😀中a'.repeat(101) });
    assert.equal(first.totalCount, 808); assert.equal(first.charCount, 303); assert.equal(first.groups.length, 100); assert.equal(first.pageCount, 4);
    const last = await run({ kind: 'tokenizer-page', page: 3 });
    assert.equal(last.groups.length, 3); assert.equal(last.groups[0].index, 300); assert.equal(last.groups[0].start, 800);
    assert.deepEqual(last.groups.flatMap(group => group.tokens.map(token => token.index)), [800, 801, 802, 803, 804, 805, 806, 807]);
    const word = await run({ kind: 'tokenizer', engine: 'word', input: 'word '.repeat(101) });
    assert.equal(word.totalCount, 202); assert.equal(word.tokens.length, 100);
    const lastWord = await run({ kind: 'tokenizer-page', page: 2 });
    assert.deepEqual(lastWord.tokens.map(token => token.index), [200, 201]);
});

test('one MiB byte analysis keeps full counts while sending only the first page', async () => {
    const run = workerHarness();
    const result = await run({ kind: 'tokenizer', engine: 'byte', input: 'a'.repeat(1024 * 1024) });
    assert.equal(result.totalCount, 1024 * 1024); assert.equal(result.charCount, 1024 * 1024);
    assert.equal(result.groups.length, 100); assert.equal(result.tokens.length, 0);
    assert.ok(JSON.stringify(result).length < 40000);
    const last = await run({ kind: 'tokenizer-page', page: result.pageCount - 1 });
    assert.equal(last.groups.at(-1).tokens.at(-1).index, 1024 * 1024 - 1);
});

test('transforms and decoder candidates match the captured original algorithms', async () => {
    const run = workerHarness();
    for (const expected of fixture('transforms')) {
        const result = await run({ kind: 'transform', input: expected.input, steps: [{ name: expected.name, options: expected.options }] });
        assert.equal(result.text, expected.output);
    }
    for (const expected of fixture('decoder')) {
        const result = await run({ kind: 'decoder', input: expected.input, decoder: 'auto' });
        assert.deepEqual(result.decoded, expected.result);
    }
});

test('manual decoder options reach the original reverse operation', async () => {
    const run = workerHarness();
    const result = await run({ kind: 'decoder', input: 'Uryyb, jbeyq!', decoder: 'ROT13' });
    assert.deepEqual(result.decoded, { text: 'Hello, world!', method: 'ROT13', alternatives: [] });
    const configured = await run({ kind: 'decoder', input: 'Ifmmp', decoder: 'Caesar Cipher', optionsByName: { 'Caesar Cipher': { shift: 1 } } });
    assert.equal(configured.decoded.text, 'Hello');
});

test('all four pinned BPE encodings load locally and retain their distinct token IDs', async () => {
    const run = workerHarness();
    const expectations = { cl100k: [15339, 1917], o200k: [24912, 2375], p50k: [31373, 995], r50k: [31373, 995] };
    for (const [engine, ids] of Object.entries(expectations)) {
        const result = await run({ kind: 'tokenizer', engine, input: 'hello world' });
        assert.deepEqual(result.tokens.map(token => token.id), ids);
        assert.deepEqual(result.tokens.map(token => token.text), ['hello', ' world']);
        assert.equal(result.totalCount, 2); assert.equal(result.charCount, 11); assert.equal(result.specialCount, 1);
    }
});

test('invalid worker requests fail explicitly without silently choosing another tokenizer', async () => {
    const run = workerHarness();
    await assert.rejects(run({ kind: 'tokenizer', engine: 'missing', input: 'hello' }), /未知的 tokenizer/);
    await assert.rejects(run({ kind: 'tokenizer', engine: 'byte', input: 123 }), /必须是文本/);
    await assert.rejects(run({ kind: 'tokenizer-page', page: -1 }), /页码无效/);
});

test('splitter can load the shared transform runtime before the transform tool is visited', async () => {
    const { scope, load } = scriptContext();
    const scriptRequests = [];
    scope.AssetLoader = { async loadScriptsSequentially(paths) { scriptRequests.push(...paths); for (const path of paths) load(resolve(resourceRoot, path)); } };
    load(resolve(resourceRoot, 'js/tools/Tool.js'));
    load(resolve(resourceRoot, 'js/utils/transformRuntime.js'));
    load(resolve(resourceRoot, 'js/tools/SplitterTool.js'));
    const tool = new scope.SplitterTool();
    const component = Object.assign(tool.getVueData(), tool.getVueMethods());
    const transformState = { favorites: ['Base64'], lastUsedTransforms: [{ name: 'ROT13' }, { kind: 'translate', lang: 'French', custom: true }] };
    component.getToolState = id => { assert.equal(id, 'transforms'); return transformState; };
    assert.equal(component.transforms.length, 0);
    assert.deepEqual(plain(component.getFavoriteDisplayItems()), []);
    assert.deepEqual(plain(component.getTransformsByCategory('encoding')), []);
    assert.equal(await component.ensureSplitterTransformsLoaded(), true);
    assert.ok(component.transforms.some(transform => transform.name === 'Base64'));
    assert.deepEqual(plain(component.getFavoriteDisplayItems().map(item => item.name)), ['Base64']);
    assert.deepEqual(plain(component.getLastUsedDisplayItems().map(item => ({ type: item.type, name: item.name }))), [{ type: 'transform', name: 'ROT13' }, { type: 'translate', name: 'French' }]);
    assert.equal(component.getLastUsedDisplayItems()[1].custom, true);
    assert.ok(component.getTransformsByCategory('encoding').some(transform => transform.name === 'Base64'));
    transformState.transformSearchQuery = 'French';
    assert.deepEqual(plain(component.getFavoriteDisplayItems()), []);
    assert.deepEqual(plain(component.getLastUsedDisplayItems().map(item => item.langName)), ['French']);
    delete transformState.transformSearchQuery;
    assert.equal(scope.TransformTool, undefined, 'shared loading does not instantiate another tool');
    component.splitterTransforms = ['Base64'];
    assert.deepEqual(plain(await component.applySplitterTransforms(['Hello'])), ['SGVsbG8=']);
    await component.ensureSplitterTransformsLoaded();
    assert.equal(scriptRequests.filter(path => path.endsWith('transforms-bundle.js')).length, 1);
});

test('image comparison uses the shared fine-grained diff before the mutation tool is visited', () => {
    const { scope, load } = scriptContext();
    load(resolve(resourceRoot, 'js/tools/Tool.js'));
    load(resolve(resourceRoot, 'js/utils/diff.js'));
    load(resolve(resourceRoot, 'js/tools/ImageInjectTool.js'));
    const tool = new scope.ImageInjectTool();
    const component = Object.assign(tool.getVueData(), tool.getVueMethods());
    component.imageSourceText = 'alpha beta'; component.imageRenderedText = 'alpha gamma'; component.imageInputMode = 'text';
    component.imageUpdateRenderAnalysis();
    assert.equal(component.imageChangedCount, 6);
    assert.equal(component.imageDiffSourceHtml, 'alpha <mark class="fuzzer-diff-removed">bet</mark>a');
    assert.equal(component.imageDiffOutputHtml, 'alpha <mark class="fuzzer-diff-added">g</mark>a<mark class="fuzzer-diff-added">mma</mark>');
    assert.equal(scope.MutationTool, undefined);
});
