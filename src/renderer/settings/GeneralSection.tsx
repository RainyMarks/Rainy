/** General section (language, chat preferences, diagnostics export and version) and the shortcut list. */
import clsx from 'clsx'
import { useEffect, useState } from 'react'
import type { UiPreferences } from '../../shared/rpc.ts'
import { setPrefs, usePrefs } from '../prefs.ts'
import { Button } from '../ui/Button.tsx'
import { ShortcutKeys } from '../ui/ShortcutKeys.tsx'
import { Switch } from '../ui/Switch.tsx'
import { toast } from '../ui/toasts.tsx'
import { useT } from './messages.ts'
import { diagnosticsBridge, shellBridge, type ShellIntegrationState } from './native.ts'
import type { SettingsMessage } from './messages.ts'
import { Choice, Select, SettingRow, errorText } from './parts.tsx'
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
  { keys: ['Ctrl', 'F'], key: 'shortcutMdFind' },
  { keys: ['Ctrl', '/'], key: 'shortcutMdSource' },
  { keys: ['Ctrl', '0–6'], key: 'shortcutMdHeading' },
  { keys: ['Ctrl', 'T'], key: 'shortcutMdTable' },
  { keys: ['Ctrl', 'Shift', 'K'], key: 'shortcutMdCode' },
  { keys: ['Ctrl', 'Shift', 'M'], key: 'shortcutMdMath' },
  { keys: ['Ctrl', 'Shift', 'Q'], key: 'shortcutMdQuote' },
  { keys: ['Ctrl', 'Click'], key: 'shortcutMdLink' },
  { keys: ['F8'], key: 'shortcutMdFocus' },
  { keys: ['F9'], key: 'shortcutMdTypewriter' },
]

function change(value: Partial<UiPreferences>): void {
  setPrefs(value).catch((error: unknown) => { toast(errorText(error), { tone: 'error' }) })
}

/** @returns Explorer's "Open with Rainy", the file types Rainy is offered for, and the way to Windows' Default apps. */
function SystemIntegration(): JSX.Element | null {
  const t = useT()
  const shell = shellBridge()
  const [state, setState] = useState<ShellIntegrationState | undefined>()
  const [busy, setBusy] = useState(false)
  useEffect(() => {
    let current = true
    shell?.state().then((value) => { if (current) setState(value) }, (error: unknown) => { toast(errorText(error), { tone: 'error' }) })
    return () => { current = false }
  }, [shell])
  if (shell === undefined || state === undefined) return null
  const change = (value: { contextMenu?: boolean; fileTypes?: boolean }): void => {
    setBusy(true)
    shell.set(value).then(setState, (error: unknown) => { toast(errorText(error), { tone: 'error' }) }).finally(() => { setBusy(false) })
  }
  const unavailable = state.available ? undefined : t('shellUnavailable')
  return <>
    <h3 className={css.groupTitle}>{t('groupSystem')}</h3>
    <div className={css.group} data-settings-shell>
      <SettingRow title={t('contextMenuTitle')} description={unavailable ?? t('contextMenuDescription')}>
        <Switch label={t('contextMenuTitle')} checked={state.contextMenu} disabled={!state.available || busy} onChange={(contextMenu) => { change({ contextMenu }) }} />
      </SettingRow>
      <SettingRow title={t('fileTypesTitle')} description={unavailable ?? t('fileTypesDescription')}>
        <Switch label={t('fileTypesTitle')} checked={state.fileTypes} disabled={!state.available || busy} onChange={(fileTypes) => { change({ fileTypes }) }} />
      </SettingRow>
      <SettingRow title={t('defaultAppsTitle')} description={t('defaultAppsDescription')}>
        <Button variant="outline" size="sm" disabled={!state.available || !state.fileTypes || busy}
          onClick={() => { shell.openDefaultApps().catch((error: unknown) => { toast(errorText(error), { tone: 'error' }) }) }}>{t('defaultAppsOpen')}</Button>
      </SettingRow>
    </div>
  </>
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
    <h3 className={css.groupTitle}>{t('groupMarkdown')}</h3>
    <div className={css.group} data-settings-markdown>
      <SettingRow title={t('imageFolderTitle')} description={t('imageFolderDescription')}>
        <Select label={t('imageFolderTitle')} value={prefs.markdownImages} onChange={(markdownImages) => { change({ markdownImages }) }}
          items={[{ id: 'assets', label: t('imageFolderAssets') }, { id: 'named', label: t('imageFolderNamed') }, { id: 'same', label: t('imageFolderSame') }]} />
      </SettingRow>
      <SettingRow title={t('focusModeTitle')} description={t('focusModeDescription')}>
        <Switch label={t('focusModeTitle')} checked={prefs.markdownFocus} onChange={(markdownFocus) => { change({ markdownFocus }) }} />
      </SettingRow>
      <SettingRow title={t('typewriterTitle')} description={t('typewriterDescription')}>
        <Switch label={t('typewriterTitle')} checked={prefs.markdownTypewriter} onChange={(markdownTypewriter) => { change({ markdownTypewriter }) }} />
      </SettingRow>
    </div>
    <SystemIntegration />
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
