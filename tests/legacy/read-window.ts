/** Read-window and final-request admission checks through an already booted named Rainy profile. */
import assert from 'node:assert/strict'
import { mkdir, readFile, readdir, writeFile } from 'node:fs/promises'
import { join } from 'node:path'
import type { Context } from '@deepseek-ai/cordis'
import type { Agent } from '@deepseek-ai/dsh-agent'
import { createMessage, createToolResultMessage, createUserMessage, ToolCallId } from '@deepseek-ai/dsh-llm'
import type { ContentBlock } from '@deepseek-ai/dsh-llm'
import type {} from '@deepseek-ai/dsh-tools'
import type {} from '@deepseek-ai/dsh-spill'
import { estimateRequest, resolveBudget } from '../src/budget.ts'

/**
 * Verify reader-owned paging and complete-request refusal without rewriting tool data.
 * @param ctx Named profile's real tool, storage and model-policy services.
 * @param agent Owner of the test calls and request schemas.
 * @param project Exclusively owned test project.
 * @param spillRoot Explicit root configured on this profile's local spill backend.
 * @param providerRequests Number of requests received by the deterministic HTTP provider.
 * @returns Measured paging and admission facts.
 */
export async function verifyReadWindow(
  ctx: Context, agent: Agent, project: string, spillRoot: string, providerRequests: () => number,
): Promise<Record<string, unknown>> {
  await mkdir(spillRoot, { recursive: true })
  const files = async () => (await readdir(spillRoot, { recursive: true, withFileTypes: true })).filter(item => item.isFile()).length
  const original = new Map<ToolCallId, { arguments: unknown; content: ContentBlock[]; value: unknown; meta: unknown }>()
  const stop = ctx.on('tools/post-execute', (exec, result, next) => {
    if (exec.callId.startsWith('read-window-')) original.set(exec.callId, {
      arguments: structuredClone(exec.arguments), content: structuredClone(result.content),
      value: result.isError ? undefined : structuredClone(result.value), meta: structuredClone(result.meta),
    })
    return next()
  })
  let call = 0
  const read = async (args: { file_path: string; offset?: number; limit?: number }) => {
    const callId = ToolCallId(`read-window-${++call}`)
    const beforeFiles = await files()
    const result = await ctx.tools.execute({ agent, name: 'read', arguments: args, callId, signal: new AbortController().signal })
    assert(!result.isError)
    const initial = original.get(callId)
    assert(initial)
    assert.deepEqual(initial.arguments, args)
    assert.deepEqual(result.content, initial.content)
    assert.deepEqual(result.value, initial.value)
    assert.deepEqual(result.meta, initial.meta)
    assert.equal(await files(), beforeFiles, 'Reading an ordinary file or a spill must never create another spill')
    return result
  }
  try {
    const normal = join(project, 'read-window-normal.txt')
    const lines = Array.from({ length: 8 }, (_, index) => `line-${index + 1} ${'data '.repeat(45)}`)
    await writeFile(normal, lines.join('\n'))
    const plain = await read({ file_path: normal, offset: 2, limit: 3 })
    assert.deepEqual(plain.value, { path: normal, offset: 2,
      lines: lines.slice(1, 4).map((text, index) => ({ number: index + 2, text })), totalLines: 8 })
    const saved = await ctx.spillStore.saveText({ owner: { sessionId: agent.id },
      source: { kind: 'tool', toolName: 'read-window-fixture', callId: ToolCallId('read-window-origin'), label: 'result' },
      suggestedName: 'read-window.txt', content: lines.join('\n') })
    const first = await read({ file_path: saved.locator, offset: 1, limit: 3 })
    const second = await read({ file_path: saved.locator, offset: 4, limit: 3 })
    assert.deepEqual(first.value, { path: saved.locator, offset: 1,
      lines: lines.slice(0, 3).map((text, index) => ({ number: index + 1, text })), totalLines: 8 })
    assert.deepEqual(second.value, { path: saved.locator, offset: 4,
      lines: lines.slice(3, 6).map((text, index) => ({ number: index + 4, text })), totalLines: 8 })

    const longPath = join(project, 'read-window-long-line.txt')
    const longText = '字'.repeat(2400)
    await writeFile(longPath, longText)
    const long = await read({ file_path: longPath, offset: 1, limit: 1 })
    assert(JSON.stringify(long.content).includes('line truncated to 2000 chars'))
    assert.equal(await readFile(longPath, 'utf8'), longText, 'Reader truncation must leave the original file untouched')

    const largePath = join(project, 'read-window-large.txt')
    await writeFile(largePath, Array.from({ length: 200 }, (_, index) => `row-${index} ${'x'.repeat(500)}`).join('\n'))
    const large = await read({ file_path: largePath })
    assert(JSON.stringify(large.content).includes('Output capped.'))
    const callId = ToolCallId('read-window-budget')
    const request = {
      provider: 'read-window-boundary', model: 'four-k', maxTokens: resolveBudget(4096).outputTokens,
      sessionId: agent.id, signal: new AbortController().signal, tools: agent.session.requestHeader()?.tools,
      messages: [createUserMessage({ source: { kind: 'user' }, content: [{ type: 'text', text: 'Inspect the bounded file window.' }] }),
        createMessage({ role: 'assistant', source: { kind: 'model', provider: 'read-window-boundary', model: 'four-k' },
          content: [{ type: 'tool-call', id: callId, name: 'read', arguments: JSON.stringify({ file_path: largePath }) }] }),
        createToolResultMessage({ callId, content: large.content, isError: false })],
    }
    const tokens = estimateRequest(request)
    assert(tokens > resolveBudget(4096).inputLimit)
    const beforeDispatch = providerRequests()
    let rejection: { code: string; message: string } | undefined
    for await (const chunk of ctx.llm.stream(request)) {
      if (chunk.type === 'finish' && chunk.reason.kind === 'error') rejection = chunk.reason.failure
    }
    assert.equal(providerRequests(), beforeDispatch, 'A complete oversized request must never reach the provider')
    assert.equal(rejection?.code, 'CONTEXT_WINDOW_EXCEEDED')
    assert(rejection.message.includes('输入预算'))
    return { calls: call, argsValueMetaPreserved: true, recursiveSpillsCreated: 0, ordinaryPaging: 'passed', spillPaging: 'passed',
      longLineMarker: 'passed', originalLongLineUnchanged: true, configuredReadByteCap: 51200,
      oversizedRequestEstimatedTokens: tokens, boundaryInputLimit: resolveBudget(4096).inputLimit, oversizedProviderDispatches: 0,
      rejectionCode: rejection.code, limitation: 'Line truncation is explicit; offset/limit does not page columns of a single long line.' }
  } finally {
    stop()
  }
}
