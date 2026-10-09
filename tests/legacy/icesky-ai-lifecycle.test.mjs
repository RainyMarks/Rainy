/** Late AI work remains owned by the tool context that requested it. */
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import vm from 'node:vm';
import test from 'node:test';

const resources = new URL('../resources/icesky/', import.meta.url);
const cases = [
    { tool: 'TranslateTool', method: 'translateTo', argument: 'French', controller: 'translateAbortController', output: 'transformOutput', loading: 'translateLoading', stop: 'translateStopRequest' },
    { tool: 'PromptCraftTool', method: 'pcRunMutation', controller: 'pcAbortController', output: 'pcOutput', loading: 'pcLoading', stop: 'pcStopMutation' },
    { tool: 'PromptCraftTool', method: 'pcRetryOutput', argument: 0, controller: 'pcAbortController', output: 'pcOutput', loading: 'pcLoading', stop: 'pcStopMutation' },
    { tool: 'DialogTemplateTool', method: 'dttRewritePrompt', controller: 'dttRewriteAbortController', output: 'dttRewriteOutput', loading: 'dttRewriteLoading' },
];
function deferred() { let resolve; const promise = new Promise(done => { resolve = done; }); return { promise, resolve }; }

function harness(spec) {
    const notifications = [];
    const scope = vm.createContext({ console, TextEncoder, TextDecoder, AbortController, setTimeout, clearTimeout });
    scope.window = scope;
    scope.IceSkyStorage = { getItem: () => null, setItem() {} };
    scope.localStorage = scope.IceSkyStorage;
    scope.IceSkyRuntime = { epoch: 1 };
    scope.NotificationUtils = { showNotification: (...args) => notifications.push(args) };
    for (const path of ['js/tools/Tool.js', `js/tools/${spec.tool}.js`]) {
        const url = new URL(path, resources); vm.runInContext(readFileSync(url, 'utf8'), scope, { filename: fileURLToPath(url) });
    }
    const tool = new scope[spec.tool]();
    const component = Object.assign(tool.getVueData(), tool.getVueMethods(), {
        _iceSkyEpoch: 1, _iceSkyDisposed: false, _iceSkyVisible: true,
        availableModelsLoading: false, availableModels: [{ id: 'fixture-model', name: 'Fixture model' }],
        transformInput: 'Hello', transformOutput: '', translateModel: 'fixture-model',
        pcInput: 'Hello', pcModel: 'fixture-model', pcCount: 1, pcStreamingEnabled: true,
        dttRewriteInput: 'Hello', dttRewriteModel: 'fixture-model',
        $set(object, key, value) { object[key] = value; },
    });
    return { scope, component, notifications, run: () => component[spec.method](spec.argument) };
}

test('each AI operation owns an abort controller before client loading and stops at a changed epoch', async () => {
    for (const spec of cases) {
        const h = harness(spec), loading = deferred(); let requests = 0;
        h.component.ensureOpenAIClientLoaded = () => loading.promise;
        h.scope.OpenAIClient = { chatCompletion() { requests++; }, streamChatCompletion() { requests++; } };
        const operation = h.run();
        assert.ok(h.component[spec.controller] instanceof AbortController, spec.method);
        h.scope.IceSkyRuntime.epoch = 2;
        loading.resolve(); await operation;
        assert.equal(requests, 0, spec.method); assert.equal(h.notifications.length, 0, spec.method);
        assert.equal(h.component[spec.loading], false, spec.method);
    }
});

test('disposed components reject late stream callbacks and responses even if the provider ignores cancellation', async () => {
    for (const spec of cases) {
        const h = harness(spec), response = deferred(); let callbacks;
        h.scope.OpenAIClient = {
            streamChatCompletion(_payload, options) { callbacks = options; return response.promise; },
            chatCompletion(_payload, options) { callbacks = options; return response.promise; },
            extractMessage: value => value,
        };
        const operation = h.run(), before = h.component[spec.output];
        assert.ok(callbacks?.signal, spec.method);
        h.component._iceSkyDisposed = true;
        callbacks.onOpen?.(); callbacks.onDelta?.('late', 'late response');
        response.resolve('late response'); await operation;
        assert.equal(h.component[spec.output], before, spec.method); assert.equal(h.notifications.length, 0, spec.method);
    }
});

test('explicit cancellation suppresses provider responses that arrive after abort', async () => {
    for (const spec of cases) {
        const h = harness(spec), response = deferred(); let callbacks;
        h.scope.OpenAIClient = {
            streamChatCompletion(_payload, options) { callbacks = options; return response.promise; },
            chatCompletion(_payload, options) { callbacks = options; return response.promise; },
            extractMessage: value => value,
        };
        const operation = h.run(), before = h.component[spec.output];
        if (spec.stop) h.component[spec.stop](); else h.component[spec.controller].abort();
        assert.equal(callbacks.signal.aborted, true, spec.method);
        callbacks.onOpen?.(); callbacks.onDelta?.('late', 'late response'); response.resolve('late response'); await operation;
        assert.equal(h.component[spec.output], before, spec.method); assert.equal(h.component[spec.loading], false, spec.method);
    }
});

test('owned user requests still accept normal completion without relying on tool visibility', async () => {
    for (const spec of cases) {
        const h = harness(spec);
        h.component._iceSkyVisible = false;
        h.scope.OpenAIClient = {
            async streamChatCompletion(_payload, options) { options.onOpen?.(); options.onDelta?.('normal', 'normal result'); return 'normal result'; },
            async chatCompletion() { return 'normal result'; },
            extractMessage: value => value,
        };
        await h.run();
        assert.equal(h.component[spec.output], 'normal result', spec.method); assert.equal(h.component[spec.loading], false, spec.method);
    }
});
