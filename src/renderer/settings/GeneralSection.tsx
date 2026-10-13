/** General section (language, appearance, font sizes, chat preferences, diagnostics export and version) and the shortcut list. */
import clsx from 'clsx'
import { useState } from 'react'
import type { UiPreferences } from '../../shared/rpc.ts'
import { setPrefs, usePrefs } from '../prefs.ts'
import { Button } from '../ui/Button.tsx'
import { ShortcutKeys } from '../ui/ShortcutKeys.tsx'
import { Switch } from '../ui/Switch.tsx'
import { toast } from '../ui/toasts.tsx'
import { IconChevronDownOutlineRegular, IconChevronUpOutlineRegular } from '../ui/icons/index.tsx'
import { useT } from './messages.ts'
import { diagnosticsBridge } from './native.ts'
import type { SettingsMessage } from './messages.ts'
import { Choice, SettingRow, errorText } from './parts.tsx'
import css from './sections.module.css'

/** Smallest and largest interface and code font sizes, in px. */
export const FONT_SIZE_RANGE = { min: 12, max: 17 } as const
/** Font sizes restored by the reset button; they match the Host defaults. */
export const DEFAULT_FONT_SIZES = { uiFontSize: 14, codeFontSize: 13 } as const

const shortcuts: readonly { keys: readonly string[]; key: SettingsMessage }[] = [
  { keys: ['Ctrl', 'Alt', 'P'], key: 'shortcutQuickOpen' },
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

function changeSize(field: 'uiFontSize' | 'codeFontSize', value: number): void {
  const size = Math.min(FONT_SIZE_RANGE.max, Math.max(FONT_SIZE_RANGE.min, value))
  change(field === 'uiFontSize' ? { uiFontSize: size } : { codeFontSize: size })
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
  const sizes = [
    { field: 'uiFontSize', value: prefs.uiFontSize, title: 'fontSizeTitle', description: 'fontSizeDescription', up: 'fontSizeIncrease', down: 'fontSizeDecrease' },
    { field: 'codeFontSize', value: prefs.codeFontSize, title: 'codeFontSizeTitle', description: 'codeFontSizeDescription', up: 'codeFontSizeIncrease', down: 'codeFontSizeDecrease' },
  ] as const
  return <section className={css.section} data-settings-section="general">
    <h2 className={css.heading}>{t('navGeneral')}</h2>
    <p className={css.lead}>{t('generalLead')}</p>
    <h3 className={css.groupTitle}>{t('groupAppearance')}</h3>
    <div className={css.group}>
      <SettingRow title={t('languageTitle')}>
        <Choice label={t('languageTitle')} value={prefs.locale} onChange={(locale) => { change({ locale }) }}
          items={[{ id: 'zh', label: t('languageZh') }, { id: 'en', label: t('languageEn') }]} />
      </SettingRow>
      <SettingRow title={t('appearanceTitle')}>
        <Choice label={t('appearanceTitle')} value={prefs.theme} onChange={(theme) => { change({ theme }) }}
          items={[{ id: 'light', label: t('appearanceLight') }, { id: 'dark', label: t('appearanceDark') }, { id: 'system', label: t('appearanceSystem') }]} />
      </SettingRow>
      <div className={css.settingGroup}>
        {sizes.map(size => <SettingRow key={size.field} title={t(size.title)} description={t(size.description)}>
          <div className={css.stepperControl}>
            <div className={css.stepper}>
              <span className={css.stepperValue} data-font-size={size.field}>{size.value}</span>
              <span className={css.stepperArrows}>
                <button type="button" className={css.stepperArrow} aria-label={t(size.up)} disabled={size.value >= FONT_SIZE_RANGE.max}
                  onClick={() => { changeSize(size.field, size.value + 1) }}><IconChevronUpOutlineRegular size={9} /></button>
                <button type="button" className={css.stepperArrow} aria-label={t(size.down)} disabled={size.value <= FONT_SIZE_RANGE.min}
                  onClick={() => { changeSize(size.field, size.value - 1) }}><IconChevronDownOutlineRegular size={9} /></button>
              </span>
            </div>
            <span className={css.muted}>{t('fontSizeUnit')}</span>
          </div>
        </SettingRow>)}
        <div className={css.resetRow}>
          <button type="button" className={css.linkButton}
            disabled={prefs.uiFontSize === DEFAULT_FONT_SIZES.uiFontSize && prefs.codeFontSize === DEFAULT_FONT_SIZES.codeFontSize}
            onClick={() => { change({ ...DEFAULT_FONT_SIZES }) }}>{t('fontSizeReset')}</button>
        </div>
      </div>
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
