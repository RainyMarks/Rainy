/** General section (language, chat preferences, diagnostics export and version) and the shortcut list. */
import clsx from 'clsx'
import { useState } from 'react'
import type { UiPreferences } from '../../shared/rpc.ts'
import { setPrefs, usePrefs } from '../prefs.ts'
import { Button } from '../ui/Button.tsx'
import { ShortcutKeys } from '../ui/ShortcutKeys.tsx'
import { Switch } from '../ui/Switch.tsx'
import { toast } from '../ui/toasts.tsx'
import { useT } from './messages.ts'
import { diagnosticsBridge } from './native.ts'
import type { SettingsMessage } from './messages.ts'
import { Choice, SettingRow, errorText } from './parts.tsx'
import css from './sections.module.css'

const shortcuts: readonly { keys: readonly string[]; key: SettingsMessage }[] = [
  { keys: ['Ctrl', 'Alt', 'P'], key: 'shortcutQuickOpen' },
  { keys: ['Ctrl', 'Shift', 'P'], key: 'shortcutPalette' },
  { keys: ['Ctrl', 'Shift', 'F'], key: 'shortcutSearchFiles' },
  { keys: ['Ctrl', 'Shift', 'H'], key: 'shortcutReplaceFiles' },
  { keys: ['Ctrl', 'Shift', 'G'], key: 'shortcutSourceControl' },
  { keys: ['Ctrl', 'Shift', 'E'], key: 'shortcutExplorer' },
  { keys: ['Ctrl', 'K'], key: 'shortcutAiEdit' },
  { keys: ['Ctrl', ','], key: 'shortcutSettings' },
  { keys: ['Ctrl', 'Alt', 'N'], key: 'shortcutNewChat' },
  { keys: ['Ctrl', 'Alt', 'K'], key: 'shortcutSearchChats' },
  { keys: ['Ctrl', 'Alt', 'O'], key: 'shortcutOpenFolder' },
  { keys: ['Ctrl', '`'], key: 'shortcutTerminal' },
  { keys: ['Enter'], key: 'shortcutSend' },
  { keys: ['Shift', 'Enter'], key: 'shortcutNewline' },
  { keys: ['Ctrl', 'Enter'], key: 'shortcutAlternate' },
  { keys: ['Esc', 'Esc'], key: 'shortcutStop' },
  { keys: ['F5'], key: 'shortcutReload' },
]

function change(value: Partial<UiPreferences>): void {
  setPrefs(value).catch((error: unknown) => { toast(errorText(error), { tone: 'error' }) })
}

/** @returns The General section; every change is stored by the Host through `prefs.set`. */
export function GeneralSection(): JSX.Element {
  const t = useT()
  const prefs = usePrefs()
  const version = typeof window === 'undefined' ? undefined : window.__RAINY_AGENT__?.version
  const diagnostics = diagnosticsBridge()
  const [exporting, setExporting] = useState(false)
  const exportDiagnostics = (): void => {
    if (diagnostics === undefined) return
    setExporting(true)
    diagnostics.export().then((path) => { if (path !== null) toast(t('diagnosticsSaved', { path }), { tone: 'success' }) },
      (error: unknown) => { toast(errorText(error), { tone: 'error' }) }).finally(() => { setExporting(false) })
  }
  return <section className={css.section} data-settings-section="general">
    <h2 className={css.heading}>{t('navGeneral')}</h2>
    <p className={css.lead}>{t('generalLead')}</p>
    <div className={css.group}>
      <SettingRow title={t('languageTitle')}>
        <Choice label={t('languageTitle')} value={prefs.locale} onChange={(locale) => { change({ locale }) }}
          items={[{ id: 'zh', label: t('languageZh') }, { id: 'en', label: t('languageEn') }]} />
      </SettingRow>
    </div>
    <h3 className={css.groupTitle}>{t('groupChat')}</h3>
    <div className={css.group}>
      <SettingRow title={t('busyEnterTitle')} description={t('busyEnterDescription')}>
        <Choice label={t('busyEnterTitle')} value={prefs.busyEnter} onChange={(busyEnter) => { change({ busyEnter }) }}
          items={[{ id: 'queue', label: t('busyEnterQueue') }, { id: 'steer', label: t('busyEnterSteer') }]} />
      </SettingRow>
      <SettingRow title={t('stepDetailTitle')} description={t('stepDetailDescription')}>
        <Choice label={t('stepDetailTitle')} value={prefs.stepDetail} onChange={(stepDetail) => { change({ stepDetail }) }}
          items={[{ id: 'compact', label: t('stepDetailCompact') }, { id: 'standard', label: t('stepDetailStandard') },
            { id: 'detailed', label: t('stepDetailDetailed') }]} />
      </SettingRow>
      <SettingRow title={t('usageTitle')} description={t('usageDescription')}>
        <Switch label={t('usageTitle')} checked={prefs.showUsage} onChange={(showUsage) => { change({ showUsage }) }} />
      </SettingRow>
    </div>
    {(diagnostics !== undefined || version !== undefined) && <>
      <h3 className={css.groupTitle}>{t('groupAbout')}</h3>
      <div className={css.group}>
        {diagnostics !== undefined && <SettingRow title={t('diagnosticsTitle')} description={t('diagnosticsDescription')}>
          <Button variant="outline" size="sm" disabled={exporting} onClick={exportDiagnostics}>{t(exporting ? 'diagnosticsExporting' : 'diagnosticsExport')}</Button>
        </SettingRow>}
        {version !== undefined && <SettingRow title={t('currentVersion', { version })} />}
      </div>
    </>}
  </section>
}

/** @returns The shortcut list, opened from the foot of the settings navigation. */
export function ShortcutsSection(): JSX.Element {
  const t = useT()
  return <section className={css.section} data-settings-section="shortcuts">
    <h2 className={css.heading}>{t('shortcutsTitle')}</h2>
    <p className={css.lead}>{t('shortcutsDescription')}</p>
    <dl className={clsx(css.group, css.shortcuts)} data-shortcuts>
      {shortcuts.map(shortcut => <div key={shortcut.key} className={css.shortcut}>
        <dt>{t(shortcut.key)}</dt>
        <dd><ShortcutKeys keys={shortcut.keys} className={css.keycaps} /></dd>
      </div>)}
    </dl>
  </section>
}
