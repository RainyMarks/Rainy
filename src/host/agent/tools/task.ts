/**
 * `task`: a sub-agent that investigates on its own and reports back in one answer.
 *
 * It keeps a long search out of the chat's own context: the chat sees the report, not every file the sub-agent read.
 * A sub-agent only reads — files, searches and web search — so no tool call of its own ever waits for the user.
 */
import { Type, type Static } from '@earendil-works/pi-ai'
import type { AgentTool, AgentToolUpdateCallback } from '@earendil-works/pi-agent-core'
import { limitOutput, textResult, type ToolContext } from './common.ts'

/** Sub-agents of one chat that may run at the same time; the model is told to stay within this. */
export const MAX_PARALLEL_TASKS = 4

/** Progress of a running sub-agent, for the chat's tool card. */
export interface TaskProgress {
  /** Tool calls it has made, newest last. */
  activity: string[]
  steps: number
  /** Its answer so far, while it is writing one. */
  text: string
}

/** What `task` needs from the chat to run a sub-agent. */
export interface SubagentRunner {
  /**
   * Run one sub-agent to completion.
   * @param input The task, a short label, a progress callback and the chat's abort signal.
   * @returns Its report and what it did.
   */
  run(input: {
    prompt: string
    description: string
    onProgress: (progress: TaskProgress) => void
    signal?: AbortSignal | undefined
  }): Promise<{ text: string; steps: number; activity: string[]; stopped?: 'steps' | 'time' | undefined }>
}

const parameters = Type.Object({
  description: Type.String({ description: 'What this sub-agent is for, 3-6 words, shown in the interface. Example: "Find the login flow".' }),
  prompt: Type.String({
    description: 'The task, written for an assistant that cannot see this conversation: what to look for, where to start, '
      + 'and exactly what to report back. The sub-agent reads files, searches the project and searches the web; it cannot change anything.',
  }),
})

/** What the card shows for a sub-agent. */
export interface TaskDetails {
  description: string
  steps: number
  activity: string[]
  stopped?: 'steps' | 'time' | undefined
}

/**
 * The sub-agent tool.
 * @param context Chat tool context, for the output limit.
 * @param runner Runs one sub-agent for this chat.
 * @returns Tool definition.
 */
export function taskTool(context: ToolContext, runner: SubagentRunner): AgentTool<typeof parameters, TaskDetails> {
  return {
    name: 'task',
    label: 'Sub-agent',
    description: 'Hand a self-contained investigation to a sub-agent that reads files, searches the project and searches the web, '
      + 'then answers in one report. Use it when finding the answer would otherwise fill this conversation with searching: '
      + 'locating where something is implemented, comparing several candidates, or reading a large area of the project. '
      + `Several sub-agents (at most ${MAX_PARALLEL_TASKS}) run at the same time when called in one step. `
      + 'A sub-agent cannot change files, run commands or ask the user, and it sees nothing of this conversation beyond the prompt you give it, '
      + 'so state the task in full and say exactly what to report.',
    parameters,
    executionMode: 'parallel',
    async execute(_id, params: Static<typeof parameters>, signal, onUpdate?: AgentToolUpdateCallback<TaskDetails>) {
      if (params.prompt.trim() === '') throw new Error('invalid prompt: expected the task for the sub-agent')
      if (params.description.trim() === '') throw new Error('invalid description: expected a short label')
      const report = await runner.run({
        prompt: params.prompt,
        description: params.description,
        signal,
        onProgress: (progress) => {
          onUpdate?.({
            content: [{ type: 'text', text: progress.text }],
            details: { description: params.description, steps: progress.steps, activity: progress.activity },
          })
        },
      })
      const note = report.stopped === 'steps' ? '\n\n[the sub-agent reached its step limit; its report may be incomplete]'
        : report.stopped === 'time' ? '\n\n[the sub-agent reached its time limit; its report may be incomplete]' : ''
      const text = report.text.trim() === '' ? '(the sub-agent reported nothing)' : report.text
      return textResult(await limitOutput(context, 'task', text + note), {
        description: params.description, steps: report.steps, activity: report.activity, ...(report.stopped === undefined ? {} : { stopped: report.stopped }),
      })
    },
  }
}
