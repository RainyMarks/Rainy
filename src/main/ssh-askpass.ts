/**
 * Answers the questions `ssh` asks (host key, password, key passphrase, one-time code) in Rainy's own windows.
 * `ssh` runs the bundled node.exe as its askpass program; `NODE_OPTIONS` preloads a small client that sends the
 * question over a named pipe and prints the answer. The preload stops Node from loading the question as a script.
 */
import { randomUUID } from 'node:crypto'
import { writeFile } from 'node:fs/promises'
import { createServer, type Socket } from 'node:net'
import { join } from 'node:path'

/** One question; `host-key` asks to trust a server seen for the first time. */
export interface AskpassQuestion { kind: 'host-key' | 'secret'; prompt: string }

/**
 * Node resolves its first argument as a script path before preloads run, so the question arrives prefixed with the
 * working directory and with `/` turned into `\`; the client undoes both. ssh never puts `\` in its questions.
 */
export const ASKPASS_CLIENT = `'use strict'
require('module').runMain = () => {}
const net = require('net')
const path = require('path')
let prompt = process.argv[1] || ''
const base = process.cwd().replace(/[\\\\/]$/, '') + path.sep
if (prompt.startsWith(base)) prompt = prompt.slice(base.length)
prompt = prompt.split('\\\\').join('/')
const fail = () => { process.exit(1) }
const socket = net.connect(process.env.RAINY_ASKPASS_PIPE || '')
let reply = ''
socket.setEncoding('utf8')
socket.on('connect', () => { socket.write(JSON.stringify({ token: process.env.RAINY_ASKPASS_TOKEN, prompt }) + '\\n') })
socket.on('data', (data) => { reply += data })
socket.on('end', () => {
  let answer
  try { answer = JSON.parse(reply).answer } catch { fail(); return }
  if (typeof answer !== 'string') { fail(); return }
  process.stdout.write(answer + '\\n', () => { process.exit(0) })
})
socket.on('error', fail)
`

/** One ssh run's askpass identity. */
export interface AskpassSession {
  /** Variables that make this run's ssh ask Rainy. */
  readonly environment: Record<string, string>
  /** The run failed to log in: forget the answers it used, so the next run asks again. */
  failed(): void
  dispose(): void
}

/** The askpass server of one application run. */
export interface Askpass {
  session(): AskpassSession
  close(): Promise<void>
}

/** @param prompt - ssh question. @returns whether it asks to trust an unknown host key. */
export function isHostKeyQuestion(prompt: string): boolean {
  return /continue connecting \(yes\/no/u.test(prompt)
}

/**
 * Start the pipe server and write the client next to the other per-user files.
 * Answers are remembered for this application run only, so the several connections of one start ask once;
 * a run that asks the same question again is told the answer was wrong and the user is asked anew.
 * @param options.node - node.exe that ssh starts.
 * @param options.directory - per-user directory for the client script.
 * @param options.ask - shows one question; `null` cancels.
 * @returns the server.
 */
export async function startAskpass(options: { node: string; directory: string; ask: (question: AskpassQuestion) => Promise<string | null> }): Promise<Askpass> {
  const client = join(options.directory, 'ssh-askpass.cjs')
  await writeFile(client, ASKPASS_CLIENT, 'utf8')
  const pipe = process.platform === 'win32' ? `\\\\.\\pipe\\rainy-askpass-${randomUUID()}` : join(options.directory, `askpass-${randomUUID()}.sock`)
  const remembered = new Map<string, string>()
  const sessions = new Map<string, Set<string>>()
  let queue: Promise<unknown> = Promise.resolve()

  const answer = async (used: Set<string>, prompt: string): Promise<string | null> => {
    const kind = isHostKeyQuestion(prompt) ? 'host-key' : 'secret'
    if (kind === 'secret' && remembered.has(prompt) && !used.has(prompt)) { used.add(prompt); return remembered.get(prompt)! }
    // A question asked twice in one run means its earlier answer did not work.
    if (used.has(prompt)) remembered.delete(prompt)
    const reply = await options.ask({ kind, prompt })
    if (reply !== null && kind === 'secret') { remembered.set(prompt, reply); used.add(prompt) }
    return reply
  }
  const server = createServer((socket: Socket) => {
    let text = ''
    socket.setEncoding('utf8')
    socket.on('error', () => { socket.destroy() })
    socket.on('data', (data: string) => {
      text += data
      if (text.length > 16384) { socket.destroy(); return }
      const end = text.indexOf('\n')
      if (end < 0) return
      let request: unknown
      try { request = JSON.parse(text.slice(0, end)) } catch { socket.destroy(); return }
      const used = request !== null && typeof request === 'object' && 'token' in request && typeof request.token === 'string' ? sessions.get(request.token) : undefined
      if (used === undefined || !('prompt' in (request as object)) || typeof (request as { prompt: unknown }).prompt !== 'string') { socket.end('{}'); return }
      const prompt = (request as { prompt: string }).prompt.slice(0, 4096)
      // One question at a time; a queued one may find the answer the previous one just received.
      const result = queue.then(() => answer(used, prompt))
      queue = result.catch(() => undefined)
      void result.then((reply) => { socket.end(JSON.stringify(reply === null ? {} : { answer: reply })) }, () => { socket.end('{}') })
    })
  })
  await new Promise<void>((resolve, reject) => { server.once('error', reject); server.listen(pipe, () => { server.off('error', reject); resolve() }) })
  return {
    session() {
      const token = randomUUID()
      const used = new Set<string>()
      sessions.set(token, used)
      return {
        environment: { SSH_ASKPASS: options.node, SSH_ASKPASS_REQUIRE: 'force', DISPLAY: 'rainy',
          NODE_OPTIONS: `--require "${client.replaceAll('\\', '/')}"`, RAINY_ASKPASS_PIPE: pipe, RAINY_ASKPASS_TOKEN: token },
        failed() { for (const prompt of used) remembered.delete(prompt) },
        dispose() { sessions.delete(token) },
      }
    },
    close: () => new Promise<void>((resolve) => { server.close(() => { resolve() }) }),
  }
}
