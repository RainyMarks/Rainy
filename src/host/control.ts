/**
 * stdio channel to the Electron carrier.
 *
 * stdout lines `RAINY_CONTROL {json}`: `ready`, `fatal`, `activity`, `project`, and the Host's own requests
 * `system-proxy {id, url}` and `preview {id, …}` (the agent's preview window).
 * stdin JSON lines: `stop`, `inspect-activity {id, mode?}`, `inspect-project {id, workspaceId?}`, and `reply {id, …}`
 * answering a Host request.
 * All other Host output goes to stderr.
 */
import { randomUUID } from 'node:crypto'
import { createInterface } from 'node:readline'
import type { IdeWorkspaceRoot } from '../shared/ide-files-protocol.ts'

/** Carrier view of the selected project. */
export interface ControlProject { projectId: string; workspaceId: string; roots: readonly IdeWorkspaceRoot[] }

/** Answers to carrier requests. */
export interface ControlHandlers {
  inspectActivity(mode: 'observe' | 'freeze' | 'resume'): boolean
  /** @returns The selected or named project, `null` when none is selected; rejects when it is unavailable. */
  inspectProject(workspaceId: string | undefined): Promise<ControlProject | null>
  stop(): Promise<void>
}

type Command = Record<string, unknown>

/**
 * Write one control message to stdout.
 * @param message JSON-serializable message with a `type`.
 */
export function sendControl(message: { type: string } & Record<string, unknown>): void {
  process.stdout.write(`RAINY_CONTROL ${JSON.stringify(message)}\n`)
}

/** Lines from the carrier. Replies settle Host requests at any time; commands wait until they are served. */
export class ControlInput {
  private readonly early: Command[] = []
  private handler: ((command: Command) => void) | undefined
  private readonly replies = new Map<string, { resolve(reply: Command): void; reject(error: Error): void }>()
  private readonly closeListeners = new Set<() => void>()
  private closed = false

  /**
   * @param input Carrier stream, normally stdin.
   * @param send Writes one control message to the carrier.
   */
  constructor(input: NodeJS.ReadableStream, private readonly send: (message: { type: string } & Record<string, unknown>) => void) {
    const lines = createInterface({ input, crlfDelay: Infinity })
    lines.on('line', (line) => {
      let command: Command
      try {
        const parsed: unknown = JSON.parse(line)
        if (parsed === null || typeof parsed !== 'object') return
        command = parsed as Command
      } catch (_error) {
        console.error('Invalid control message')
        return
      }
      if (command.type === 'reply') {
        if (typeof command.id !== 'string') return
        const pending = this.replies.get(command.id)
        this.replies.delete(command.id)
        pending?.resolve(command)
      } else if (this.handler !== undefined) this.handler(command)
      else this.early.push(command)
    })
    lines.on('close', () => {
      this.closed = true
      for (const pending of this.replies.values()) pending.reject(new Error('The carrier closed the control channel'))
      this.replies.clear()
      for (const listener of this.closeListeners) listener()
    })
  }

  /**
   * Ask the carrier and wait for its reply.
   * @param request Request with a `type`; an `id` is added.
   * @param timeoutMs Time to wait for the reply.
   * @returns The reply's fields.
   */
  ask(request: { type: string } & Record<string, unknown>, timeoutMs = 5000): Promise<Command> {
    if (this.closed) return Promise.reject(new Error('The carrier closed the control channel'))
    const id = randomUUID()
    const { promise, resolve, reject } = Promise.withResolvers<Command>()
    const timer = setTimeout(() => {
      this.replies.delete(id)
      reject(new Error(`The carrier did not answer ${request.type}`))
    }, timeoutMs)
    timer.unref()
    this.replies.set(id, {
      resolve: (reply) => { clearTimeout(timer); resolve(reply) },
      reject: (error) => { clearTimeout(timer); reject(error) },
    })
    this.send({ ...request, id })
    return promise
  }

  /**
   * Serve carrier commands, including those that arrived before.
   * @param handlers Command handlers.
   * @param stopOnClose Stop the Host when the channel closes (off for detached development runs).
   */
  serve(handlers: ControlHandlers, stopOnClose: boolean): void {
    this.handler = (command) => { this.dispatch(handlers, command) }
    for (const command of this.early.splice(0)) this.dispatch(handlers, command)
    if (!stopOnClose) return
    if (this.closed) void handlers.stop()
    else this.closeListeners.add(() => { void handlers.stop() })
  }

  private dispatch(handlers: ControlHandlers, command: Command): void {
    if (command.type === 'stop') { void handlers.stop(); return }
    const id = command.id
    if (typeof id !== 'string') return
    if (command.type === 'inspect-activity') {
      const mode = command.mode === 'freeze' || command.mode === 'resume' ? command.mode : 'observe'
      this.send({ type: 'activity', id, active: handlers.inspectActivity(mode) })
    } else if (command.type === 'inspect-project') {
      const workspaceId = typeof command.workspaceId === 'string' ? command.workspaceId : undefined
      handlers.inspectProject(workspaceId).then(
        (project) => { this.send({ type: 'project', id, project }) },
        (error: unknown) => {
          const code = error instanceof Error && error.message === 'workspace-unavailable' ? 'workspace-unavailable' : 'project-unavailable'
          this.send({ type: 'project', id, error: code })
        },
      )
    }
  }
}
