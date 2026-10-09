/** Durable text-state regressions with an isolated VM and a synthetic Host transport. */
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import vm from 'node:vm';
import test from 'node:test';

const source = new URL('../../resources/icesky/js/app/persistence.js', import.meta.url);
const plain = value => JSON.parse(JSON.stringify(value));
const envelope = (data = {}, revision = 0) => ({ version: 1, revision, data });
const draft = text => ({ shared: { activeTab: 'transforms' }, tools: { transforms: { fields: { transformInput: text }, files: {} } }, legacyMigrated: true });
const response = (value, status = 200) => ({ ok: status >= 200 && status < 300, status, async json() { return value; } });

function harness(handler = () => response(envelope())) {
    const requests = [], statuses = [];
    const scope = vm.createContext({ console, fetch: async (url, options = {}) => {
        const request = { url, method: options.method ?? 'GET', data: options.body === undefined ? undefined : JSON.parse(options.body) };
        requests.push(request);
        return handler(request);
    } });
    scope.window = scope;
    vm.runInContext(readFileSync(source, 'utf8'), scope, { filename: source.pathname });
    const store = new scope.IceSkyPersistence.DraftStore('standalone', (state, message) => statuses.push({ state, message }));
    return { store, api: scope.IceSkyPersistence, requests, statuses };
}

test('read failure cannot enable saving or replace a draft with empty data', async () => {
    const h = harness(() => response({ error: { message: 'disk unavailable' } }, 500));
    await assert.rejects(h.store.load(), /disk unavailable/);
    h.store.changed();
    await assert.rejects(h.store.save(() => draft('unsaved text')), /not been loaded/);
    assert.equal(h.store.ready, false);
    assert.equal(h.requests.filter(request => request.method === 'PUT').length, 0);
    assert.equal(h.store.saved, 0);
});

test('uploaded source text is omitted while editable text and file metadata survive', () => {
    const h = harness();
    const fields = { richTextInjectUploadedSource: 'private uploaded source', richTextInjectVisibleText: 'edited ordinary text' };
    const keys = h.api.persistentKeys(fields, '<textarea v-model="richTextInjectVisibleText"></textarea>');
    assert.deepEqual(plain(h.api.fields(fields, keys)), { richTextInjectVisibleText: 'edited ordinary text' });
    const value = h.api.jsonValue({ fields, files: { attachment: { name: 'ordinary.txt', size: 23, type: 'text/plain', lastModified: 1 } } });
    assert.equal(value.fields.richTextInjectUploadedSource, undefined);
    assert.equal(value.files.attachment.name, 'ordinary.txt');
});

test('invalid durable envelopes are rejected before any empty-data normalization or write', async () => {
    const invalid = [
        envelope({}, -1),
        { version: 2, revision: 0, data: {} },
        envelope({ shared: [], tools: {} }),
        envelope({ shared: {}, tools: { transforms: { fields: [] } } }),
        envelope({ shared: {}, tools: { transforms: { fields: {}, bytes: [1, 2, 3] } } }),
        envelope({ shared: {}, tools: {}, legacyMigrated: 'yes' }),
        envelope({ shared: {}, tools: {}, unexpected: true }),
    ];
    for (const value of invalid) {
        const h = harness(() => response(value));
        await assert.rejects(h.store.load(), /draft|format|revision|fields|tools|record|envelope/i);
        h.store.changed();
        await assert.rejects(h.store.save(() => draft('new text')), /not been loaded/);
        assert.equal(h.store.ready, false);
        assert.equal(h.requests.filter(request => request.method === 'PUT').length, 0);
    }
});

test('all records accepted by the Host optional fields/files contract can be restored', async () => {
    const h = harness(() => response(envelope({ shared: {}, tools: {
        transforms: { fields: { transformInput: 'saved input' } }, imageinject: { files: { input: { name: 'sample.png' } } }, decoder: {},
    } }, 1)));
    const restored = plain(await h.store.load());
    assert.equal(restored.tools.transforms.fields.transformInput, 'saved input');
    assert.equal(restored.tools.imageinject.files.input.name, 'sample.png');
    assert.deepEqual(restored.tools.decoder, {});
    assert.equal(h.store.ready, true);
    assert.equal(h.requests.length, 1);
});

test('a failed reload retains the accepted draft but disables subsequent saves', async () => {
    let reads = 0;
    const h = harness(request => {
        if (request.method === 'GET') {
            if (++reads === 1) return response(envelope(draft('original text'), 2));
            throw new Error('read interrupted');
        }
        return response(envelope(request.data.data, 3));
    });
    await h.store.load();
    const before = plain(h.store.data);
    await assert.rejects(h.store.load(), /read interrupted/);
    assert.deepEqual(plain(h.store.data), before);
    assert.equal(h.store.ready, false);
    h.store.changed();
    await assert.rejects(h.store.save(() => draft('edited text')), /not been loaded/);
    assert.equal(h.requests.filter(request => request.method === 'PUT').length, 0);
});

test('queued changes wait for the prior CAS commit and preserve edits arriving during its await', async () => {
    const firstWrite = Promise.withResolvers();
    const entered = Promise.withResolvers();
    let writes = 0;
    const h = harness(request => {
        if (request.method === 'GET') return response(envelope(draft('old text'), 3));
        if (++writes === 1) { entered.resolve(); return firstWrite.promise; }
        return response(envelope(request.data.data, 5));
    });
    await h.store.load();
    let current = draft('first edit');
    h.store.changed();
    const first = h.store.save(() => current);
    await entered.promise;
    current = draft('second edit');
    h.store.changed();
    const second = h.store.save(() => current);
    assert.equal(h.requests.filter(request => request.method === 'PUT').length, 1);
    firstWrite.resolve(response(envelope(draft('first edit'), 4)));
    await first;
    await second;
    const sent = h.requests.filter(request => request.method === 'PUT');
    assert.deepEqual(sent.map(request => request.data.baseRevision), [3, 4]);
    assert.deepEqual(sent.map(request => request.data.data.tools.transforms.fields.transformInput), ['first edit', 'second edit']);
    assert.equal(h.store.dirty, h.store.saved);
    assert.equal(h.store.revision, 5);
    assert.equal(h.statuses.at(-1).state, 'saved');
    assert.deepEqual(plain(h.store.data), draft('second edit'));
});

test('a CAS conflict preserves accepted state and unsaved edits without overwriting the remote change', async () => {
    const committed = envelope(draft('original text'), 1);
    const h = harness(request => request.method === 'GET' ? response(committed)
        : response({ error: { code: 'conflict', message: 'changed' }, current: envelope(draft('other window text'), 2) }, 409));
    await h.store.load();
    h.store.changed();
    const unsaved = draft('local edit');
    await assert.rejects(h.store.save(() => unsaved), /another window/);
    assert.equal(h.requests.filter(request => request.method === 'PUT').length, 1);
    assert.equal(h.store.revision, 1);
    assert.equal(h.store.saved, 0);
    assert.equal(h.store.dirty, 1);
    assert.deepEqual(plain(h.store.data), committed.data);
    assert.equal(unsaved.tools.transforms.fields.transformInput, 'local edit');
    assert.equal(h.statuses.at(-1).state, 'error');
});

test('a failed save leaves its generation dirty and does not poison a later retry', async () => {
    let writes = 0;
    const h = harness(request => request.method === 'GET' ? response(envelope(draft('old text'), 1))
        : ++writes === 1 ? response({ error: { message: 'disk full' } }, 500) : response(envelope(request.data.data, 2)));
    await h.store.load();
    h.store.changed();
    await assert.rejects(h.store.save(() => draft('local edit')), /disk full/);
    assert.equal(h.store.revision, 1);
    assert.equal(h.store.saved, 0);
    assert.equal(h.store.dirty, 1);
    assert.deepEqual(plain(h.store.data), draft('old text'));
    await h.store.save(() => draft('local edit'));
    assert.equal(h.store.revision, 2);
    assert.equal(h.store.saved, 1);
    assert.equal(h.statuses.at(-1).state, 'saved');
});

test('a malformed success response cannot mark unacknowledged data saved', async () => {
    const h = harness(request => request.method === 'GET' ? response(envelope(draft('old text'), 1))
        : response({ revision: -1, data: draft('local edit') }));
    await h.store.load();
    h.store.changed();
    await assert.rejects(h.store.save(() => draft('local edit')), /draft|format|revision|record|envelope|confirmation/i);
    assert.equal(h.store.revision, 1);
    assert.equal(h.store.saved, 0);
    assert.deepEqual(plain(h.store.data), draft('old text'));
});

test('draft serialization excludes credentials and live carrier objects while retaining file metadata', () => {
    const h = harness();
    const value = {
        shared: { openaiApiKey: 'key', authorization: 'header', refresh_token: 'refresh', credentials: { bearer: 'credential' },
            copyHistory: [{ source: 'transforms', content: 'plain text', timestamp: '2026-09-30T00:00:00Z' }] },
        tools: { imageinject: { fields: { imageSourceText: 'plain text', imageUploadDataUrl: 'data:image/png;base64,AAAA',
            imagePreviewUrl: 'blob:temporary', imageUploadBytes: new Uint8Array([1, 2, 3]), imageFile: new File(['content'], 'sample.png'),
            imageBlob: new Blob(['content']), imageBuffer: new ArrayBuffer(2), nested: { api_key: 'nested key', text: 'retained' } },
            files: { input: { name: 'sample.png', size: 7, type: 'image/png', lastModified: 123 } } } },
        legacyMigrated: true,
    };
    const saved = plain(h.api.jsonValue(value));
    assert.deepEqual(saved.shared, { copyHistory: value.shared.copyHistory });
    assert.deepEqual(saved.tools.imageinject.fields, { imageSourceText: 'plain text', nested: { text: 'retained' } });
    assert.deepEqual(saved.tools.imageinject.files.input, value.tools.imageinject.files.input);
    assert.equal(saved.legacyMigrated, true);
});

test('persistent editable fields exclude transient flags and derive text fields without mounting tools', () => {
    const h = harness();
    const data = { transformInput: 'input', transformOutput: 'output', pcLoading: false, imageUploadDataUrl: '',
        openaiApiKey: 'key', decoderInput: '', activeTransform: null, irrelevantCatalogue: [], selectedDecoder: 'auto' };
    const keys = plain(h.api.persistentKeys(data, '<textarea v-model="transformInput"></textarea><select v-model.trim="selectedDecoder"></select>'));
    assert.deepEqual(keys.sort(), ['activeTransform', 'decoderInput', 'selectedDecoder', 'transformInput', 'transformOutput'].sort());
    assert.equal(h.api.scopeKey({ kind: 'standalone' }), 'standalone');
    assert.equal(h.api.scopeKey({ kind: 'session', id: 'a' }), 'session:a');
    assert.throws(() => h.api.scopeKey({ kind: 'session', id: '' }), /Invalid workbench context/);
});
