/** Rainy-only controls for settled Assistant code fences. */
import type { HostObservable, InjectFace, PropsLocale, PropsRuntime } from '@deepseek-ai/dsh-client-ui-slots'
import type {} from '@deepseek-ai/dsh-client-ui-chat/client'
import type { Context } from '@deepseek-ai/cordis'
import type { IdeModel, IdeState } from './ide-model.ts'
import css from './IdeShell.module.css'

interface Injected { readonly model: IdeModel; readonly hooks: { readonly ide: HostObservable<IdeState> } }

function SnippetActions({ code, language, model, useIde, t }:
  PropsRuntime<'conversation.chat.code-actions'> & PropsLocale<'rainy'> & InjectFace<Injected>) {
  const canCompare = useIde(state => state.data.activePath !== null
    && state.buffers[state.data.activePath]?.source !== 'snippet'
    && state.buffers[state.data.activePath]?.document.readOnlyReason === null)
  return <span className={css.snippetActions}>
    <button type="button" className={css.button} onClick={() => { model.openSnippet(code, language, t('ideSnippet'), false) }}>{t('ideOpenSnippet')}</button>
    <button type="button" className={css.button} disabled={!canCompare}
      onClick={() => { model.openSnippet(code, language, t('ideSnippet'), true) }}>{t('ideCompareSnippet')}</button>
  </span>
}

/** Register source previews only in the Rainy profile's Assistant response slot.
 * @param ctx Rainy browser context.
 * @param model Retained workspace buffer owner.
 */
export function installSnippetActions(ctx: Context, model: IdeModel): void {
  ctx.slots.inject('conversation.chat.code-actions', () => ctx.slots.register({
    name: 'conversation.chat.code-actions', id: 'rainy-editor', locale: 'rainy',
    inject: (): Injected => ({ model, hooks: { ide: model.state } }),
  }, SnippetActions))
}
