// @vitest-environment happy-dom
/** The Rainy mark rains while asked, lets a running fall finish when the rain stops, and plays its entrance once. */
import { afterEach, expect, it, vi } from 'vitest'
import { RainyMark } from '../../src/renderer/ui/RainyMark.tsx'
import { cleanup, flush, render, waitFor } from './settings-harness.tsx'

afterEach(async () => { await cleanup(); vi.restoreAllMocks() })

const mark = (): SVGSVGElement => document.querySelector<SVGSVGElement>('[data-rainy-mark]')!

it('rains while asked and stops at once when nothing is falling', async () => {
  const view = await render(<RainyMark size={18} raining />)
  expect(mark().hasAttribute('data-raining')).toBe(true)
  await view.rerender(<RainyMark size={18} raining={false} />)
  await waitFor(() => { expect(mark().hasAttribute('data-raining')).toBe(false) })
})

it('keeps raining until the fall in progress ends, and leaves bars still waiting at rest', async () => {
  let finish: () => void = () => undefined
  const falling = {
    effect: { getComputedTiming: () => ({ currentIteration: 3 }), updateTiming: vi.fn() },
    finished: new Promise<void>((resolve) => { finish = resolve }),
    cancel: vi.fn(),
  }
  const waiting = { effect: { getComputedTiming: () => ({ currentIteration: null }), updateTiming: vi.fn() }, finished: new Promise(() => undefined), cancel: vi.fn() }
  const view = await render(<RainyMark size={18} raining />)
  Object.defineProperty(mark(), 'getAnimations', { value: () => [falling, waiting] })
  await view.rerender(<RainyMark size={18} raining={false} />)
  await flush()
  expect(falling.effect.updateTiming).toHaveBeenCalledWith({ iterations: 4 })
  expect(waiting.cancel).toHaveBeenCalled()
  expect(mark().hasAttribute('data-raining')).toBe(true)
  finish()
  await waitFor(() => { expect(mark().hasAttribute('data-raining')).toBe(false) })
})

it('drops the entrance after its last bar lands, so a later shower does not replay it', async () => {
  await render(<RainyMark size={56} entrance />)
  expect(mark().hasAttribute('data-entrance')).toBe(true)
  const last = mark().querySelector('[data-bar="2"]')!
  last.dispatchEvent(new Event('animationend', { bubbles: true }))
  await flush()
  expect(mark().hasAttribute('data-entrance')).toBe(false)
})
