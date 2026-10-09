/** Rainy settings commands navigate the existing settings dialog. */
import { useEffect } from 'react'
import type { PropsRuntime } from '@deepseek-ai/dsh-client-ui-slots'
import type {} from '@deepseek-ai/dsh-client-ui-settings/client'

/** Open feature sections through their single shared dialog owner.
 * @param props The settings owner's open action.
 * @returns No additional visible launcher.
 */
export function GeneralSettingsBridge({ openSettings }: PropsRuntime<'settings.launcher'>) {
  useEffect(() => {
    const general = (): void => { openSettings('general') }
    const open = (event: Event): void => {
      const detail: unknown = event instanceof CustomEvent ? event.detail : undefined
      const page = detail !== null && typeof detail === 'object' && 'page' in detail ? detail.page : undefined
      const section = page === 'extensions' ? 'skills' : page === 'runtime' ? 'runtime'
        : page === 'memory' ? 'memory' : page === 'preferences' ? 'general' : 'models'
      openSettings(section)
    }
    window.addEventListener('rainy:open-general-settings', general)
    window.addEventListener('rainy:open-panel', open)
    return () => {
      window.removeEventListener('rainy:open-general-settings', general)
      window.removeEventListener('rainy:open-panel', open)
    }
  }, [openSettings])
  return null
}
