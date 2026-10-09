/** Offline browser dependency paths and the managed OCR lifecycle, without generating injection samples. */
import assert from 'node:assert/strict';
import { readFile, access } from 'node:fs/promises';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import vm from 'node:vm';
import test from 'node:test';

const resourceRoot = fileURLToPath(new URL('../../resources/icesky/', import.meta.url));
const assetBase = 'http://127.0.0.1:9941/rainy/icesky/v/' + 'a'.repeat(64) + '/';
const plain = value => JSON.parse(JSON.stringify(value));

async function runtime(options = {}) {
  const requested = [], workers = [];
  const started = Promise.withResolvers();
  const recognizing = Promise.withResolvers();
  class FixtureWorker {
    constructor(url) { this.url = String(url); this.messages = []; this.terminated = false; workers.push(this); }
    postMessage(message) {
      this.messages.push(message);
      if (message.action === 'load') started.resolve();
      if (message.action === 'recognize') recognizing.resolve();
      if (options.holdStartup && message.action === 'load') { this.startup = message; return; }
      if (options.holdRecognition && message.action === 'recognize') return;
      this.reply(message);
    }
    reply(message) {
      queueMicrotask(() => this.onmessage?.({ data: { ...message, status: 'resolve', data: message.action === 'recognize' ? { text: '  Plain fixture text  ' } : {} } }));
    }
    terminate() { this.terminated = true; }
  }
  const scope = vm.createContext({ URL, Blob, TextEncoder, TextDecoder, Uint8Array, Uint32Array, ArrayBuffer, DOMException, AbortController, console, btoa, atob, setTimeout, clearTimeout, queueMicrotask, Worker: FixtureWorker,
    document: { baseURI: assetBase, createElement: () => ({}) }, location: { href: assetBase + 'index.html', origin: 'http://127.0.0.1:9941' },
    fetch: async () => { throw new Error('Runtime fixture forbids network fetch.'); },
  });
  scope.window = scope; scope.self = scope;
  const load = async name => vm.runInContext(await readFile(resolve(resourceRoot, name), 'utf8'), scope, { filename: name });
  scope.AssetLoader = { async loadScriptOnce(url) {
    requested.push(String(url));
    assert.ok(String(url).startsWith(assetBase), 'every executable dependency must use the versioned local base');
    const path = String(url).slice(assetBase.length);
    if (options.missing === path) throw new Error('Missing packaged dependency.');
    await load(path);
  } };
  await load('js/tools/Tool.js');
  await load('js/tools/pdfinject/config.js');
  await load('js/tools/pdfinject/library.js');
  await load('js/tools/PdfInjectTool.js');
  await load('js/tools/DocxInjectTool.js');
  const tool = new scope.PdfInjectTool();
  const pdf = Object.assign({}, tool.getVueData(), tool.getVueMethods());
  const docxTool = new scope.DocxInjectTool();
  const docx = Object.assign({}, docxTool.getVueData(), docxTool.getVueMethods());
  pdf.pdfiArenaOcrEnabled = true;
  pdf.pdfiPreviewImageUrl = 'data:image/png;base64,AA==';
  return { scope, load, pdf, docx, tool, requested, workers, started, recognizing };
}

test('DOCX and PDF load the actual pinned local JSZip, PDF-lib and PDF.js entrypoints', async () => {
  const { scope, pdf, docx, requested } = await runtime();
  const zip = await docx.docxInjectEnsureJsZip();
  assert.equal(typeof zip, 'function');
  assert.equal(await pdf.pdfiEnsureJsZip(), zip, 'both workbenches reuse the same loaded browser dependency');
  const pdfLib = await pdf.pdfiEnsurePdfLib();
  assert.equal(typeof pdfLib.PDFDocument.create, 'function');
  const pdfJs = await pdf.pdfiEnsurePdfJs();
  assert.equal(pdfJs, scope.pdfjsLib);
  assert.equal(pdfJs.GlobalWorkerOptions.workerSrc, assetBase + 'js/vendor/pdfjs/pdf.worker.min.js');
  assert.deepEqual(requested, [assetBase + 'js/vendor/jszip/jszip.min.js', assetBase + 'js/vendor/pdf-lib/pdf-lib.min.js', assetBase + 'js/vendor/pdfjs/pdf.min.js']);
});

test('PDF.js extraction receives local CMaps and standard font directories', async () => {
  const { scope, pdf } = await runtime();
  let observed;
  scope.pdfjsLib = { GlobalWorkerOptions: {}, getDocument(options) {
    observed = options;
    return { promise: Promise.resolve({ numPages: 1, getPage: async () => ({ getTextContent: async () => ({ items: [{ str: 'Plain fixture' }] }) }), destroy: async () => {} }) };
  } };
  assert.deepEqual(plain(await pdf.pdfiExtractPdfJsText(new Uint8Array([1, 2]))), [{ pageNumber: 1, text: 'Plain fixture' }]);
  assert.equal(observed.cMapUrl, assetBase + 'js/vendor/pdfjs/cmaps/');
  assert.equal(observed.standardFontDataUrl, assetBase + 'js/vendor/pdfjs/standard_fonts/');
  assert.equal(scope.pdfjsLib.GlobalWorkerOptions.workerSrc, assetBase + 'js/vendor/pdfjs/pdf.worker.min.js');
});

test('the actual Tesseract browser client creates one managed local worker with full core and language paths', async () => {
  const { pdf, requested, workers } = await runtime();
  try {
    assert.equal(await pdf.pdfiRunOcrOnPreview(), 'Plain fixture text');
    assert.equal(await pdf.pdfiRunOcrOnPreview(), 'Plain fixture text');
    assert.equal(workers.length, 1);
    assert.deepEqual(requested, [assetBase + 'js/vendor/tesseract/tesseract.min.js']);
    const worker = workers[0];
    assert.equal(worker.url, assetBase + 'js/vendor/tesseract/worker.min.js');
    const load = worker.messages.find(message => message.action === 'load');
    const language = worker.messages.find(message => message.action === 'loadLanguage');
    assert.equal(load.payload.options.corePath, assetBase + 'js/vendor/tesseract/core/');
    assert.equal(language.payload.options.langPath, assetBase + 'js/vendor/tesseract/lang/');
    assert.equal(language.payload.options.cacheMethod, 'none');
    assert.equal(language.payload.options.gzip, true);
    assert.deepEqual(plain(language.payload.langs), ['chi_sim', 'eng']);
    for (const name of ['tesseract-core', 'tesseract-core-simd', 'tesseract-core-lstm', 'tesseract-core-simd-lstm']) {
      await access(resolve(resourceRoot, 'js/vendor/tesseract/core/' + name + '.wasm.js'));
      await access(resolve(resourceRoot, 'js/vendor/tesseract/core/' + name + '.wasm'));
    }
    for (const name of ['chi_sim', 'eng']) await access(resolve(resourceRoot, 'js/vendor/tesseract/lang/' + name + '.traineddata.gz'));
  } finally { await pdf.pdfiStopOcr(); }
  assert.equal(workers[0].terminated, true);
  assert.equal(pdf._pdfiOcrWorker, null);
});

test('stopping a pending initialization owns its completion and immediately terminates the late worker', async () => {
  const { pdf, workers, started } = await runtime({ holdStartup: true });
  const recognition = pdf.pdfiRunOcrOnPreview();
  const cancelled = assert.rejects(recognition, error => error.name === 'AbortError');
  await started.promise;
  const stopping = pdf.pdfiStopOcr();
  const worker = workers[0];
  worker.reply(worker.startup);
  await stopping;
  await cancelled;
  assert.equal(worker.terminated, true);
  assert.equal(pdf._pdfiOcrWorker, null);
  assert.equal(pdf._pdfiOcrWorkerPromise, null);
});

test('deactivation cancels an active recognition without publishing an OCR result', async () => {
  const { pdf, tool, workers, recognizing } = await runtime({ holdRecognition: true });
  const ready = await pdf.pdfiEnsureOcrWorker();
  assert.ok(ready);
  const recognition = pdf.pdfiRunOcrOnPreview();
  const cancelled = assert.rejects(recognition, error => error.name === 'AbortError');
  await recognizing.promise;
  await tool.onDeactivate(pdf);
  await cancelled;
  assert.equal(workers[0].terminated, true);
});

test('missing packaged dependencies reject explicitly and can be retried', async () => {
  const state = await runtime({ missing: 'js/vendor/jszip/jszip.min.js' });
  await assert.rejects(state.docx.docxInjectEnsureJsZip(), /无法加载本地 JSZip/);
  await assert.rejects(state.docx.docxInjectEnsureJsZip(), /无法加载本地 JSZip/);
  assert.equal(state.requested.length, 2);
  assert.ok(state.requested.every(url => url.startsWith(assetBase)));
  const pdfState = await runtime({ missing: 'js/vendor/pdfjs/pdf.min.js' });
  await assert.rejects(pdfState.pdf.pdfiEnsurePdfJs(), /无法加载本地 pdfjsLib/);
});
