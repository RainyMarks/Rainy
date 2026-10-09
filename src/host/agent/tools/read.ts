/** `read`: a line-numbered window of a text file. */
import { createReadStream } from 'node:fs'
import { stat } from 'node:fs/promises'
import { Type, type Static } from '@earendil-works/pi-ai'
import type { AgentTool } from '@earendil-works/pi-agent-core'
import { resolvePath, textResult, versionOf, type ToolContext } from './common.ts'

/** Lines returned by one call. */
export const READ_MAX_LINES = 2000
/** Characters kept per line. */
export const READ_MAX_LINE_LENGTH = 2000
/** Bytes of selected output per call. */
export const READ_MAX_BYTES = 50 * 1024

const parameters = Type.Object({
  file_path: Type.String({ description: 'Path to read, resolved by the filesystem backend.' }),
  offset: Type.Optional(Type.Number({ description: '1-based first line to return. Defaults to 1.' })),
  limit: Type.Optional(Type.Number({ description: `Maximum number of lines to return. Defaults to ${READ_MAX_LINES}.` })),
})

/** Structured window for the UI's read card. */
export interface ReadDetails {
  path: string
  offset: number
  lines: { number: number; text: string }[]
  totalLines: number
}

/**
 * Read a window of lines from a stream of text chunks.
 * @param chunks UTF-8 text chunks.
 * @param offset First line (1-based).
 * @param limit Maximum lines.
 * @param displayPath Path shown in errors.
 * @returns The selected lines, the file's line count, and whether the byte cap cut the window.
 */
export async function readWindow(chunks: AsyncIterable<string>, offset: number, limit: number, displayPath: string): Promise<{ lines: ReadDetails['lines']; totalLines: number; truncatedByBytes: boolean }> {
  const lines: ReadDetails['lines'] = []
  let totalLines = 0
  let bytes = 0
  let truncatedByBytes = false
  let buffer = ''
  const consume = (raw: string): void => {
    totalLines++
    if (truncatedByBytes || totalLines < offset || lines.length >= limit) return
    const line = raw.endsWith('\r') ? raw.slice(0, -1) : raw
    const text = line.length > READ_MAX_LINE_LENGTH ? `${line.slice(0, READ_MAX_LINE_LENGTH)}... (line truncated to ${READ_MAX_LINE_LENGTH} chars)` : line
    const size = Buffer.byteLength(text, 'utf8') + (lines.length > 0 ? 1 : 0)
    if (bytes + size > READ_MAX_BYTES) { truncatedByBytes = true; return }
    bytes += size
    lines.push({ number: totalLines, text })
  }
  for await (const chunk of chunks) {
    let start = 0
    let newline: number
    while ((newline = chunk.indexOf('\n', start)) !== -1) {
      if (buffer.length <= READ_MAX_LINE_LENGTH + 1) buffer += chunk.slice(start, newline)
      consume(buffer)
      buffer = ''
      start = newline + 1
    }
    if (buffer.length <= READ_MAX_LINE_LENGTH + 1) buffer += chunk.slice(start)
  }
  if (buffer.length > 0) consume(buffer)
  if (!truncatedByBytes && offset > totalLines && !(totalLines === 0 && offset === 1)) {
    throw new Error(`offset ${offset} is out of range for "${displayPath}" (${totalLines} lines)`)
  }
  return { lines, totalLines, truncatedByBytes }
}

/**
 * The model-facing text of a read window.
 * @param displayPath Path shown to the model.
 * @param offset First requested line.
 * @param window Selected lines.
 * @returns `<path>…</path><type>file</type><content>…</content>`.
 */
export function formatRead(displayPath: string, offset: number, window: { lines: ReadDetails['lines']; totalLines: number; truncatedByBytes: boolean }): string {
  const endLine = window.lines.at(-1)?.number ?? Math.max(0, offset - 1)
  const footer = window.truncatedByBytes ? `(Output capped. Showing lines ${offset}-${endLine}. Use offset=${endLine + 1} to continue.)`
    : endLine < window.totalLines ? `(Showing lines ${offset}-${endLine} of ${window.totalLines}. Use offset=${endLine + 1} to continue.)`
      : `(End of file - total ${window.totalLines} lines)`
  const body = window.lines.length > 0 ? `${window.lines.map(line => `${line.number}: ${line.text}`).join('\n')}\n\n${footer}` : footer
  return `<path>${displayPath}</path>\n<type>file</type>\n<content>\n${body}\n</content>`
}

function positiveInteger(value: number | undefined, name: string, fallback: number): number {
  if (value === undefined) return fallback
  if (!Number.isInteger(value) || value < 1) throw new Error(`invalid ${name}: expected a positive integer`)
  return value
}

/**
 * The `read` tool.
 * @param context Chat tool context.
 * @returns Tool definition.
 */
export function readTool(context: ToolContext): AgentTool<typeof parameters, ReadDetails> {
  return {
    name: 'read',
    label: 'Read',
    description: 'Read a file or a bounded line range. Use small ranges for large files.',
    parameters,
    async execute(_id, params: Static<typeof parameters>, signal) {
      const offset = positiveInteger(params.offset, 'offset', 1)
      const limit = positiveInteger(params.limit, 'limit', READ_MAX_LINES)
      if (limit > READ_MAX_LINES) throw new Error(`invalid limit: must be at most ${READ_MAX_LINES}`)
      const { absolute, display } = resolvePath(context, params.file_path)
      const version = await versionOf(absolute)
      if (version === undefined) {
        context.observations.record(absolute, 'absent')
        throw new Error(`cannot read "${display}": not found`)
      }
      if (!(await stat(absolute)).isFile()) throw new Error(`cannot read "${display}": not a regular file`)
      const stream = createReadStream(absolute, { encoding: 'utf8', signal })
      const window = await readWindow(stream as AsyncIterable<string>, offset, limit, display)
      context.observations.record(absolute, version)
      context.onFileTouched?.(absolute)
      return textResult(formatRead(display, offset, window), { path: display, offset, lines: window.lines, totalLines: window.totalLines })
    },
  }
}
