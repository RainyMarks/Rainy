/** Models → AI in the editor: ghost-text suggestions and the model behind them, and where Ctrl+K rewrites come from. */
import type { ModelsStatus, UiPreferences } from '../../shared/rpc.ts'
import { setPrefs, usePrefs } from '../prefs.ts'
import { Switch } from '../ui/Switch.tsx'
import { toast } from '../ui/toasts.tsx'
import { useT } from './messages.ts'
import { Select, SettingRow, errorText } from './parts.tsx'
import css from './sections.module.css'

function change(value: Partial<UiPreferences>): void {
  setPrefs(value).catch((error: unknown) => { toast(errorText(error), { tone: 'error' }) })
}

/**
 * @param props.status Saved models and presets; `undefined` while loading.
 * @returns The card.
 */
export function EditorAiSettings({ status }: { status: ModelsStatus | undefined }): JSX.Element {
  const t = useT()
  const prefs = usePrefs()
  const presets = status?.presets ?? []
  const nameOf = (model: { provider: string; model: string }): string =>
    presets.find(preset => preset.model.provider === model.provider && preset.model.model === model.model)?.name ?? `${model.provider}/${model.model}`
  const models = status?.models ?? []
  // A model removed since it was chosen falls back to the chat model, as the Host does.
  const chosen = models.some(model => model.provider === prefs.completionModel) ? prefs.completionModel : ''
  return <article className={css.card} data-editor-ai>
    <h3 className={css.subheading}>{t('editorAiTitle')}</h3>
    <div className={css.rows}>
      <SettingRow title={t('editorAiSuggest')} description={t('editorAiSuggestNote')}>
        <Switch label={t('editorAiSuggest')} checked={prefs.inlineCompletion} onChange={(inlineCompletion) => { change({ inlineCompletion }) }} />
      </SettingRow>
      <SettingRow title={t('editorAiModel')} description={t('editorAiModelNote')}>
        <Select label={t('editorAiModel')} value={chosen} className={css.fontSelect} onChange={(completionModel) => { change({ completionModel }) }}
          items={[{ id: '', label: t('editorAiSameModel') }, ...models.map(model => ({ id: model.provider, label: nameOf(model) }))]} />
      </SettingRow>
    </div>
    <p className={css.muted}>{t('editorAiEditNote')}</p>
  </article>
}
