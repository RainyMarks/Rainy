/** Web search for chats through DeepSeek's Search and Fetch APIs: on or off, and whether a DeepSeek key is there to pay for it. */
import type { ModelsStatus } from '../../shared/rpc.ts'
import { setPrefs, usePrefs } from '../prefs.ts'
import { Switch } from '../ui/Switch.tsx'
import { toast } from '../ui/toasts.tsx'
import { useT } from './messages.ts'
import { Notice, SettingRow, errorText } from './parts.tsx'
import css from './sections.module.css'

/** @param baseURL A model's service address. @returns Whether it is DeepSeek's official API, whose key web search uses. */
function isDeepSeek(baseURL: string): boolean {
  try { return new URL(baseURL).hostname.toLowerCase() === 'api.deepseek.com' } catch { return false }
}

/**
 * @param props.status Saved models and which of them have keys.
 * @returns A card with the switch and what web search needs.
 */
export function WebSearchSettings({ status }: { status: ModelsStatus | undefined }): JSX.Element {
  const t = useT()
  const prefs = usePrefs()
  const keyed = status?.models.some(model => isDeepSeek(model.baseURL) && status.credentials.includes(model.provider)) === true
  return <article className={css.card} data-web-search>
    <h3 className={css.subheading}>{t('webSearchTitle')}</h3>
    <p className={css.muted}>{t('webSearchNote')}</p>
    <div className={css.group}>
      <SettingRow title={t('webSearchSwitch')} description={t('webSearchSwitchNote')}>
        <Switch label={t('webSearchSwitch')} checked={prefs.webSearch} onChange={(webSearch) => {
          setPrefs({ webSearch }).catch((failure: unknown) => { toast(errorText(failure), { tone: 'error' }) })
        }} />
      </SettingRow>
    </div>
    {status !== undefined && prefs.webSearch && (keyed ? <Notice>{t('webSearchReady')}</Notice> : <Notice>{t('webSearchNoKey')}</Notice>)}
  </article>
}
