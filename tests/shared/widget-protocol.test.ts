/** Interactive answer arguments, streaming markup and frame messages. */
import { expect, it } from 'vitest'
import { frameMessage, partialHtml, standaloneHtml, widgetArgs } from '../../src/shared/widget-protocol.ts'

it('reads partial arguments with defaults and a clamped height', () => {
  expect(widgetArgs(undefined)).toEqual({ title: '', height: 240, css: '', html: '', script: '' })
  expect(widgetArgs({ title: ' 图表 ', height: 99999, css: 'a{}', html: '<p>', script: 1 })).toEqual({ title: '图表', height: 2400, css: 'a{}', html: '<p>', script: '' })
  expect(widgetArgs({ height: 3 }).height).toBe(40)
  expect(widgetArgs({ height: Number.NaN }).height).toBe(240)
})

it('shows streaming markup without its unfinished tail, styles or scripts', () => {
  expect(partialHtml('<div class="a"><p>Hello</p><butt')).toBe('<div class="a"><p>Hello</p>')
  expect(partialHtml('<p>a &amp; b &am')).toBe('<p>a &amp; b ')
  expect(partialHtml('<p>x</p><script>alert(1)</script><style>p{}</style><p>y</p>')).toBe('<p>x</p><p>y</p>')
  expect(partialHtml('<p>x</p><script>while(true){')).toBe('<p>x</p>')
  expect(partialHtml('<html><head><title>t</title></head><body><h1>T</h1></body></html>')).toBe('<h1>T</h1>')
})

it('accepts only well-formed messages from a frame', () => {
  expect(frameMessage({ tag: 'rainy-widget', type: 'ready' })).toEqual({ tag: 'rainy-widget', type: 'ready' })
  expect(frameMessage({ tag: 'rainy-widget', type: 'resize', height: 120 })).toMatchObject({ height: 120 })
  expect(frameMessage({ tag: 'rainy-widget', type: 'resize', height: Infinity })).toBeUndefined()
  expect(frameMessage({ tag: 'rainy-widget', type: 'prompt', id: 1, text: 'hi' })).toMatchObject({ type: 'prompt', text: 'hi' })
  expect(frameMessage({ tag: 'rainy-widget', type: 'prompt', id: '1', text: 'hi' })).toBeUndefined()
  expect(frameMessage({ tag: 'other', type: 'ready' })).toBeUndefined()
  expect(frameMessage({ tag: 'rainy-widget', type: 'eval' })).toBeUndefined()
  expect(frameMessage('ready')).toBeUndefined()
  expect(frameMessage({ tag: 'rainy-widget', type: 'error', message: 'x'.repeat(900) })).toMatchObject({ message: 'x'.repeat(500) })
})

it('copies a widget as a standalone page', () => {
  const page = standaloneHtml({ title: 'A <b> & c', height: 100, css: 'p{color:red}', html: '<p>x</p>', script: 'document.title = "</script>"' })
  expect(page).toContain('<title>A &lt;b> &amp; c</title>')
  expect(page).toContain('<style>\np{color:red}\n</style>')
  expect(page).toContain('<script>\ndocument.title = "<\\/script>"\n</script>')
  expect(standaloneHtml({ title: '', height: 100, css: '', html: '<p>x</p>', script: ' ' })).not.toContain('<script>')
})
