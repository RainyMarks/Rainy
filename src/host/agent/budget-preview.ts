/** Read-only estimates for a prospective request, without creating a Session. */
import type { Context } from '@deepseek-ai/cordis'
import { assembleContextFor } from '@deepseek-ai/dsh-agent'
import { loadBaselineInstructions } from '@deepseek-ai/dsh-agent-instructions'
import { SessionId } from '@deepseek-ai/dsh-session'
import { renderPrompt } from '@deepseek-ai/dsh-system-prompt'
import { WorkspaceId } from '@deepseek-ai/dsh-workspace'
import { z } from 'zod'
import { estimateRequest, promptBreakdown, resolveBudget } from './budget.ts'
import type { CountableRequest } from './budget.ts'
import { GLOBAL_PROMPT_SECTION } from './global-prompt.ts'
import type { BudgetSnapshot } from './policy.ts'
import type {} from './project-memory.ts'
import type {} from './project-roots.ts'
import type {} from '@deepseek-ai/dsh-agent-default-model'
import type {} from '@deepseek-ai/dsh-config-editor'

/** Stable client-localized explanations for the estimate's scope. */
export type BudgetPreviewLimitation = 'before-dispatch-estimate' | 'attachments-not-included'
/** A preview never replaces the last admitted request or its reported usage. */
export interface BudgetPreview extends BudgetSnapshot {
  preview: true
  limitations: BudgetPreviewLimitation[]
}

const querySchema = z
  .object({
    workspaceId: z.string().min(1).max(160),
    sessionId: z.string().min(1).max(160).optional(),
    provider: z.string().min(1).max(160).optional(),
    model: z.string().min(1).max(256).optional(),
    draft: z.string().max(60000).optional(),
  })
  .refine(query => (query.provider === undefined) === (query.model === undefined), '模型和供应商必须一起指定。')
const instructionSchema = z.object({
  maxBytes: z.number(),
  maxSourceBytes: z.number().optional(),
  dshHome: z.string().optional(),
  budgetOverflow: z.enum(['truncate', 'error']).optional(),
  projectRootMarkers: z.array(z.string()).optional(),
  instructionFileCandidates: z.array(z.string()).optional(),
  localInstructionFileCandidates: z.array(z.string()).optional(),
})

/**
 * Estimate the selected project's prompt, schemas, instructions, recall, history and optional draft.
 * @param ctx Live Host services; prompt assembly does not start model inference or extension processes.
 * @param raw Authenticated control request.
 * @returns An explicitly estimated snapshot; no chat, inbox item, or session event is created.
 */
export async function previewBudget(ctx: Context, raw: unknown): Promise<BudgetPreview> {
  const query = querySchema.parse(raw)
  const workspace = ctx.workspaceRegistry.get(WorkspaceId(query.workspaceId))
  if (!workspace) throw new Error('请先打开项目文件夹。')
  const agent = query.sessionId === undefined ? undefined : ctx.agents.get(SessionId(query.sessionId))
  if (query.sessionId !== undefined && !agent) throw new Error('会话尚未加载，无法预览其历史预算。')
  if (agent && agent.session.header.cwd !== workspace.path) throw new Error('预算会话不属于当前项目。')
  const route =
    query.provider && query.model
      ? { provider: query.provider, model: query.model }
      : (agent?.session.requestHeader()?.config ?? ctx.agentDefaultModel.currentSelection())
  if (!route.provider || !route.model) throw new Error('请先选择模型。')
  const info = await ctx.llm.resolveModelInfo(route.provider, route.model)
  const budget = resolveBudget(info.context?.contextWindow ?? 32768, info.defaultMaxTokens)
  const assembly = await ctx.systemPrompt.assemble(agent ? assembleContextFor(agent) : {})
  if (!agent) assembly.variables = { ...assembly.variables, cwd: workspace.path }
  let system = renderPrompt(assembly)
  if (!agent) {
    const roots = ctx.rainyProjectRoots
      .forSessionCwd(workspace.path)
      .filter(root => !root.primary)
      .map(root => root.path)
    if (roots.length > 0)
      system += `\n\nAdditional project directories: ${JSON.stringify(roots)}. Use their absolute paths; the session working directory stays unchanged.`
  }
  const messages: Array<CountableRequest['messages'][number]> = agent
    ? [
      ...agent.session.deriveMessages().filter(message => message.role !== 'system'),
      ...agent.inbox.nextStep,
      ...agent.inbox.nextTurn.slice(0, 1),
    ]
    : []
  if (!messages.some(message => message.source?.kind === 'agent-instructions')) {
    const entry = ctx.configEditor.entries().find(candidate => candidate.options.id === 'agent-instructions')
    if (!entry) throw new Error('项目指令配置不存在。')
    const instructions = await loadBaselineInstructions({
      cwd: workspace.path,
      ...instructionSchema.parse(entry.options.config),
    })
    if (instructions?.text)
      messages.push({
        role: 'user',
        source: { kind: 'agent-instructions' },
        content: [{ type: 'text', text: instructions.text }],
      })
  }
  const memory = ctx.get('rainyMemory')
  if (!memory) throw new Error('项目记忆服务尚未就绪，请稍后重试。')
  const memoryIds = agent ? [...memory.recallMessageIds(agent.id)] : []
  if (!agent || agent.session.requestHeader() === undefined) {
    const recall = await memory.previewRecall(workspace.id, budget.inputLimit)
    if (recall && !messages.some(message => message.id !== undefined && memoryIds.includes(message.id))) {
      memoryIds.push('preview-project-memory')
      messages.push({
        id: 'preview-project-memory',
        role: 'user',
        source: { kind: 'session-reference' },
        content: [{ type: 'text', text: recall }],
      })
    }
  }
  if (query.draft)
    messages.push({ role: 'user', source: { kind: 'user' }, content: [{ type: 'text', text: query.draft }] })
  const request: CountableRequest = {
    provider: route.provider,
    model: route.model,
    system,
    messages,
    tools: assembly.tools,
  }
  const tokens = estimateRequest(request)
  const descriptions = assembly.sections
    .filter(section => section.name.startsWith('rainy-skill:') || section.name.startsWith('mcp:'))
    .map(section => section.text)
  const instructions = assembly.sections.filter(section => section.name === GLOBAL_PROMPT_SECTION).map(section => section.text)
  return {
    ...budget,
    sessionId: agent?.id ?? `preview:${workspace.id}`,
    provider: route.provider,
    model: route.model,
    tokens,
    kind: 'estimated',
    method: 'prospective-request-estimate',
    compacting: false,
    preview: true,
    limitations: ['before-dispatch-estimate', 'attachments-not-included'],
    breakdown: promptBreakdown(request, descriptions, memoryIds, instructions),
    ...(tokens > budget.inputLimit
      ? { error: `预计输入 ${tokens} tokens 超过 ${budget.inputLimit} 的输入预算。` }
      : {}),
  }
}
