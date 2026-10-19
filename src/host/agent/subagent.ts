/**
 * One run of a `task` sub-agent: its own transcript, read-only tools and limits.
 *
 * The chat keeps only the report, so a long search costs the conversation one tool result instead of every file read.
 */
import { Agent, type AgentEvent, type AgentTool } from '@earendil-works/pi-agent-core'
import { createInitialSystemMessage, toToolDeclaration, type AssistantMessage } from '@earendil-works/pi-ai'
import type { TaskProgress } from './tools/task.ts'
import type { Models, ResolvedModel } from './models.ts'
import { usageOf } from './llm.ts'
import type { UsageSummary } from '../../shared/rpc.ts'

/** Assistant turns one sub-agent may take before it must answer. */
export const MAX_SUBAGENT_STEPS = 24
/** How long one sub-agent may run. */
export const MAX_SUBAGENT_MS = 5 * 60_000
/** Tool calls kept in the progress list shown on the card. */
const ACTIVITY_KEPT = 12

/** What one sub-agent run needs. */
export interface SubagentInput {
  prompt: string
  description: string
  resolved: ResolvedModel
  tools: AgentTool[]
  /** Working directory and platform, for the sub-agent's own system prompt. */
  cwd: string
  platform: 'win32' | 'linux'
  streamFn: Models['streamFn']
  onProgress: (progress: TaskProgress) => void
  /** Counts the sub-agent's model usage into the chat's run. */
  onUsage?: ((usage: UsageSummary) => void) | undefined
  signal?: AbortSignal | undefined
  /** Time limit in place of the default, for tests. */
  maxMs?: number | undefined
}

/**
 * The sub-agent's system prompt. It never sees the conversation, so it is told to answer in full.
 * @param cwd Working directory.
 * @param platform Host platform.
 * @returns The prompt.
 */
export function subagentPrompt(cwd: string, platform: 'win32' | 'linux'): string {
  return 'You are a sub-agent of Rainy, a coding assistant. You are given one self-contained task by the main assistant, which cannot see your work. '
    + 'Investigate with the read, grep and glob tools, and web search when you have it. You cannot change files, run commands or ask questions. '
    + 'Search before reading: grep and glob narrow the work, and read targeted line ranges rather than whole files. '
    + 'Answer with one report that stands on its own: what you found, the file paths and line numbers that matter, and what you could not determine. '
    + 'Quote only the lines that carry the answer. Do not describe your process or ask for confirmation.'
    + `\n\nWorking directory: ${cwd} (${platform === 'win32' ? 'Windows' : 'Linux'}).`
}

/** @param message An assistant message. @returns Its text blocks joined. */
function assistantText(message: AssistantMessage): string {
  return typeof message.content === 'string'
    ? message.content
    : message.content.flatMap(block => block.type === 'text' ? [block.text] : []).join('\n')
}

/**
 * Run one sub-agent to completion.
 * @param input The task, model, tools and callbacks.
 * @returns Its report and what it did.
 */
export async function runSubagent(input: SubagentInput): Promise<{ text: string; steps: number; activity: string[]; stopped?: 'steps' | 'time' | undefined }> {
  const limit = new AbortController()
  let stopped: 'steps' | 'time' | undefined
  const timer = setTimeout(() => { stopped = 'time'; limit.abort() }, input.maxMs ?? MAX_SUBAGENT_MS)
  const onParentAbort = (): void => { limit.abort() }
  input.signal?.addEventListener('abort', onParentAbort, { once: true })
  const declarations = input.tools.map(tool => toToolDeclaration(tool))
  const initial = createInitialSystemMessage(subagentPrompt(input.cwd, input.platform), declarations)
  // The limit also ends a model request in flight, not only the next tool call.
  const streamFn: Models['streamFn'] = (model, context, options) => input.streamFn(model, context, {
    ...options, signal: options?.signal === undefined ? limit.signal : AbortSignal.any([options.signal, limit.signal]),
  })
  const agent = new Agent({
    initialState: {
      model: input.resolved.model,
      thinkingLevel: input.resolved.thinking ?? 'off',
      tools: input.tools,
      messages: initial === undefined ? [] : [initial],
    },
    streamFn,
    toolExecution: 'parallel',
    beforeToolCall: () => Promise.resolve(limit.signal.aborted ? { block: true, reason: 'The sub-agent was stopped.', terminate: true } : undefined),
  })
  const activity: string[] = []
  let steps = 0
  let text = ''
  let partial = ''
  agent.subscribe((event: AgentEvent) => {
    if (event.type === 'tool_execution_start') {
      activity.push(event.toolName)
      if (activity.length > ACTIVITY_KEPT) activity.shift()
      input.onProgress({ activity: [...activity], steps, text: text || partial })
    } else if (event.type === 'message_update' && event.message.role === 'assistant') {
      partial = assistantText(event.message as AssistantMessage)
      input.onProgress({ activity: [...activity], steps, text: partial })
    } else if (event.type === 'message_end' && event.message.role === 'assistant') {
      const message = event.message as AssistantMessage
      steps++
      const said = assistantText(message)
      if (said.trim() !== '') text = said
      input.onUsage?.(usageOf(message))
      if (steps >= MAX_SUBAGENT_STEPS) { stopped = 'steps'; limit.abort() }
      input.onProgress({ activity: [...activity], steps, text })
    }
  })
  try {
    await agent.prompt([{ role: 'user', content: input.prompt, timestamp: Date.now() }])
  } catch (error) {
    if (!limit.signal.aborted) throw error
  } finally {
    clearTimeout(timer)
    input.signal?.removeEventListener('abort', onParentAbort)
    agent.abort()
  }
  if (input.signal?.aborted === true) throw new Error('tool call aborted')
  return { text, steps, activity, ...(stopped === undefined ? {} : { stopped }) }
}
