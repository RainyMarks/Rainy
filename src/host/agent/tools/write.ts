/** `write` and `edit`: whole-file writes and literal replacements, both guarded by a prior read. */
import { mkdir, readFile, writeFile } from 'node:fs/promises'
import { dirname } from 'node:path'
import { Type, type Static } from '@earendil-works/pi-ai'
import type { AgentTool } from '@earendil-works/pi-agent-core'
import { MAX_SNAPSHOT_BYTES } from '../snapshots.ts'
import { resolvePath, textResult, versionOf, type ToolContext } from './common.ts'
import { decodeFile, encodeFile, encodingLabel, roundTrips, type DecodedFile } from './text-encoding.ts'

/** Text kept per side of a change for the UI diff card. */
const DIFF_TEXT_LIMIT = 256 * 1024

/** Before/after text for the UI's diff card; omitted when either side is too large. */
export interface ChangeDetails {
  path: string
  operation: 'create' | 'update'
  before?: string | undefined
  after?: string | undefined
  /** The previous content is kept, so the user can undo this change. */
  undoable?: boolean | undefined
}

/**
 * Keep the replaced content for undo. A failure costs only the undo, never the change itself.
 * @param context Chat tool context.
 * @param toolCallId The write or edit call.
 * @param path Absolute path.
 * @param before Previous content, `null` for a new file, `undefined` when it is too large to keep.
 * @param after Written bytes.
 * @returns Whether the change can be undone.
 */
async function keepForUndo(context: ToolContext, toolCallId: string, path: string, before: Uint8Array | null | undefined, after: Uint8Array): Promise<boolean> {
  if (before === undefined || context.recordChange === undefined) return false
  return context.recordChange(toolCallId, path, before, after).catch(() => false)
}

/**
 * A file's text for an edit, in an encoding it can be written back in without touching the rest of the file.
 * @param bytes File content.
 * @param display Path shown in errors.
 * @returns The decoded file.
 * @throws Error for binary files and text that would not convert back to the same bytes.
 */
function editableText(bytes: Uint8Array, display: string): DecodedFile {
  const decoded = decodeFile(bytes)
  if (decoded === undefined) {
    throw new Error(`cannot modify "${display}": it is binary or its text encoding is not UTF-8, UTF-16 with a byte-order mark or GB18030`)
  }
  if (!roundTrips(bytes, decoded)) {
    throw new Error(`cannot modify "${display}": its ${decoded.damaged ? 'UTF-8 text has invalid bytes' : `${encodingLabel(decoded.encoding)} text does not convert back to the same bytes`}, `
      + 'so an edit could damage it; change it with a command that keeps its bytes')
  }
  return decoded
}

function diffSides(before: string | undefined, after: string): Pick<ChangeDetails, 'before' | 'after'> {
  if ((before?.length ?? 0) > DIFF_TEXT_LIMIT || after.length > DIFF_TEXT_LIMIT) return {}
  return { ...(before === undefined ? {} : { before }), after }
}

const writeParameters = Type.Object({
  file_path: Type.String({ description: 'Path to write, resolved by the filesystem backend.' }),
  content: Type.String({ description: 'Full UTF-8 text content to write.' }),
})

/**
 * The `write` tool.
 * @param context Chat tool context.
 * @returns Tool definition.
 */
export function writeTool(context: ToolContext): AgentTool<typeof writeParameters, ChangeDetails> {
  return {
    name: 'write',
    label: 'Write',
    description: 'Create or replace a file. Inspect existing files before overwriting. A replaced file keeps its text encoding when it can.',
    parameters: writeParameters,
    executionMode: 'sequential',
    async execute(id, params: Static<typeof writeParameters>) {
      const { absolute, display } = resolvePath(context, params.file_path)
      const current = await versionOf(absolute)
      context.observations.assertCurrent(absolute, current, display, 'write')
      // `null`: the file is new; `undefined`: too large to keep for undo.
      const original = current === undefined ? null : current.size > MAX_SNAPSHOT_BYTES ? undefined : await readFile(absolute)
      const decoded = original ? decodeFile(original) : undefined
      const before = original && original.length <= DIFF_TEXT_LIMIT ? decoded?.text ?? original.toString('utf8') : undefined
      // A replaced file keeps its encoding and byte-order mark when its text converts back unchanged.
      const kept = original && decoded !== undefined && roundTrips(original, decoded) ? decoded : undefined
      const stored = kept === undefined ? undefined : encodeFile(kept, kept.bom && params.content.startsWith('\ufeff') ? params.content.slice(1) : params.content)
      const bytes = stored ?? Buffer.from(params.content, 'utf8')
      await mkdir(dirname(absolute), { recursive: true })
      await writeFile(absolute, bytes)
      const written = await versionOf(absolute)
      if (written !== undefined) context.observations.record(absolute, written)
      context.onFileTouched?.(absolute)
      const undoable = await keepForUndo(context, id, absolute, original, bytes)
      const operation = current === undefined ? 'create' : 'update'
      const encoding = kept === undefined || kept.encoding === 'utf-8' ? ''
        : stored === undefined ? ` as UTF-8: ${encodingLabel(kept.encoding)}, its previous encoding, cannot store some of the new characters`
          : ` in its ${encodingLabel(kept.encoding)} encoding`
      return textResult(`<path>${display}</path>\n<type>file</type>\n<content>\n${operation === 'create' ? 'Created' : 'Updated'} file${encoding}\n</content>`,
        { path: display, operation, ...diffSides(before, params.content), ...(undoable ? { undoable } : {}) })
    },
  }
}

const editParameters = Type.Object({
  file_path: Type.String({ description: 'Path to edit, resolved by the filesystem backend.' }),
  old_string: Type.String({ description: 'Literal text to replace.' }),
  new_string: Type.String({ description: 'Literal replacement text. Use an empty string to delete the match.' }),
  replace_all: Type.Optional(Type.Boolean({ description: 'Replace all matches. Defaults to false; when false, old_string must appear exactly once.' })),
})

function countOccurrences(text: string, needle: string): number {
  let count = 0
  let index = text.indexOf(needle)
  while (index !== -1) { count++; index = text.indexOf(needle, index + needle.length) }
  return count
}

/**
 * Replace literal text in LF-normalized content.
 * @param content File text with LF line endings.
 * @param oldString Text to find; CRLF inside it is normalized.
 * @param newString Replacement text; CRLF inside it is normalized.
 * @param replaceAll Replace every match instead of requiring exactly one.
 * @param displayPath Path shown in errors.
 * @returns The edited text.
 */
export function applyEdit(content: string, oldString: string, newString: string, replaceAll: boolean, displayPath: string): string {
  const oldNorm = oldString.replaceAll('\r\n', '\n')
  const newNorm = newString.replaceAll('\r\n', '\n')
  if (oldNorm.length === 0) throw new Error('old_string must be a non-empty string')
  if (oldNorm === newNorm) throw new Error('old_string and new_string must differ')
  const matches = countOccurrences(content, oldNorm)
  if (matches === 0) throw new Error(`old_string was not found in "${displayPath}"`)
  if (!replaceAll && matches > 1) throw new Error(`old_string matched ${matches} times in "${displayPath}"; provide a more specific old_string or set replace_all to true`)
  return replaceAll ? content.split(oldNorm).join(newNorm) : content.replace(oldNorm, () => newNorm)
}

/**
 * The `edit` tool. The file keeps its byte-order mark and its dominant line ending.
 * @param context Chat tool context.
 * @returns Tool definition.
 */
export function editTool(context: ToolContext): AgentTool<typeof editParameters, ChangeDetails> {
  return {
    name: 'edit',
    label: 'Edit',
    description: 'Apply an exact text replacement to a previously read file. The file keeps its text encoding.',
    parameters: editParameters,
    executionMode: 'sequential',
    async execute(id, params: Static<typeof editParameters>) {
      const { absolute, display } = resolvePath(context, params.file_path)
      const current = await versionOf(absolute)
      if (current === undefined) throw new Error(`cannot modify "${display}": not found`)
      context.observations.assertCurrent(absolute, current, display, 'modify')
      const bytes = await readFile(absolute)
      const decoded = editableText(bytes, display)
      const body = decoded.text
      const sample = body.slice(0, 4096)
      const crlf = sample.split('\r\n').length - 1
      const lf = sample.split('\n').length - 1 - crlf
      const normalized = body.replaceAll('\r\n', '\n')
      const edited = applyEdit(normalized, params.old_string, params.new_string, params.replace_all ?? false, display)
      const output = encodeFile(decoded, crlf > lf ? edited.split('\n').join('\r\n') : edited)
      if (output === undefined) {
        throw new Error(`cannot modify "${display}": it is stored as ${encodingLabel(decoded.encoding)}, which cannot store some characters of new_string`)
      }
      await writeFile(absolute, output)
      const written = await versionOf(absolute)
      if (written !== undefined) context.observations.record(absolute, written)
      context.onFileTouched?.(absolute)
      const undoable = await keepForUndo(context, id, absolute, bytes.length > MAX_SNAPSHOT_BYTES ? undefined : bytes, output)
      const message = params.replace_all ? `The file ${display} has been updated. All occurrences were successfully replaced.` : `The file ${display} has been updated successfully.`
      return textResult(message, { path: display, operation: 'update', ...diffSides(normalized, edited), ...(undoable ? { undoable } : {}) })
    },
  }
}
