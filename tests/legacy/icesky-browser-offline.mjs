/** Exercise native offline document, BPE, and OCR dependencies through a private real Rainy profile. */
import assert from 'node:assert/strict'
import { mkdir, writeFile } from 'node:fs/promises'
import { resolve } from 'node:path'
import { openWorkbenchHarness } from './icesky-browser-harness.mjs'

const runtime = process.argv[2]
if (!runtime) throw new Error('Pass a staged or installed Linux runtime directory')
const output = resolve('apps/rainy-desktop/validation/icesky-browser-offline')
await mkdir(output, { recursive: true })
const harness = await openWorkbenchHarness({ runtime, fixture: !process.argv.includes('--production') })
const report = { kind: 'native-browser-offline-dependencies', runtime, browser: harness.browser.version(),
  errors: [], consoleErrors: [], localRequests: [], blockedExternalHosts: [], limits: [] }
const page = await harness.context.newPage()
page.on('pageerror', error => report.errors.push(error.message))
page.on('console', message => { if (message.type() === 'error') report.consoleErrors.push(message.text()) })
harness.context.on('response', response => {
  const url = new URL(response.url())
  if (url.origin === harness.origin && /\/(?:vendor|workers)\//.test(url.pathname)) {
    report.localRequests.push({ path: url.pathname, status: response.status() })
  }
})
await page.addInitScript(() => {
  const NativeWorker = window.Worker
  const workers = window.__offlineWorkers = []
  window.Worker = class extends NativeWorker {
    constructor(url, options) {
      super(url, options)
      this.record = { url: new URL(String(url), document.baseURI).href, terminated: false, actions: [] }
      workers.push(this.record)
    }
    postMessage(message, transfer) {
      if (message?.action) this.record.actions.push(message.action)
      const result = transfer === undefined ? super.postMessage(message) : super.postMessage(message, transfer)
      if (message?.action === 'recognize' && window.__offlineCancelOnRecognize) {
        window.__offlineCancelOnRecognize = false
        queueMicrotask(() => { window.__offlineCancellation = window.__offlinePdfTool.onDeactivate(window.__offlinePdf) })
      }
      return result
    }
    terminate() { this.record.terminated = true; return super.terminate() }
  }
})

try {
  await page.goto(`${harness.origin}/rainy/icesky/index.html?embed=rainy`)
  await page.waitForFunction(() => window.app && !window.app.toolLoading)
  report.documentBase = await page.evaluate(() => document.baseURI)

  report.documents = await page.evaluate(async () => {
    await window.AssetLoader.loadScriptsSequentially(['js/tools/pdfinject/config.js', 'js/tools/pdfinject/library.js',
      'js/tools/PdfInjectTool.js', 'js/tools/DocxInjectTool.js'])
    const pdfTool = window.__offlinePdfTool = new window.PdfInjectTool()
    const pdf = window.__offlinePdf = Object.assign({}, pdfTool.getVueData(), pdfTool.getVueMethods(), { $refs: {} })
    const docxTool = new window.DocxInjectTool()
    const docx = Object.assign({}, docxTool.getVueData(), docxTool.getVueMethods())
    const Zip = await docx.docxInjectEnsureJsZip()
    const zipShared = await pdf.pdfiEnsureJsZip() === Zip
    const archive = new Zip()
    const documentXml = '<?xml version="1.0" encoding="UTF-8"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>HELLO ordinary document</w:t></w:r></w:p></w:body></w:document>'
    archive.file('word/document.xml', documentXml)
    archive.file('[Content_Types].xml', '<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>')
    const docxBytes = await archive.generateAsync({ type: 'uint8array', compression: 'DEFLATE' })
    const reopened = await Zip.loadAsync(docxBytes)
    const reopenedXml = await reopened.file('word/document.xml').async('string')
    const library = await pdf.pdfiEnsurePdfLib()
    const document = await library.PDFDocument.create()
    const font = await document.embedFont(library.StandardFonts.Helvetica)
    document.addPage([600, 240]).drawText('HELLO ordinary PDF', { x: 45, y: 130, size: 36, font })
    const pdfBytes = await document.save({ useObjectStreams: false })
    const extracted = await pdf.pdfiExtractPdfJsText(pdfBytes)
    await pdf.pdfiRenderPreviewBytes(pdfBytes)
    const result = { zipVersion: Zip.version, zipShared, docxBytes: docxBytes.length, docxXmlMatches: reopenedXml === documentXml,
      pdfBytes: pdfBytes.length, pdfPages: pdf.pdfiPreviewPageCount, pdfText: extracted,
      renderError: pdf.pdfiPreviewRenderError, renderedImage: pdf.pdfiPreviewImageUrl.startsWith('data:image/png;base64,'),
      workerUrl: window.pdfjsLib.GlobalWorkerOptions.workerSrc, cMapUrl: pdf.pdfiRuntimeUrl('cMaps'),
      standardFontUrl: pdf.pdfiRuntimeUrl('standardFonts') }
    pdf.pdfiClearRenderedPreview()
    const standardLoading = window.pdfjsLib.getDocument({ data: new Uint8Array(pdfBytes), useSystemFonts: false,
      cMapUrl: pdf.pdfiRuntimeUrl('cMaps'), cMapPacked: true, standardFontDataUrl: pdf.pdfiRuntimeUrl('standardFonts') })
    const standardDocument = await standardLoading.promise
    let standardFontRendered
    try { standardFontRendered = (await pdf.pdfiRenderPageImage(standardDocument, 1)).url.startsWith('data:image/png;base64,') }
    finally { await standardDocument.destroy() }

    const cMapDocument = await library.PDFDocument.create()
    const cMapPage = cMapDocument.addPage([600, 240])
    const context = cMapDocument.context
    const name = value => library.PDFName.of(value)
    const descriptor = context.register(context.obj({ Type: name('FontDescriptor'), FontName: name('STSong-Light'), Flags: 4,
      FontBBox: [0, -250, 1000, 880], ItalicAngle: 0, Ascent: 880, Descent: -120, CapHeight: 880, StemV: 80 }))
    const cidFont = context.register(context.obj({ Type: name('Font'), Subtype: name('CIDFontType0'), BaseFont: name('STSong-Light'),
      CIDSystemInfo: context.obj({ Registry: library.PDFString.of('Adobe'), Ordering: library.PDFString.of('GB1'), Supplement: 4 }),
      FontDescriptor: descriptor, DW: 1000 }))
    const composite = context.register(context.obj({ Type: name('Font'), Subtype: name('Type0'), BaseFont: name('STSong-Light'),
      Encoding: name('UniGB-UCS2-H'), DescendantFonts: [cidFont] }))
    cMapPage.node.set(name('Resources'), context.obj({ Font: context.obj({ F1: composite }) }))
    cMapPage.node.set(name('Contents'), context.register(context.flateStream('BT /F1 36 Tf 1 0 0 1 45 130 Tm <00480045004c004c004f> Tj ET')))
    const cMapBytes = await cMapDocument.save({ useObjectStreams: false })
    const cMapText = await pdf.pdfiExtractPdfJsText(cMapBytes)
    await pdf.pdfiRenderPreviewBytes(cMapBytes)
    const cMapRendered = pdf.pdfiPreviewImageUrl.startsWith('data:image/png;base64,') && !pdf.pdfiPreviewRenderError
    pdf.pdfiClearRenderedPreview()
    return { ...result, standardFontRendered, cMapText, cMapRendered,
      previewCleared: pdf._pdfiPreviewDoc === null && pdf._pdfiPreviewBytes === null && !pdf.pdfiPreviewImageUrl }
  })
  assert.equal(report.documents.zipShared, true)
  assert.equal(report.documents.docxXmlMatches, true)
  assert.equal(report.documents.pdfPages, 1)
  assert.equal(report.documents.renderError, '')
  assert.equal(report.documents.renderedImage, true)
  assert.equal(report.documents.pdfText[0].text, 'HELLO ordinary PDF')
  assert.equal(report.documents.standardFontRendered, true)
  assert.equal(report.documents.cMapText[0].text, 'HELLO')
  assert.equal(report.documents.cMapRendered, true)
  assert.equal(report.documents.previewCleared, true)
  await page.waitForFunction(() => window.__offlineWorkers.filter(worker => /pdf\.worker/.test(worker.url)).every(worker => worker.terminated))
  report.documents.nativePdfWorkers = await page.evaluate(() => window.__offlineWorkers.filter(worker => /pdf\.worker/.test(worker.url)))
  assert(report.documents.nativePdfWorkers.length >= 2)

  await page.evaluate(async () => { await window.app.openTool('tokenizer') })
  await page.waitForFunction(() => window.app.getToolView('tokenizer')?.$el?.isConnected)
  report.bpe = await page.evaluate(async () => {
    const view = window.app.getToolView('tokenizer')
    view._iceSkyRestoring = true
    view.tokenizerInput = 'HELLO ordinary text 世界🙂'
    const values = []
    for (const engine of ['cl100k', 'o200k', 'p50k', 'r50k']) {
      view.tokenizerEngine = engine
      await view.runTokenizer()
      values.push({ engine, error: view.tokenizerError, count: view.tokenizerTotalCount,
        tokens: view.tokenizerTokens.map(token => ({ id: token.id, text: token.text })), resultEngine: view.tokenizerResultEngine })
    }
    view._iceSkyRestoring = false
    view.cancelTokenizer()
    view._tokenizerCoordinator.dispose()
    return values
  })
  for (const value of report.bpe) {
    assert.equal(value.error, '', value.engine)
    assert.equal(value.resultEngine, value.engine)
    assert(value.count > 0 && value.tokens.every(token => Number.isInteger(token.id)), value.engine)
    assert.equal(value.tokens.map(token => token.text).join(''), 'HELLO ordinary text 世界🙂', value.engine)
  }

  const ocrStarted = performance.now()
  report.ocr = await page.evaluate(async () => {
    const pdf = window.__offlinePdf
    const canvas = document.createElement('canvas')
    canvas.width = 920; canvas.height = 240
    const context = canvas.getContext('2d')
    context.fillStyle = '#ffffff'; context.fillRect(0, 0, canvas.width, canvas.height)
    context.fillStyle = '#000000'; context.font = 'bold 112px Arial'; context.fillText('HELLO', 70, 155)
    pdf.pdfiArenaOcrEnabled = true
    pdf.pdfiPreviewImageUrl = canvas.toDataURL('image/png')
    const firstText = await pdf.pdfiRunOcrOnPreview()
    const firstWorker = pdf._pdfiOcrWorker
    const secondText = await pdf.pdfiRunOcrOnPreview()
    const reused = firstWorker === pdf._pdfiOcrWorker
    window.__offlineCancelOnRecognize = true
    const cancellation = await pdf.pdfiRunOcrOnPreview().then(
      text => ({ kind: 'completed', text }), error => ({ kind: 'rejected', name: error.name }))
    await window.__offlineCancellation
    return { firstText, secondText, reused, cancellation, workerCleared: pdf._pdfiOcrWorker === null,
      startupCleared: pdf._pdfiOcrWorkerPromise === null, abortCleared: pdf._pdfiOcrAbort === null,
      workers: window.__offlineWorkers.filter(worker => /tesseract\/worker\.min\.js/.test(worker.url)) }
  })
  report.ocr.elapsedMs = performance.now() - ocrStarted
  assert.match(report.ocr.firstText, /HELLO/)
  assert.match(report.ocr.secondText, /HELLO/)
  assert.equal(report.ocr.reused, true)
  assert.deepEqual(report.ocr.cancellation, { kind: 'rejected', name: 'AbortError' })
  assert.equal(report.ocr.workers.length, 1)
  assert.equal(report.ocr.workers[0].terminated, true)
  assert.equal(report.ocr.workerCleared && report.ocr.startupCleared && report.ocr.abortCleared, true)
  report.workers = await page.evaluate(() => window.__offlineWorkers)
  assert(report.workers.every(worker => worker.terminated), 'every native test worker must be terminated')
  report.blockedExternalHosts = [...new Set(harness.blocked)]
  assert.equal(report.blockedExternalHosts.length, 0, 'offline operations must never attempt an external origin')
  assert.equal(report.errors.length, 0)
  assert.equal(report.consoleErrors.length, 0)
  assert(report.localRequests.some(request => /\/core\/.*\.wasm\.js$/.test(request.path) && request.status === 200), 'native Tesseract core must load locally')
  assert(report.localRequests.some(request => /\/lang\/eng\.traineddata\.gz$/.test(request.path) && request.status === 200))
  assert(report.localRequests.some(request => /\/lang\/chi_sim\.traineddata\.gz$/.test(request.path) && request.status === 200))
  assert(report.localRequests.some(request => /\/standard_fonts\//.test(request.path) && request.status === 200))
  assert(report.localRequests.some(request => /\/cmaps\//.test(request.path) && request.status === 200))
  report.ocr.wasmSource = 'Embedded WebAssembly in the local upstream core .wasm.js file; no separate .wasm fetch is required.'
  report.limits.push('Benign DOCX ZIP roundtrip uses the actual tool dependency loader; no document injection or payload generation runs.')
  report.limits.push('Recognition is canceled immediately after its message is dispatched to the native worker; no claim is made about how much native computation ran before termination.')
  report.limits.push('Standard-font retrieval is forced with the PDF.js test option useSystemFonts=false. The ordinary composite-font PDF uses UniGB-UCS2-H but contains only HELLO; Chinese visual glyph fidelity is not covered.')
} catch (error) {
  report.failure = error.stack
  report.blockedExternalHosts = [...new Set(harness.blocked)]
  process.exitCode = 1
} finally {
  report.localRequests = [...new Map(report.localRequests.map(value => [value.path, value])).values()]
  await writeFile(resolve(output, 'report.json'), JSON.stringify(report, null, 2) + '\n')
  console.log(JSON.stringify({ output, failure: report.failure?.split('\n')[0], documents: report.documents,
    bpe: report.bpe?.map(value => ({ engine: value.engine, count: value.count, error: value.error })), ocr: report.ocr,
    errors: report.errors, consoleErrors: report.consoleErrors, blockedExternalHosts: report.blockedExternalHosts,
    limits: report.limits }))
  await harness.stop()
}
