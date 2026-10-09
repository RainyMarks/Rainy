/** Rainy sections reuse the shared settings dialog and app-wide outcome surface. */
import type { Context } from '@deepseek-ai/cordis'
import type { HostObservable, InjectFace, PropsRuntime } from '@deepseek-ai/dsh-client-ui-slots'
import type {} from '@deepseek-ai/dsh-client-ui-settings/client'
import { createSnapshotStore } from '@deepseek-ai/dsh-client-store'
import { Toast } from '@deepseek-ai/dsh-client-ui-primitives'
import type { IdeModel } from './ide-model.ts'
import type { Config } from '../config.ts'
import { SettingsController } from './settings-controller.ts'
import { ModelsSection, ExtensionsSection, type SettingsSectionInjected } from './SettingsSections.tsx'
import { RuntimeSection, MemorySection } from './ProjectSettingsSections.tsx'
import { GeneralSettingsBridge } from './GeneralSettingsBridge.tsx'
import type { StrataNativeHost } from '../strata-protocol.ts'
import { StrataController } from './strata-controller.ts'

interface Notice { id: number; text: string; success: boolean }
interface ToastInjected {
  readonly hooks: { readonly notice: HostObservable<Notice | null> }
  readonly dismiss: () => void
}
function SettingsToast({ useNotice, dismiss }: PropsRuntime<'shell.overlay'> & InjectFace<ToastInjected>) {
  const notice = useNotice(value => value)
  return notice === null ? null : <Toast key={notice.id} text={notice.text} {...notice.success ? { tone: 'success' as const } : {}} onDone={dismiss} />
}

/** Register feature-owned pages without adding another dialog or modifying generic appearance controls.
 * @param ctx Client feature context.
 * @param editor Current project source.
 * @param config Visibility-scoped refresh interval and new-model defaults.
 */
export function installRainySettings(ctx: Context, editor: IdeModel, config: Config): void {
  const controller = new SettingsController()
  const notices = createSnapshotStore<Notice | null>(null)
  let sequence = 0
  const t = ctx.locale.bind('rainy')
  const notify = (text: string, success = false): void => { notices.set({ id: ++sequence, text, success }) }
  const host = globalThis as typeof globalThis & { __RAINY_STRATA_NATIVE__?: StrataNativeHost }
  const strata = new StrataController(host.__RAINY_STRATA_NATIVE__, {
    saved: () => t('strataSaved'), stopped: () => t('strataStopped'), connected: () => t('strataConnected'),
  }, notify, async (selected) => {
    await controller.refreshAfterChange()
    const model = controller.state.getSnapshot().status?.models.find(value =>
      value.provider === selected.provider && value.model === selected.model)
    if (model === undefined) throw new Error(t('strataSelectionUnavailable'))
    return model
  })
  const inject = (): SettingsSectionInjected => ({
    ...controller.operations, ...strata.actions, pollMs: config.editorPollMs,
    localModelContextWindow: config.localModelContextWindow, apiModelContextWindow: config.apiModelContextWindow,
    notify,
    hooks: { settings: controller.state, ide: editor.state, strata: strata.state },
  })
  // The shared General section registers order 0; Rainy pages follow it in everyday-use order.
  const sections = [
    { id: 'models', order: 10, labelKey: 'settingsModels', component: ModelsSection },
    { id: 'skills', order: 20, labelKey: 'extensions', component: ExtensionsSection },
    { id: 'runtime', order: 30, labelKey: 'settingsRuntime', component: RuntimeSection },
    { id: 'memory', order: 40, labelKey: 'settingsMemory', component: MemorySection },
  ] as const
  for (const section of sections) ctx.slots.inject('settings.section', () => ctx.slots.register({
    name: 'settings.section', id: section.id, order: section.order, label: () => t(section.labelKey), locale: 'rainy', inject,
  }, section.component))
  ctx.slots.inject('settings.launcher', () => ctx.slots.register({ name: 'settings.launcher' }, GeneralSettingsBridge))
  ctx.slots.inject('shell.overlay', () => ctx.slots.register({ name: 'shell.overlay', id: 'rainy-settings-outcome',
    inject: (): ToastInjected => ({ hooks: { notice: notices }, dismiss: () => { notices.set(null) } }),
  }, SettingsToast))
  ctx.effect(() => () => { strata.dispose(); controller.dispose() })
}
