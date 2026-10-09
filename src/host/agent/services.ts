/** Host services shared by every chat. */
import type { Activity } from '../activity.ts'
import type { HostEnvironment } from '../env.ts'
import type { Projects } from '../projects.ts'
import type { RpcHub } from '../rpc.ts'
import type { RuntimeService } from '../runtime/index.ts'
import type { Settings } from '../settings.ts'
import type { McpManager } from './mcp.ts'
import type { ProjectMemory } from './memory/index.ts'
import type { Models } from './models.ts'
import type { ChatStore } from './store.ts'

/** What a chat session uses from the rest of the Host. */
export interface AgentServices {
  env: HostEnvironment
  settings: Settings
  models: Models
  store: ChatStore
  mcp: McpManager
  memory: ProjectMemory
  projects: Projects
  runtime: RuntimeService
  activity: Activity
  rpc: RpcHub
  log(message: string): void
  /** A chat model request started streaming. */
  modelBusy(): void
  /** A chat run finished; project memory waits for a quiet period after this. */
  modelIdle(): void
}
