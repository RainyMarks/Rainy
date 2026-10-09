/** Browser validation and interpreter selection for persisted human run/debug choices. */
import { z } from 'zod'
import type { IdeExecutionConfiguration, IdeRunConfiguration } from '../ide-execution-protocol.ts'
import type { IdeRootId } from '../ide-files-protocol.ts'

/** Named run choices accepted from the Host's versioned workspace state. */
export const executionRunConfigurationSchema: z.ZodType<IdeRunConfiguration> = z.object({
  name: z.string().min(1), language: z.enum(['python', 'javascript', 'typescript', 'php', 'c', 'cpp']), program: z.string().min(1),
  rootId: z.string().transform(value => value as IdeRootId).optional(),
  cwd: z.string().optional(), arguments: z.array(z.string()).optional(), environment: z.record(z.string(), z.string()).optional(),
  executable: z.string().optional(), pythonModule: z.string().optional(), terminal: z.boolean().optional(),
  build: z.discriminatedUnion('kind', [
    z.object({ kind: z.literal('single-file'), flags: z.array(z.string()).optional() }),
    z.object({ kind: z.literal('cmake'), buildDirectory: z.string(), target: z.string(), executable: z.string(),
      configurePreset: z.string().optional(), buildPreset: z.string().optional() }),
  ]).optional(),
})

/** Complete retained run configurations, line breakpoints and user watch expressions. */
export const executionConfigurationSchema: z.ZodType<IdeExecutionConfiguration> = z.object({
  profiles: z.array(executionRunConfigurationSchema), activeProfile: z.string().nullable(),
  breakpoints: z.array(z.object({ path: z.string(), rootId: z.string().transform(value => value as IdeRootId).optional(),
    lines: z.array(z.number().int().positive()) })), watches: z.array(z.string()),
})

/**
 * Select the Python interpreter to send to the workspace language client.
 * @param configuration - current saved workspace run choices.
 * @returns the active Python profile's interpreter, or the first configured Python interpreter when another language is active.
 */
export function selectedPythonExecutable(configuration: IdeExecutionConfiguration | undefined): string | undefined {
  if (configuration === undefined) return undefined
  const active = configuration.profiles.find(profile => profile.name === configuration.activeProfile && profile.language === 'python')
  return active === undefined
    ? configuration.profiles.find(profile => profile.language === 'python' && profile.executable !== undefined)?.executable
    : active.executable
}
