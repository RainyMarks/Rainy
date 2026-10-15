/** The askpass bridge: node.exe preloads the client, which asks Rainy over a pipe and prints the answer for ssh. */
import { spawn } from 'node:child_process'
import { mkdtemp, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { afterEach, expect, it } from 'vitest'
import { isHostKeyQuestion, startAskpass, type Askpass, type AskpassQuestion } from '../../src/main/ssh-askpass.ts'

const cleanups: (() => Promise<void>)[] = []
afterEach(async () => { for (const cleanup of cleanups.splice(0).reverse()) await cleanup() })

async function server(answers: (string | null)[]): Promise<{ askpass: Askpass; asked: AskpassQuestion[]; directory: string }> {
  const directory = await mkdtemp(join(tmpdir(), 'rainy-askpass-'))
  cleanups.push(() => rm(directory, { recursive: true, force: true }))
  const asked: AskpassQuestion[] = []
  const askpass = await startAskpass({ node: process.execPath, directory, ask: async (question) => { asked.push(question); return answers.shift() ?? null } })
  cleanups.push(() => askpass.close())
  return { askpass, asked, directory }
}

/** Start the askpass program the way ssh does: the bundled node with the question as its only argument. */
function ask(environment: Record<string, string>, prompt: string, cwd: string): Promise<{ code: number | null; stdout: string }> {
  return new Promise((resolve, reject) => {
    const child = spawn(environment.SSH_ASKPASS!, [prompt], { cwd, env: { ...process.env, ...environment }, stdio: ['ignore', 'pipe', 'pipe'] })
    let stdout = ''
    child.stdout.setEncoding('utf8')
    child.stdout.on('data', (data: string) => { stdout += data })
    child.once('error', reject)
    child.once('close', (code) => { resolve({ code, stdout }) })
  })
}

it('restores the question node turned into a path, and prints the answer', async () => {
  const { askpass, asked, directory } = await server(['s3cret', 'yes'])
  const session = askpass.session()
  const passphrase = 'Enter passphrase for key \'C:/Users/me/.ssh/id_ed25519\': '
  expect(await ask(session.environment, passphrase, directory)).toEqual({ code: 0, stdout: 's3cret\n' })
  const hostKey = 'The authenticity of host \'[dev]:2222\' can\'t be established.\nED25519 key fingerprint is SHA256:abc+def/ghi.\n'
    + 'Are you sure you want to continue connecting (yes/no/[fingerprint])? '
  expect(await ask(session.environment, hostKey, directory)).toEqual({ code: 0, stdout: 'yes\n' })
  expect(asked).toEqual([{ kind: 'secret', prompt: passphrase }, { kind: 'host-key', prompt: hostKey }])
})

it('answers a later run from memory, asks again after a wrong answer, and refuses unknown or cancelled runs', async () => {
  const { askpass, asked, directory } = await server(['wrong', 'right', null])
  const prompt = 'alice@dev\'s password: '
  const first = askpass.session()
  expect((await ask(first.environment, prompt, directory)).stdout).toBe('wrong\n')
  // The same run asking again means the first answer did not work.
  expect((await ask(first.environment, prompt, directory)).stdout).toBe('right\n')
  first.dispose()
  const second = askpass.session()
  expect((await ask(second.environment, prompt, directory)).stdout).toBe('right\n')
  expect(asked).toHaveLength(2)
  second.failed()
  const third = askpass.session()
  expect((await ask(third.environment, prompt, directory)).code).toBe(1)
  expect((await ask({ ...third.environment, RAINY_ASKPASS_TOKEN: 'forged' }, prompt, directory)).code).toBe(1)
  expect(asked).toHaveLength(3)
})

it('recognizes the host key question', () => {
  expect(isHostKeyQuestion('Are you sure you want to continue connecting (yes/no/[fingerprint])? ')).toBe(true)
  expect(isHostKeyQuestion('Password: ')).toBe(false)
})
