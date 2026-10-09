/** Isolated native bridge and model settings for Strata controller and card tests. */
import { vi } from 'vitest'
import type { StrataNativeHost, StrataStatus } from '../src/strata-protocol.ts'
import type { RainyModelSetup } from '../src/client/settings-protocol.ts'
import { StrataController } from '../src/client/strata-controller.ts'

/** @returns Stopped bundled runtime with user-owned external model and MTP files. */
export function strataStatus(): StrataStatus {
  return {
    phase: 'stopped', settings: { modelPath: 'C:\\models\\main.gguf', mtpPath: 'C:\\models\\mtp.gguf',
      contextWindow: 32768, port: 8081, kvCache: 'int8', vramReserveMiB: 700, residentBudgetGiB: null },
    runtime: { available: true, version: '0.1.39', root: 'C:\\RainyAgent\\resources\\strata', missing: [] }, profiles: [],
    model: { sourcePath: 'C:\\models\\main.gguf', model: 'Qwen3.8-Flash-Next', ggufPath: 'C:\\models\\main.gguf',
      packPath: null, tokenizerPath: null, mtpPath: 'C:\\models\\mtp.gguf', needsPreparation: true },
    server: null, progress: null, error: null,
  }
}

/** @returns Independent bridge methods and a controller; the caller owns disposal. */
export function strataFixture() {
  const status = strataStatus()
  const bridge = {
    status: vi.fn<StrataNativeHost['status']>(async () => status),
    save: vi.fn<StrataNativeHost['save']>(async settings => ({ ...status, settings })),
    start: vi.fn<StrataNativeHost['start']>(async () => ({ ...status, phase: 'preparing', progress: 'Preparing local model files' })),
    stop: vi.fn<StrataNativeHost['stop']>(async () => status),
    selectModel: vi.fn<StrataNativeHost['selectModel']>(async () => null),
    connect: vi.fn<StrataNativeHost['connect']>(async () => ({ provider: 'rainy-strata', model: 'served-model' })),
  }
  const model: RainyModelSetup = { provider: 'rainy-strata', model: 'served-model', baseURL: 'http://127.0.0.1:8081/v1',
    contextWindow: 65536, api: 'openai-completions', local: true, thinking: 'off', thinkingFormat: 'openai', maxTokensField: 'max_tokens' }
  const configured = vi.fn(async () => model)
  const notify = vi.fn()
  const copy = { saved: () => 'Strata saved', stopped: () => 'Strata stopped', connected: () => 'Strata connected' }
  const controller = new StrataController(bridge, copy, notify, configured)
  return { status, model, controller, bridge, configured, notify, copy }
}
