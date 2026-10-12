/** The carrier channel: replies settle Host requests at once, commands wait for their handlers, and closing stops the Host. */
import { PassThrough } from 'node:stream'
import { expect, it, vi } from 'vitest'
import { ControlInput, type ControlHandlers } from '../../src/host/control.ts'

function channel() {
  const input = new PassThrough()
  const sent: ({ type: string } & Record<string, unknown>)[] = []
  const control = new ControlInput(input, (message) => { sent.push(message) })
  const write = (message: unknown): void => { input.write(`${JSON.stringify(message)}\n`) }
  return { input, sent, control, write }
}

const handlers = (): ControlHandlers & { stop: ReturnType<typeof vi.fn> } => ({
  inspectActivity: () => true,
  inspectProject: () => Promise.resolve(null),
  stop: vi.fn(() => Promise.resolve()),
})

it('answers requests by id while commands that came early wait until they are served', async () => {
  const { sent, control, write } = channel()
  write({ type: 'inspect-activity', id: 'early' })
  const answer = control.ask({ type: 'system-proxy', url: 'https://api.openai.com/' })
  const request = sent.at(-1)
  expect(request).toMatchObject({ type: 'system-proxy', url: 'https://api.openai.com/' })
  write({ type: 'reply', id: 'someone-else', url: 'http://wrong:1' })
  write({ type: 'reply', id: request?.id, url: 'http://127.0.0.1:7890' })
  expect(await answer).toMatchObject({ url: 'http://127.0.0.1:7890' })
  expect(sent).toHaveLength(1)
  control.serve(handlers(), true)
  expect(sent.at(-1)).toEqual({ type: 'activity', id: 'early', active: true })
})

it('rejects a request the carrier does not answer in time or before the channel closes, and stops the Host on close', async () => {
  vi.useFakeTimers()
  try {
    const { input, control } = channel()
    const late = control.ask({ type: 'system-proxy', url: 'https://a.example/' }, 1000)
    const assertion = expect(late).rejects.toThrow('did not answer')
    await vi.advanceTimersByTimeAsync(1000)
    await assertion
    const served = handlers()
    control.serve(served, true)
    const pending = control.ask({ type: 'system-proxy', url: 'https://b.example/' })
    input.end()
    await expect(pending).rejects.toThrow('closed')
    await vi.waitFor(() => { expect(served.stop).toHaveBeenCalledOnce() })
    await expect(control.ask({ type: 'system-proxy', url: 'https://c.example/' })).rejects.toThrow('closed')
  } finally { vi.useRealTimers() }
})
