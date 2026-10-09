/** Profile-wide instructions edited inside the model settings page. */
import { useState } from 'react'
import { Button } from '@deepseek-ai/dsh-client-ui-primitives'
import type { TranslateNS } from '@deepseek-ai/dsh-client-locale/client'
import type { SettingsOperations, SettingsStatus } from './settings-protocol.ts'
import css from './SettingsSections.module.css'

/** Saved prompt state and the page's shared operation launcher. */
export interface GlobalPromptSettingsProps {
  saved: SettingsStatus['globalPrompt'] | undefined
  busy: boolean
  run: (operation: () => Promise<void>, success?: string) => void
  saveGlobalPrompt: SettingsOperations['saveGlobalPrompt']
  t: TranslateNS<'rainy'>
}

/** Edit the prompt added to every later model request; text the user has not edited follows the saved Host value.
 * @param props Saved value, page operation state, the Host save operation, and locale.
 * @returns A card with a bounded textarea and an explicit save action that keeps the draft after a failure.
 */
export function GlobalPromptSettings({ saved, busy, run, saveGlobalPrompt, t }: GlobalPromptSettingsProps) {
  const [draft, setDraft] = useState<string | undefined>()
  const text = draft ?? saved?.text ?? ''
  const maxChars = saved?.maxChars ?? 0
  const changed = saved !== undefined && text.trim() !== saved.text
  return <article className={css.card} data-rainy-global-prompt>
    <h3 className={css.subheading}>{t('settingsGlobalPrompt')}</h3>
    <p className={css.muted}>{t('settingsGlobalPromptNote')}</p>
    <textarea className={`${css.textarea} ${css.prose}`} aria-label={t('settingsGlobalPrompt')} value={text}
      placeholder={t('settingsGlobalPromptPlaceholder')} disabled={saved === undefined} maxLength={saved?.maxChars}
      onChange={(event) => { setDraft(event.target.value) }} />
    <div className={css.row}>
      <span className={css.muted}>{t('settingsGlobalPromptCount', { count: text.length.toLocaleString(), max: maxChars.toLocaleString() })}</span>
      <Button variant="primary" disabled={!changed || busy || text.length > maxChars} onClick={() => {
        run(async () => { await saveGlobalPrompt(text); setDraft(undefined) }, t('settingsSaved'))
      }}>{t('settingsSave')}</Button>
    </div>
  </article>
}
