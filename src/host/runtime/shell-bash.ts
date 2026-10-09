/** Rainy Bash provider applying workspace environment choices per resolved command. */
import { SandboxBashExecutor } from '@deepseek-ai/dsh-bash-sandbox'
import type { ShellExecRequest, ShellExecSpec } from '@deepseek-ai/dsh-shell'
import type {} from './runtime.ts'

/** Native Bash execution with explicit workspace-local environment layering. */
export default class RainyBashExecutor extends SandboxBashExecutor {
  static override inject = [...SandboxBashExecutor.inject, 'rainyRuntime']
  /** @param request - command and session-derived cwd. @returns the existing sandbox specification with selected interpreter paths. */
  override resolve(request: ShellExecRequest): ShellExecSpec {
    const spec = super.resolve(request)
    return { ...spec, env: {
      ...this.ctx.rainyRuntime.resolveDirectory(request.sandboxPolicy?.workspaceRoot ?? spec.workdir).environment,
      ...spec.env,
    } }
  }
}
