/** Text handoff regressions across independently mounted workbench tools. */
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import vm from 'node:vm';
import test from 'node:test';

const resources = new URL('../../resources/icesky/', import.meta.url);
const plain = value => JSON.parse(JSON.stringify(value));

function harness(toolName) {
    const scope = vm.createContext({ console, TextEncoder, TextDecoder });
    scope.window = scope;
    const loaded = [];
    const load = path => { loaded.push(path); const url = new URL(path, resources); vm.runInContext(readFileSync(url, 'utf8'), scope, { filename: fileURLToPath(url) }); };
    load('js/tools/Tool.js');
    load(`js/tools/${toolName}.js`);
    const tool = new scope[toolName]();
    const component = Object.assign(tool.getVueData(), tool.getVueMethods());
    const states = new Map(), views = new Map(), opened = [], confirmations = [];
    component.getToolState = id => states.get(id) || {};
    component.getToolView = id => views.get(id) || null;
    component.openTool = async (id, fields = {}) => { opened.push({ id, fields: plain(fields) }); return views.get(id) || {}; };
    scope.confirm = message => { confirmations.push(message); return true; };
    scope.AssetLoader = { async loadScriptOnce(path) { if (!loaded.includes(path)) load(path); } };
    return { component, scope, states, views, opened, confirmations, loaded, load };
}

test('sending text to transforms confirms against the target draft and only sets fields', async () => {
    const h = harness('InjectionGeneratorTool');
    h.states.set('transforms', { transformInput: 'existing text' });
    h.scope.confirm = () => false;
    await h.component.igSendToTransform('new text');
    assert.equal(h.opened.length, 0);
    h.scope.confirm = () => true;
    await h.component.igSendToTransform('new text');
    assert.deepEqual(h.opened, [{ id: 'transforms', fields: { transformInput: 'new text', transformOutput: '', transformChainOutput: '', transformChainError: '' } }]);
    assert.equal(h.component.transformInput, undefined, 'the source component does not receive target fields');
});

test('sample source reads use each tool draft and preserve the replacement confirmation', async () => {
    const h = harness('SampleBuilderTool');
    h.component.sbSource = 'transforms'; h.component.transformOutput = 'unrelated source component value';
    h.states.set('transforms', { transformOutput: 'target result' });
    await h.component.sbPull();
    assert.equal(h.component.sb.payload, 'target result');
    h.component.sbSource = 'fuzzer'; h.component.sbFuzzIndex = 1;
    h.states.set('fuzzer', { fuzzerOutputs: [{ text: 'first' }, { text: 'second' }] });
    h.scope.confirm = () => false;
    await h.component.sbPull(); assert.equal(h.component.sb.payload, 'target result');
    h.scope.confirm = () => true;
    await h.component.sbPull(); assert.equal(h.component.sb.payload, 'second');
    assert.equal(h.opened.length, 0);
});

test('unvisited empty dialog sources do not initialize a tool or invent a preview', async () => {
    const h = harness('SampleBuilderTool');
    h.component.sbSource = 'dialogtemplate';
    await h.component.sbPull();
    assert.equal(h.component.sbError, '来源工具没有可用文本');
    assert.equal(h.loaded.includes('js/tools/DialogTemplateTool.js'), false);
    assert.equal(h.opened.length, 0);
});

test('an existing dialog view supplies its current preview without loading another tool', async () => {
    const h = harness('SampleBuilderTool');
    h.component.sbSource = 'dialogtemplate';
    h.views.set('dialogtemplate', { dttPreviewContent() { return 'visible dialog preview'; } });
    await h.component.sbPull();
    assert.equal(h.component.sb.payload, 'visible dialog preview');
    assert.equal(h.loaded.includes('js/tools/DialogTemplateTool.js'), false);
});

test('saved unmounted dialog drafts reuse the original formatters without restoring tool state', async () => {
    const h = harness('SampleBuilderTool');
    h.component.sbSource = 'dialogtemplate';
    h.load('js/tools/DialogTemplateTool.js');
    h.scope.DialogTemplateTool.prototype.getVueData = () => { throw new Error('must not initialize saved source'); };
    const source = { dttTitle: 'Greeting', dttGoal: 'A simple exchange', dttVariablesText: 'name = Sam', dttPreviewMode: 'json', dttTurns: [{ id: 'greeting', role: 'user', title: 'First', content: 'Hello {{name}}', kind: 'normal', note: '' }] };
    h.states.set('dialogtemplate', source);
    const before = JSON.stringify(source);
    await h.component.sbPull();
    assert.equal(h.component.sbError, '');
    assert.deepEqual(JSON.parse(h.component.sb.payload).messages, [{ role: 'user', content: 'Hello Sam' }]);
    assert.equal(JSON.stringify(source), before);
    for (const mode of ['markdown', 'transcript']) {
        source.dttPreviewMode = mode;
        await h.component.sbPull();
        assert.equal(h.component.sbError, ''); assert.match(h.component.sb.payload, /Hello Sam/);
    }
    assert.equal(h.opened.length, 0);
});

test('sample handoffs set the selected target fields without calling a generator', async () => {
    const h = harness('SampleBuilderTool');
    h.component.sbDirty = () => false;
    h.component.sbResult = { text: 'plain note', terminal: false };
    h.component.sbTarget = 'transforms';
    h.states.set('transforms', { transformInput: 'existing text' });
    h.scope.confirm = () => false;
    await h.component.sbSend(); assert.equal(h.opened.length, 0);
    h.scope.confirm = () => true;
    await h.component.sbSend();
    assert.deepEqual(h.opened, [{ id: 'transforms', fields: { transformInput: 'plain note' } }]);
    h.component.sbResult = { text: 'a comment', terminal: true };
    await h.component.sbSend();
    assert.deepEqual(h.opened[1], { id: 'docxinject', fields: { docxInjectCommentText: 'a comment', docxInjectEnableComment: true } });
});

test('dialog handoffs append messages to the returned target view after it is ready', async () => {
    const h = harness('SampleBuilderTool');
    const turns = [];
    h.component.sbDirty = () => false; h.component.sbTarget = 'dialogtemplate'; h.component.sb.carrier = 'turns';
    h.component.sbResult = { type: 'json', text: JSON.stringify({ messages: [{ role: 'user', content: 'Hello' }, { role: 'assistant', content: 'Hi' }] }) };
    h.views.set('dialogtemplate', { dttAddTurn(role, index, seed) { turns.push({ role, index, content: seed.content }); } });
    await h.component.sbSend();
    assert.deepEqual(turns, [{ role: 'user', index: null, content: 'Hello' }, { role: 'assistant', index: null, content: 'Hi' }]);
    assert.deepEqual(h.opened, [{ id: 'dialogtemplate', fields: {} }]);
});

test('invalid saved message JSON is reported before navigating away', async () => {
    const h = harness('SampleBuilderTool');
    h.component.sbDirty = () => false; h.component.sbTarget = 'dialogtemplate'; h.component.sb.carrier = 'turns';
    h.component.sbResult = { type: 'json', text: '{' };
    await h.component.sbSend();
    assert.ok(h.component.sbError); assert.equal(h.opened.length, 0);
});
