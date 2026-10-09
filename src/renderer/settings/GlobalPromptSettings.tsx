/** Instructions added to every model request, edited inside the Models section. */
import { useState } from 'react'
import clsx from 'clsx'
import type { ModelsStatus } from '../../shared/rpc.ts'
import { host } from '../rpc.ts'
import { Button } from '../ui/Button.tsx'
import { useT } from './messages.ts'
import type { SettingsAction } from './parts.tsx'
import css from './sections.module.css'

/** Saved prompt, the section's operation runner and the status publisher. */
export interface GlobalPromptSettingsProps {
  saved: ModelsStatus['globalPrompt'] | undefined
  action: SettingsAction
  accept(status: ModelsStatus): void
}

/**
 * Edit the global prompt. Text the user has not edited follows the saved value; a rejected save keeps the draft.
 * @param props Saved value, operation runner and status publisher.
 * @returns A card with a bounded textarea and a save button.
 */
export function GlobalPromptSettings({ saved, action, accept }: GlobalPromptSettingsProps): JSX.Element {
  const t = useT()
  const [draft, setDraft] = useState<string | undefined>()
  const text = draft ?? saved?.text ?? ''
  const maxChars = saved?.maxChars ?? 0
  const changed = saved !== undefined && text.trim() !== saved.text
  return <article className={css.card} data-global-prompt>
    <h3 className={css.subheading}>{t('settingsGlobalPrompt')}</h3>
    <p className={css.muted}>{t('settingsGlobalPromptNote')}</p>
    <textarea className={clsx(css.textarea, css.prose)} aria-label={t('settingsGlobalPrompt')} value={text}
      placeholder={t('settingsGlobalPromptPlaceholder')} disabled={saved === undefined} maxLength={saved?.maxChars}
      onChange={(event) => { setDraft(event.target.value) }} />
    <div className={css.row}>
      <span className={css.muted}>{t('settingsGlobalPromptCount', { count: text.length.toLocaleString('en-US'), max: maxChars.toLocaleString('en-US') })}</span>
      <Button variant="primary" disabled={!changed || action.busy || text.length > maxChars} onClick={() => {
        action.run(async () => { accept(await host.call('prompt.global', { text })); setDraft(undefined) }, t('settingsSaved'))
      }}>{t('settingsSave')}</Button>
    </div>
  </article>
}
