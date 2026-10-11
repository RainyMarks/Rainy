/** Terminals accept resizes until and after their program exits; a layout change racing the exit is not an error. */
import { expect, it } from 'vitest'
import { spawnTerminal } from '../../src/host/process.ts'

it('ignores resizes that arrive while or after the program exits', async () => {
  const terminal = await spawnTerminal({ argv: [process.execPath, '-e', 'process.stdout.write("done")'], cwd: process.cwd(), cols: 80, rows: 24 })
  terminal.output.resume()
  let finished = false
  const done = terminal.done.then((outcome) => { finished = true; return outcome })
  const resizes: Promise<void>[] = []
  for (let size = 0; !finished; size++) {
    resizes.push(terminal.resize(80 + size % 20, 24))
    await new Promise(resolve => setTimeout(resolve, 5))
  }
  expect((await done).exitCode).toBe(0)
  await expect(Promise.all(resizes)).resolves.toBeDefined()
  await expect(terminal.resize(100, 30)).resolves.toBeUndefined()
}, 30000)
