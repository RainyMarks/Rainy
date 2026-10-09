/** Rainy PowerShell provider applying workspace environment choices per resolved command. */
import { SandboxPwshExecutor } from '@deepseek-ai/dsh-pwsh-sandbox'
import type { ShellExecRequest, ShellExecSpec } from '@deepseek-ai/dsh-shell'
import type {} from './runtime.ts'

/** Native PowerShell execution with explicit workspace-local environment layering. */
export default class RainyPwshExecutor extends SandboxPwshExecutor {
  static override inject = [...SandboxPwshExecutor.inject, 'rainyRuntime']
  /** @param request - command and session-derived cwd. @returns the existing sandbox specification with selected interpreter paths. */
  override resolve(request: ShellExecRequest): ShellExecSpec {
    const spec = super.resolve(request)
    return { ...spec, env: {
      ...this.ctx.rainyRuntime.resolveDirectory(request.sandboxPolicy?.workspaceRoot ?? spec.workdir).environment,
      ...spec.env,
    } }
  }
}
