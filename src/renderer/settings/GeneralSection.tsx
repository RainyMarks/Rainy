/** General section: language, appearance, font sizes, chat preferences, shortcut reference and version. */
import clsx from 'clsx'
import type { UiPreferences } from '../../shared/rpc.ts'
import { setPrefs, usePrefs } from '../prefs.ts'
import { ShortcutKeys } from '../ui/ShortcutKeys.tsx'
import { Switch } from '../ui/Switch.tsx'
import { toast } from '../ui/toasts.tsx'
import {
  IconChevronDownOutlineRegular, IconChevronUpOutlineRegular, IconDarkOutlineMedium, IconFollowsystemOutlineMedium, IconLightOutlineMedium,
} from '../ui/icons/index.tsx'
import { useT } from './messages.ts'
import type { SettingsMessage } from './messages.ts'
import { Select, SettingRow, errorText } from './parts.tsx'
import css from './sections.module.css'

/** Smallest and largest interface and code font sizes, in px. */
export const FONT_SIZE_RANGE = { min: 12, max: 17 } as const
/** Font sizes restored by the reset button; they match the Host defaults. */
export const DEFAULT_FONT_SIZES = { uiFontSize: 14, codeFontSize: 13 } as const

const themes = [
  { id: 'light', key: 'appearanceLight', Icon: IconLightOutlineMedium },
  { id: 'dark', key: 'appearanceDark', Icon: IconDarkOutlineMedium },
  { id: 'system', key: 'appearanceSystem', Icon: IconFollowsystemOutlineMedium },
] as const

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
  setPrefs(value).catch((error: unknown) => { toast(errorText(error)) })
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
  const sizes = [
    { field: 'uiFontSize', value: prefs.uiFontSize, title: 'fontSizeTitle', description: 'fontSizeDescription', up: 'fontSizeIncrease', down: 'fontSizeDecrease' },
    { field: 'codeFontSize', value: prefs.codeFontSize, title: 'codeFontSizeTitle', description: 'codeFontSizeDescription', up: 'codeFontSizeIncrease', down: 'codeFontSizeDecrease' },
  ] as const
  return <section className={css.section} data-settings-section="general">
    <div className={css.rows}>
      <SettingRow title={t('languageTitle')}>
        <Select label={t('languageTitle')} value={prefs.locale} onChange={(locale) => { change({ locale }) }}
          items={[{ id: 'zh', label: t('languageZh') }, { id: 'en', label: t('languageEn') }]} />
      </SettingRow>
      <div className={css.settingGroup}>
        <div className={css.settingTitle}>{t('appearanceTitle')}</div>
        <div className={css.cubes}>
          {themes.map(({ id, key, Icon }) => <button key={id} type="button" className={clsx(css.cube, prefs.theme === id && css.cubeSelected)}
            aria-pressed={prefs.theme === id} onClick={() => { change({ theme: id }) }}><Icon />{t(key)}</button>)}
        </div>
      </div>
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
      <SettingRow title={t('busyEnterTitle')} description={t('busyEnterDescription')}>
        <Select label={t('busyEnterTitle')} value={prefs.busyEnter} onChange={(busyEnter) => { change({ busyEnter }) }}
          items={[{ id: 'queue', label: t('busyEnterQueue') }, { id: 'steer', label: t('busyEnterSteer') }]} />
      </SettingRow>
      <SettingRow title={t('stepDetailTitle')} description={t('stepDetailDescription')}>
        <Select label={t('stepDetailTitle')} value={prefs.stepDetail} onChange={(stepDetail) => { change({ stepDetail }) }}
          items={[{ id: 'compact', label: t('stepDetailCompact') }, { id: 'standard', label: t('stepDetailStandard') },
            { id: 'detailed', label: t('stepDetailDetailed') }]} />
      </SettingRow>
      <SettingRow title={t('usageTitle')} description={t('usageDescription')}>
        <Switch label={t('usageTitle')} checked={prefs.showUsage} onChange={(showUsage) => { change({ showUsage }) }} />
      </SettingRow>
      <div className={css.settingGroup} data-shortcuts>
        <div className={css.settingTitle}>{t('shortcutsTitle')}</div>
        <div className={css.settingDescription}>{t('shortcutsDescription')}</div>
        <dl className={css.shortcuts}>
          {shortcuts.map(shortcut => <div key={shortcut.key} className={css.shortcut}>
            <dt>{t(shortcut.key)}</dt>
            <dd><ShortcutKeys keys={shortcut.keys} /></dd>
          </div>)}
        </dl>
      </div>
      {version !== undefined && <div className={css.version}>{t('currentVersion', { version })}</div>}
    </div>
  </section>
}
