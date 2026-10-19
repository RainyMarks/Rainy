/** The built-in model tools of a chat. */
import type { AgentTool } from '@earendil-works/pi-agent-core'
import { Observations, type ToolContext } from './common.ts'
import { BackgroundJobs } from './jobs.ts'
import { readTool } from './read.ts'
import { globTool, grepTool } from './search.ts'
import { commandOutputTool, shellTool, stopCommandTool } from './shell.ts'
import { taskTool, type SubagentRunner } from './task.ts'
import { webFetchTool, webSearchTool, type WebAccess } from './web.ts'
import { editTool, writeTool } from './write.ts'

/**
 * Built-in tools: read, grep, glob, write, edit, the platform shell, and web_search and web_fetch when given web access.
 * @param context Chat tool context.
 * @param platform Host platform; Windows gets `pwsh`, Linux/WSL gets `bash`.
 * @param web DeepSeek web access; absent when web search is off or no DeepSeek key exists.
 * @param subagents Runs a sub-agent for this chat; absent outside a chat, where `task` is not offered.
 * @returns The tools.
 */
export function coreTools(context: ToolContext, platform: 'win32' | 'linux', web?: WebAccess, subagents?: SubagentRunner): AgentTool[] {
  return [
    readTool(context), grepTool(context), globTool(context), writeTool(context), editTool(context), shellTool(context, platform === 'win32' ? 'pwsh' : 'bash'),
    ...context.jobs === undefined ? [] : [commandOutputTool(context), stopCommandTool(context)],
    ...web === undefined ? [] : [webSearchTool(web), webFetchTool(web, context)],
    ...subagents === undefined ? [] : [taskTool(context, subagents)],
  ] as AgentTool[]
}

/**
 * Built-in tools for estimates outside a chat; they must not be executed.
 * @param platform Host platform.
 * @returns Tools bound to an inert context.
 */
export function inertCoreTools(platform: 'win32' | 'linux'): AgentTool[] {
  // The jobs registry starts nothing by itself; it is here so an estimate declares the same tools a chat does.
  const context: ToolContext = {
    cwd: '.', sessionId: '', spillDir: '.', toolTokens: () => 4000, observations: new Observations(), jobs: new BackgroundJobs(),
    runtimeEnvironment: () => { throw new Error('inert tool context') },
  }
  const subagents: SubagentRunner = { run: () => Promise.reject(new Error('inert tool context')) }
  return coreTools(context, platform, undefined, subagents)
}

/**
 * Tools a sub-agent may use: it reads and searches, so no call of its own ever waits for the user's approval.
 * @param context Sub-agent tool context.
 * @param web DeepSeek web access, when the chat has it.
 * @returns The tools.
 */
export function subagentTools(context: ToolContext, web?: WebAccess): AgentTool[] {
  return [
    readTool(context), grepTool(context), globTool(context),
    ...web === undefined ? [] : [webSearchTool(web)],
  ] as AgentTool[]
}
