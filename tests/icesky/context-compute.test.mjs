/** Context-owned preferences and cancellation of delayed decoder translations. */
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import vm from 'node:vm';
import test from 'node:test';

const resources = new URL('../../resources/icesky/', import.meta.url);
const plain = value => JSON.parse(JSON.stringify(value));
function deferred() { let resolve; const promise = new Promise(done => { resolve = done; }); return { promise, resolve }; }

function harness(name, scripts = []) {
    const scope = vm.createContext({ console, TextEncoder, TextDecoder, AbortController, btoa, atob, setTimeout, clearTimeout });
    scope.window = scope;
    scope.IceSkyStorage = { getItem: () => null, setItem() {} };
    scope.localStorage = { getItem() { throw new Error('legacy origin preferences must not be read'); } };
    const load = path => { const url = new URL(path, resources); vm.runInContext(readFileSync(url, 'utf8'), scope, { filename: fileURLToPath(url) }); };
    for (const path of ['js/tools/Tool.js', 'js/utils/computeCoordinator.js', ...scripts, `js/tools/${name}.js`]) load(path);
    const tool = new scope[name]();
    const component = Object.assign(tool.getVueData(), tool.getVueMethods());
    component.getToolState = () => ({});
    component.$nextTick = async callback => callback?.();
    component.$set = (object, key, value) => { object[key] = value; };
    component.showNotification = () => {};
    const copied = [];
    component.copyToClipboard = async text => { copied.push(text); return true; };
    return { scope, component, copied };
}

test('restored transform preferences and selected metadata work when the runtime is already loaded', async () => {
    const { component } = harness('TransformTool', ['js/core/transformOptions.js', 'js/bundles/transforms-bundle.js']);
    component.transformOptionPrefs = { 'Alternating Case': { startWith: 'lower' } };
    component.activeTransform = { name: 'Alternating Case', category: 'case' };
    await component.ensureTransformAssetsLoaded();
    assert.equal(typeof component.activeTransform.func, 'function');
    assert.deepEqual(plain(component.getMergedOptionsForTransform('Alternating Case')), { startWith: 'lower' });
    assert.equal(component.activeTransform.func('hello', component.getMergedOptionsForTransform('Alternating Case')), 'hElLo');
    component.transformOptionPrefs['Alternating Case'].startWith = 'upper'; component.invalidateTransformOptionsCache();
    assert.equal(component.activeTransform.func('hello', component.getMergedOptionsForTransform('Alternating Case')), 'HeLlO');
});

test('decoder requests receive preferences only from the same context transform draft', async () => {
    const { component } = harness('DecodeTool');
    const preferences = { 'Caesar Cipher': { shift: 1 } };
    component.getToolState = id => { assert.equal(id, 'transforms'); return { transformOptionPrefs: preferences }; };
    let request;
    component._decoderCoordinator = { async run(value) { request = value; return { decoded: null, language: null }; } };
    component.decoderInput = 'Ifmmp'; component.selectedDecoder = 'Caesar Cipher';
    await component.runUniversalDecode();
    assert.deepEqual(plain(request.optionsByName), preferences);
});

test('cancelling during client loading prevents a translation request from starting', async () => {
    const { scope, component, copied } = harness('DecodeTool');
    scope.OPENAI_MODELS = [{ id: 'configured-test-model' }];
    const loading = deferred();
    component.decoderInput = '普通文本';
    component.ensureOpenAIClientLoaded = () => loading.promise;
    let requests = 0;
    scope.OpenAIClient = { chatCompletion() { requests++; }, extractMessage: value => value };
    const operation = component.decoderTranslateToEnglish();
    const controller = component.decoderTranslationController;
    assert.ok(controller);
    component.cancelDecoder(); loading.resolve(); await operation;
    assert.equal(controller.signal.aborted, true); assert.equal(requests, 0); assert.equal(copied.length, 0);
    assert.equal(component.decoderTranslating, false); assert.equal(component.decoderTranslationController, null);
});

test('late translation responses cannot update or copy after a context disposes the decoder', async () => {
    const { scope, component, copied } = harness('DecodeTool');
    scope.OPENAI_MODELS = [{ id: 'configured-test-model' }];
    const response = deferred(); let signal;
    component.decoderInput = '普通文本'; component.decoderOutput = 'existing result';
    scope.OpenAIClient = { chatCompletion(_payload, options) { signal = options.signal; return response.promise; }, extractMessage: value => value };
    const operation = component.decoderTranslateToEnglish();
    assert.ok(signal);
    component.cancelDecoder(); component._iceSkyDisposed = true;
    response.resolve('late translation'); await operation;
    assert.equal(signal.aborted, true); assert.equal(component.decoderOutput, 'existing result'); assert.deepEqual(copied, []);
});

test('an older translation completion cannot clear a newer request or copy its result', async () => {
    const { scope, component, copied } = harness('DecodeTool');
    scope.OPENAI_MODELS = [{ id: 'configured-test-model' }];
    const first = deferred(), second = deferred(); let count = 0;
    component.decoderInput = '普通文本';
    scope.OpenAIClient = { chatCompletion() { return ++count === 1 ? first.promise : second.promise; }, extractMessage: value => value };
    const oldOperation = component.decoderTranslateToEnglish();
    component.cancelDecoder();
    const newOperation = component.decoderTranslateToEnglish(), newController = component.decoderTranslationController;
    first.resolve('old'); await oldOperation;
    assert.equal(component.decoderTranslating, true); assert.equal(component.decoderTranslationController, newController); assert.deepEqual(copied, []);
    second.resolve('current'); await newOperation;
    assert.equal(component.decoderOutput, 'current'); assert.deepEqual(copied, ['current']); assert.equal(component.decoderTranslating, false);
});
