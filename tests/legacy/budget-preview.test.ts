import { mkdtemp, mkdir, rm, writeFile } from 'node:fs/promises'
import { join } from 'node:path'
import { tmpdir } from 'node:os'
import { Context } from '@deepseek-ai/cordis'
import { WorkspaceId } from '@deepseek-ai/dsh-workspace'
import type { LlmResolvedModelInfo } from '@deepseek-ai/dsh-llm'
import { describe, expect, it, onTestFinished } from 'vitest'
import { mountAgentLoopTestDependencies } from '../../../packages/test-support/agent-loop-testkit/src/index.ts'
import { MockAdapter } from '../../../packages/core/agent-loop/tests/mock-adapter.ts'
import { previewBudget } from '../src/budget-preview.ts'
import { estimateText } from '../src/budget.ts'
import { GLOBAL_PROMPT_ORDER, GLOBAL_PROMPT_SECTION } from '../src/global-prompt.ts'

class PreviewAdapter extends MockAdapter {
  constructor(private readonly window: number) {
    super([])
  }
  override resolveModel(provider: string, model: string): Promise<LlmResolvedModelInfo> {
    return Promise.resolve({
      provider,
      id: model,
      name: model,
      context: { contextWindow: this.window },
      defaultMaxTokens: 512,
    })
  }
}

describe('first-send budget preview', () => {
  it.each([4096, 8192, 32768, 65536])(
    'counts project inputs in a %i-token window without creating a chat or calling a model',
    async (window) => {
      const root = await mkdtemp(join(tmpdir(), 'rainy-preview-'))
      onTestFinished(() => rm(root, { recursive: true, force: true }))
      await mkdir(join(root, '.git'))
      await writeFile(join(root, 'AGENTS.md'), '本项目保留中文注释，修改后需要检查。')
      const ctx = new Context()
      onTestFinished(() => ctx.fiber.dispose())
      await mountAgentLoopTestDependencies(ctx, {
        systemPrompt: { includeHarnessIdentity: false, includeRuntimeContext: false },
      })
      const adapter = new PreviewAdapter(window)
      ctx.llm.registerAdapter(['mock'], adapter)
      const workspace = { id: WorkspaceId('preview-workspace'), path: root, title: 'Preview' }
      ctx.provide('workspaceRegistry', { get: (id: string) => (id === workspace.id ? workspace : undefined) } as never)
      ctx.provide('agentDefaultModel', { currentSelection: () => ({ provider: 'mock', model: 'model' }) } as never)
      ctx.provide('configEditor', {
        entries: () => [
          {
            options: {
              id: 'agent-instructions',
              config: {
                maxBytes: 65536,
                maxSourceBytes: 65536,
                budgetOverflow: 'error',
                dshHome: join(root, 'home'),
              },
            },
          },
        ],
      } as never)
      ctx.provide('rainyProjectRoots', { forSessionCwd: () => [] } as never)
      ctx.provide('rainyMemory', { previewRecall: async () => 'Historical project note: use UTF-8.' } as never)
      ctx.systemPrompt.section({ name: 'preview-persona', order: 0, text: 'Work in {{cwd}}.' })
      ctx.systemPrompt.section({
        name: 'rainy-skill:fixture',
        order: 1,
        text: 'Selected skill details.',
        interpolate: false,
      })
      ctx.systemPrompt.tools(() => ({
        schemas: [{ name: 'read', description: 'Read a file.', parameters: { type: 'object', properties: {} } }],
      }))
      let created = 0
      ctx.on('session/created', () => {
        created++
      })
      const preview = { run: (query: unknown) => previewBudget(ctx, query) }
      await ctx.plugin({
        name: 'budget-preview-owner',
        inject: [
          'workspaceRegistry',
          'agentDefaultModel',
          'agents',
          'llm',
          'systemPrompt',
          'configEditor',
          'rainyProjectRoots',
        ],
        apply(owner) {
          preview.run = query => previewBudget(owner, query)
        },
      })
      const before = await preview.run({ workspaceId: workspace.id })
      const draft = await preview.run({ workspaceId: workspace.id, draft: '请检查当前项目的配置。' })
      expect(before).toMatchObject({ preview: true, kind: 'estimated', contextWindow: window, outputTokens: 512 })
      expect(before.breakdown?.instructions).toBeGreaterThan(0)
      expect(before.breakdown?.memory).toBeGreaterThan(0)
      expect(before.breakdown?.extensions).toBeGreaterThan(0)
      expect(before.breakdown?.tools).toBeGreaterThan(0)
      expect(draft.tokens).toBeGreaterThan(before.tokens)
      const globalPrompt = 'Answer in Chinese.'
      const withdraw = ctx.systemPrompt.section({
        name: GLOBAL_PROMPT_SECTION, order: GLOBAL_PROMPT_ORDER, text: globalPrompt, interpolate: false,
      })
      const prompted = await preview.run({ workspaceId: workspace.id })
      withdraw()
      expect((prompted.breakdown?.instructions ?? 0) - (before.breakdown?.instructions ?? 0)).toBe(estimateText(globalPrompt))
      expect(prompted.tokens).toBeGreaterThan(before.tokens)
      expect(created).toBe(0)
      expect(ctx.agents.list()).toEqual([])
      expect(adapter.requests).toEqual([])
      await writeFile(join(root, 'AGENTS.md'), 'mandatory '.repeat(7000))
      await expect(preview.run({ workspaceId: workspace.id })).rejects.toThrow('AGENTS.md')
    },
  )
})
