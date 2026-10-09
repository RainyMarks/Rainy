/** Browser wire validation for debug values and interpreter settings. */
import { afterEach, expect, it, vi } from 'vitest'
import { brandString } from '@deepseek-ai/dsh-brand'
import type { WorkspaceId } from '../src/ide-files-protocol.ts'
import type { IdeDebugId } from '../src/ide-execution-protocol.ts'
import { createIdeExecutionApi } from '../src/client/ide-execution-api.ts'
import { executionConfigurationSchema, selectedPythonExecutable } from '../src/client/ide-execution-schema.ts'

afterEach(() => { vi.unstubAllGlobals(); vi.restoreAllMocks() })

it('rejects malformed adapter values and accepts typed child-variable handles', async () => {
  const fetcher = vi.fn<typeof fetch>().mockResolvedValueOnce(new Response(JSON.stringify({ ok: true, value: [
    { name: 'bad', value: 'bad', variablesReference: 'not-a-number' },
  ] }), { status: 200 })).mockResolvedValueOnce(new Response(JSON.stringify({ ok: true, value: [
    { name: 'counter', value: '3', type: 'int', variablesReference: 0 },
  ] }), { status: 200 }))
  vi.stubGlobal('fetch', fetcher)
  const api = createIdeExecutionApi()
  const request = { op: 'debug.variables' as const, workspaceId: brandString<WorkspaceId>('workspace-a'),
    debugId: brandString<IdeDebugId>('debug-a'), variablesReference: 4 }
  await expect(api.request(request)).rejects.toMatchObject({ code: 'invalid-response' })
  await expect(api.request(request)).resolves.toEqual([{ name: 'counter', value: '3', type: 'int', variablesReference: 0 }])
  expect(fetcher.mock.calls[0]?.[0]).toBe('/rainy/ide')
  expect(fetcher.mock.calls[0]?.[1]).toMatchObject({ method: 'POST', credentials: 'same-origin' })
})

it('retains Python module configuration and selects the active profile interpreter consistently', () => {
  const configuration = executionConfigurationSchema.parse({ profiles: [
    { name: 'default', language: 'python', program: 'main.py' },
    { name: 'venv', language: 'python', program: 'main.py', pythonModule: 'demo.main', executable: '/venv/bin/python' },
  ], activeProfile: 'venv', breakpoints: [], watches: [] })
  expect(configuration.profiles[1]?.pythonModule).toBe('demo.main')
  expect(selectedPythonExecutable(configuration)).toBe('/venv/bin/python')
  expect(selectedPythonExecutable({ ...configuration, activeProfile: 'default' })).toBeUndefined()
})
