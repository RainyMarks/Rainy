/** Messages the Host writes follow the interface language chosen in Settings. */
import { afterEach, expect, it } from 'vitest'
import { parseDiscovery } from '../../src/host/agent/models.ts'
import { resolveBudget } from '../../src/shared/budget.ts'
import { currentLocale, followLocale, localized } from '../../src/shared/locale.ts'

afterEach(() => { followLocale(() => 'zh') })

it('writes errors in the interface language, Chinese until the language is known', () => {
  const ftp = { provider: 'local', baseURL: 'ftp://models.example', api: 'openai-responses' }
  expect(currentLocale()).toBe('zh')
  expect(() => parseDiscovery(ftp)).toThrow('服务地址必须是 HTTP(S) Base URL，不能包含密钥或查询参数。')
  let locale: 'zh' | 'en' = 'en'
  followLocale(() => locale)
  expect(() => parseDiscovery(ftp)).toThrow('The service address must be an HTTP(S) base URL without credentials or query parameters.')
  expect(() => resolveBudget(1000)).toThrow('at least 4096')
  locale = 'zh'
  expect(localized({ zh: '已停止。', en: 'Stopped.' })).toBe('已停止。')
})
