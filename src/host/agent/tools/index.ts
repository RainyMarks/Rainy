/** The built-in model tools of a chat. */
import type { AgentTool } from '@earendil-works/pi-agent-core'
import { Observations, type ToolContext } from './common.ts'
import { readTool } from './read.ts'
import { shellTool } from './shell.ts'
import { editTool, writeTool } from './write.ts'

/**
 * Built-in tools: read, write, edit and the platform shell.
 * @param context Chat tool context.
 * @param platform Host platform; Windows gets `pwsh`, Linux/WSL gets `bash`.
 * @returns The tools.
 */
export function coreTools(context: ToolContext, platform: 'win32' | 'linux'): AgentTool[] {
  return [readTool(context), writeTool(context), editTool(context), shellTool(context, platform === 'win32' ? 'pwsh' : 'bash')] as AgentTool[]
}

/**
 * Built-in tools for estimates outside a chat; they must not be executed.
 * @param platform Host platform.
 * @returns Tools bound to an inert context.
 */
export function inertCoreTools(platform: 'win32' | 'linux'): AgentTool[] {
  const context: ToolContext = {
    cwd: '.', sessionId: '', spillDir: '.', toolTokens: () => 4000, observations: new Observations(),
    runtimeEnvironment: () => { throw new Error('inert tool context') },
  }
  return coreTools(context, platform)
}
