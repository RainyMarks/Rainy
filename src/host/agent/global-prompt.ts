/** User-authored instructions rendered into every Agent's system prompt from the Rainy profile. */
import type { Context } from '@deepseek-ai/cordis'
import type {} from '@deepseek-ai/dsh-config-editor'

/** System-prompt section name; budget attribution finds the rendered text by this name. */
export const GLOBAL_PROMPT_SECTION = 'rainy:global-prompt'
/** After first-party guidance and before the working-directory persona suffix (10200). */
export const GLOBAL_PROMPT_ORDER = 9500

/** Saved prompt and the configured limit the settings page enforces before submission. */
export interface GlobalPromptSettings {
  text: string
  maxChars: number
}

/**
 * Normalize saved or submitted text against the configured limit.
 * @param text Raw text; surrounding whitespace is not model-visible.
 * @param maxChars Limit in UTF-16 code units, matching the settings textarea `maxLength`.
 * @returns Trimmed text; empty adds no section.
 */
export function checkedGlobalPrompt(text: string, maxChars: number): string {
  const trimmed = text.trim()
  if (trimmed.length > maxChars) throw new Error(`全局提示词超过 ${maxChars} 字符上限，请在模型设置中缩短。`)
  return trimmed
}

/**
 * Validate an authenticated settings request.
 * @param raw Control parameters `{ text }`.
 * @param maxChars Configured limit in UTF-16 code units.
 * @returns Trimmed text ready to persist.
 */
export function parseGlobalPrompt(raw: unknown, maxChars: number): string {
  if (raw === null || typeof raw !== 'object' || !('text' in raw) || typeof raw.text !== 'string') throw new Error('全局提示词必须是文本。')
  return checkedGlobalPrompt(raw.text, maxChars)
}

/**
 * Persist the prompt in the owning profile entry. `globalPrompt` is volatile, so Loader commits it to the
 * running Config reference without remounting the owner; the next prompt assembly renders the new text.
 * @param editor Active profile configuration editor.
 * @param entry Loader entry whose Config declares `globalPrompt`.
 * @param text Validated text; empty removes the profile override.
 * @returns Fulfillment after Loader reconciliation.
 */
export async function saveGlobalPrompt(
  editor: Context['configEditor'],
  entry: Parameters<Context['configEditor']['edit']>[0],
  text: string,
): Promise<void> {
  await editor.edit(entry, (current) => {
    const rest = Object.fromEntries(Object.entries(current).filter(([key]) => key !== 'globalPrompt'))
    return text === '' ? rest : { ...rest, globalPrompt: text }
  })
}
