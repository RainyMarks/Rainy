# IceSky integration patch

The bundled source is pinned to `spindriftpapilio/icesky` commit `e0321b41ade55b288466203c68fad96ab6c18455`. The upstream README is stored byte-for-byte as `UPSTREAM_README.txt` so it is not treated as a RainyAgent bilingual document. [The desktop README](../../README.md) owns the user-visible behavior and focused validation commands.

## Embedding and requests

RainyAgent changes `js/utils/openaiClient.js` so OpenAI-compatible and Anthropic requests use the authenticated local `/api/` relay for all configured Base URLs, including WSL localhost endpoints. The relay forwards each request without persisting its key or body. The root application workspace owns one workbench iframe. `css/rainy-embed.css` and the runtime bridge adapt its layout, resolved semantic colors, typography and locale to the application. Bridge messages require the expected source window and origin.

## Tool loading and drafts

The metadata catalog, external templates and shared bootstrap load each selected tool as a Vue component. The runtime owns tool activation, cancellation, cleanup and context changes. Transform, decoder and tokenizer patches use cancellable workers; the tokenizer worker retains its full result while returning one page to the view.

`js/app/persistence.js` and `js/app/runtime.js` replace per-tool browser draft writes with versioned snapshots in authenticated Host storage. The snapshots separate conversation drafts from the standalone draft and retain editable text, options, text results and selected-file metadata. Credentials, binary contents and transient workers or requests are excluded. Existing standalone PromptCraft text imports once; its browser record is removed only after the Host confirms the save. Context changes and application exit use the flush acknowledgement so failed saves leave the current contents available for retry or export.

## Offline dependencies and asset versions

Document and OCR dependencies resolve from the versioned local asset base. JSZip, PDF-lib, PDF.js with its worker, CMaps and standard fonts, and Tesseract with its worker, WASM core and Chinese/English language data are bundled locally. Their exact package versions and acquisition integrity values live in [`icesky-vendor-lock.json`](../../scripts/icesky-vendor-lock.json); file digests and dependency identities live in [`OFFLINE_DEPENDENCIES.json`](js/vendor/OFFLINE_DEPENDENCIES.json). `prepare-icesky-vendor.mjs --verify` checks these records during the desktop build, and `icesky-manifest.mjs` regenerates [`assets-manifest.json`](assets-manifest.json) before bundling. Static responses use the recorded version, digest, MIME type and size.

The controlled resource tests load actual packaged library entrypoints and exercise OCR worker ownership with a fixture worker. The real browser checks execute the local WASM OCR worker, PDF renderer and four BPE encodings with external origins blocked. [The validation record](../../validation/ICESKY.md) distinguishes these observations from installed desktop and document-fidelity coverage.
