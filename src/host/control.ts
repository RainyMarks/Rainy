/**
 * stdio channel to the Electron carrier.
 *
 * stdout lines `RAINY_CONTROL {json}`: `ready`, `fatal`, `activity`, `project`.
 * stdin JSON lines: `stop`, `inspect-activity {id, mode?}`, `inspect-project {id, workspaceId?}`.
 * All other Host output goes to stderr.
 */
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

/**
 * Write one control message to stdout.
 * @param message JSON-serializable message with a `type`.
 */
export function sendControl(message: { type: string } & Record<string, unknown>): void {
  process.stdout.write(`RAINY_CONTROL ${JSON.stringify(message)}\n`)
}

/**
 * Serve carrier requests from stdin until it closes.
 * @param handlers Request handlers.
 * @param stopOnClose Stop the Host when stdin closes (off for detached development runs).
 */
export function serveControl(handlers: ControlHandlers, stopOnClose: boolean): void {
  const input = createInterface({ input: process.stdin, crlfDelay: Infinity })
  input.on('line', (line) => {
    let command: Record<string, unknown>
    try {
      const parsed: unknown = JSON.parse(line)
      if (parsed === null || typeof parsed !== 'object') return
      command = parsed as Record<string, unknown>
    } catch (_error) {
      console.error('Invalid control message')
      return
    }
    if (command.type === 'stop') { input.close(); void handlers.stop(); return }
    const id = command.id
    if (typeof id !== 'string') return
    if (command.type === 'inspect-activity') {
      const mode = command.mode === 'freeze' || command.mode === 'resume' ? command.mode : 'observe'
      sendControl({ type: 'activity', id, active: handlers.inspectActivity(mode) })
    } else if (command.type === 'inspect-project') {
      const workspaceId = typeof command.workspaceId === 'string' ? command.workspaceId : undefined
      handlers.inspectProject(workspaceId).then(
        (project) => { sendControl({ type: 'project', id, project }) },
        (error: unknown) => {
          const code = error instanceof Error && error.message === 'workspace-unavailable' ? 'workspace-unavailable' : 'project-unavailable'
          sendControl({ type: 'project', id, error: code })
        },
      )
    }
  })
  if (stopOnClose) input.on('close', () => { void handlers.stop() })
}
