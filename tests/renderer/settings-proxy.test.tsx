// @vitest-environment happy-dom
/** Settings → Models network proxy: shows the proxy in effect, tests a choice before saving, and saves a manual address. */
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import type { ProxySettings as ProxyChoice, ProxyStatus, UiPreferences } from '../../src/shared/rpc.ts'
import { useAction } from '../../src/renderer/settings/parts.tsx'
import { ProxySettings } from '../../src/renderer/settings/ProxySettings.tsx'
import { button, choose, cleanup, emit, fakeHost, handle, hasText, render, type, waitFor } from './settings-harness.tsx'

vi.mock('../../src/renderer/rpc.ts', async () => (await import('./settings-harness.tsx')).rpcModule)
vi.mock('../../src/renderer/ui/toasts.tsx', async () => (await import('./settings-harness.tsx')).toastsModule)

const prefs: UiPreferences = { locale: 'zh', theme: 'system', uiFontSize: 14, codeFontSize: 13, busyEnter: 'queue', stepDetail: 'standard', showUsage: true, colorScheme: 'graphite', accent: 'auto', uiFont: '', codeFont: '' }
const calls = (method: string): unknown[] => fakeHost.call.mock.calls.filter(([name]) => name === method).map(([, params]) => params)

function Card(): JSX.Element {
  return <ProxySettings action={useAction()} />
}

beforeEach(async () => {
  handle('prefs.get', () => prefs)
  await emit('prefs.changed', prefs)
})
afterEach(async () => { await cleanup() })

it('shows the system proxy, tests a manual address and saves it', async () => {
  let status: ProxyStatus = { settings: { mode: 'system' }, effective: 'http://127.0.0.1:7890', system: 'http://127.0.0.1:7890' }
  handle('network.status', () => status)
  handle('network.test', () => ({ ok: true, url: 'https://api.deepseek.com', status: 401, ms: 120 }))
  handle('network.set', (choice: ProxyChoice) => {
    status = { settings: choice, effective: `http://${choice.url ?? ''}`, system: status.system }
    return status
  })
  await render(<Card />)
  await waitFor(() => { expect(hasText('系统代理：http://127.0.0.1:7890')).toBe(true) })
  expect(document.body.textContent).toContain('当前使用 http://127.0.0.1:7890')
  await choose('代理方式', '手动设置')
  await type(document.querySelector<HTMLInputElement>('input[aria-label="代理地址"]')!, '10.0.0.2:3128')
  await (async () => { button('测试连接').click() })()
  await waitFor(() => { expect(document.body.textContent).toContain('可以连接 https://api.deepseek.com（HTTP 401，120 毫秒）') })
  expect(calls('network.test')).toEqual([{ mode: 'manual', url: '10.0.0.2:3128' }])
  button('保存').click()
  await waitFor(() => { expect(calls('network.set')).toEqual([{ mode: 'manual', url: '10.0.0.2:3128' }]) })
  await waitFor(() => { expect(document.body.textContent).toContain('当前使用 http://10.0.0.2:3128') })
})
