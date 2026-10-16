/** Appearance section: color mode, color scheme, accent color, rain effects, interface and code fonts, and font sizes. */
import { useMemo, useState } from 'react'
import { ACCENTS, COLOR_SCHEMES, SCHEME_ACCENTS, effectiveAccent, fontFamilies, normalizeFont } from '../../shared/appearance.ts'
import type { Accent, ColorScheme } from '../../shared/appearance.ts'
import type { UiPreferences } from '../../shared/rpc.ts'
import { setPrefs, usePrefs } from '../prefs.ts'
import { Button } from '../ui/Button.tsx'
import { Switch } from '../ui/Switch.tsx'
import { isComposingKey } from '../ui/keyboard-composition.ts'
import { toast } from '../ui/toasts.tsx'
import { IconChevronDownOutlineRegular, IconChevronUpOutlineRegular } from '../ui/icons/index.tsx'
import { CODE_FONT_CANDIDATES, FONT_NAMES_ZH, UI_FONT_CANDIDATES, fontInstalled, installedFonts } from './fonts.ts'
import { useT } from './messages.ts'
import type { SettingsMessage } from './messages.ts'
import { Choice, Select, SettingRow, errorText } from './parts.tsx'
import css from './sections.module.css'

/** Smallest and largest interface and code font sizes, in px. */
export const FONT_SIZE_RANGE = { min: 12, max: 17 } as const
/** What "Reset appearance" restores; it matches the Host defaults. */
export const DEFAULT_APPEARANCE = {
  theme: 'system', colorScheme: 'graphite', accent: 'auto', uiFont: '', codeFont: '', uiFontSize: 14, codeFontSize: 13, rainEffects: true,
} as const satisfies Partial<UiPreferences>

const schemeLabels: Readonly<Record<ColorScheme, SettingsMessage>> = {
  graphite: 'schemeGraphite', slate: 'schemeSlate', midnight: 'schemeMidnight', sand: 'schemeSand', forest: 'schemeForest',
  rose: 'schemeRose', black: 'schemeBlack',
}
const accentLabels: Readonly<Record<Accent, SettingsMessage>> = {
  auto: 'accentAuto', blue: 'accentBlue', violet: 'accentViolet', green: 'accentGreen', orange: 'accentOrange', pink: 'accentPink',
  teal: 'accentTeal',
}
/** Menu ids of the default and custom font entries; a colon never appears in a family name. */
const DEFAULT_FONT = 'default:'
const CUSTOM_FONT = 'custom:'

function change(value: Partial<UiPreferences>): void {
  setPrefs(value).catch((error: unknown) => { toast(errorText(error), { tone: 'error' }) })
}

/** @returns The Appearance section; every change is stored by the Host through `prefs.set`. */
export function AppearanceSection(): JSX.Element {
  const t = useT()
  const prefs = usePrefs()
  const sizes = [
    { field: 'uiFontSize', value: prefs.uiFontSize, title: 'fontSizeTitle', description: 'fontSizeDescription', up: 'fontSizeIncrease', down: 'fontSizeDecrease' },
    { field: 'codeFontSize', value: prefs.codeFontSize, title: 'codeFontSizeTitle', description: 'codeFontSizeDescription', up: 'codeFontSizeIncrease', down: 'codeFontSizeDecrease' },
  ] as const
  const unchanged = (Object.keys(DEFAULT_APPEARANCE) as (keyof typeof DEFAULT_APPEARANCE)[]).every(key => prefs[key] === DEFAULT_APPEARANCE[key])
  /** Counts resets; remounting the font rows closes their custom entries. */
  const [resets, setResets] = useState(0)
  return <section className={css.section} data-settings-section="appearance">
    <h2 className={css.heading}>{t('navAppearance')}</h2>
    <p className={css.lead}>{t('appearanceLead')}</p>
    <h3 className={css.groupTitle}>{t('groupColors')}</h3>
    <div className={css.group}>
      <SettingRow title={t('colorModeTitle')} description={t('colorModeDescription')}>
        <Choice label={t('colorModeTitle')} value={prefs.theme} onChange={(theme) => { change({ theme }) }}
          items={[{ id: 'light', label: t('appearanceLight') }, { id: 'dark', label: t('appearanceDark') }, { id: 'system', label: t('appearanceSystem') }]} />
      </SettingRow>
      <div className={css.stackedRow}>
        <span className={css.settingText}>
          <span className={css.settingTitle}>{t('colorSchemeTitle')}</span>
          <span className={css.settingDescription}>{t('colorSchemeDescription')}</span>
        </span>
        <div className={css.swatches} role="group" aria-label={t('colorSchemeTitle')}>
          {COLOR_SCHEMES.map(scheme => <button key={scheme} type="button" className={css.swatch} aria-pressed={scheme === prefs.colorScheme}
            data-scheme-option={scheme} onClick={() => { if (scheme !== prefs.colorScheme) change({ colorScheme: scheme }) }}>
            <span className={css.preview} data-color-scheme={scheme} data-accent={effectiveAccent(scheme, prefs.accent)} aria-hidden="true">
              <span className={css.previewSide}><span className={css.previewLine} /><span className={css.previewLine} /></span>
              <span className={css.previewMain}>
                <span className={css.previewLine} /><span className={css.previewBubble} /><span className={css.previewAccent} />
              </span>
            </span>
            <span className={css.swatchLabel}>{t(schemeLabels[scheme])}</span>
          </button>)}
        </div>
      </div>
      <SettingRow title={t('accentTitle')} description={t('accentDescription')}>
        <div className={css.accents} role="group" aria-label={t('accentTitle')}>
          {ACCENTS.map(accent => <button key={accent} type="button" className={accent === 'auto' ? css.accentAuto : css.accentDot}
            aria-pressed={accent === prefs.accent} aria-label={t(accentLabels[accent])} title={t(accentLabels[accent])}
            data-accent={accent === 'auto' ? SCHEME_ACCENTS[prefs.colorScheme] : accent}
            onClick={() => { if (accent !== prefs.accent) change({ accent }) }}>{accent === 'auto' ? t('accentAuto') : null}</button>)}
        </div>
      </SettingRow>
    </div>
    <h3 className={css.groupTitle}>{t('groupMotion')}</h3>
    <div className={css.group}>
      <SettingRow title={t('rainEffectsTitle')} description={t('rainEffectsDescription')}>
        <Switch label={t('rainEffectsTitle')} checked={prefs.rainEffects} onChange={(rainEffects) => { change({ rainEffects }) }} />
      </SettingRow>
    </div>
    <h3 className={css.groupTitle}>{t('groupFonts')}</h3>
    <div className={css.group}>
      <FontSetting key={`ui-${resets}`} field="uiFont" candidates={UI_FONT_CANDIDATES}
        labels={{ title: 'uiFontTitle', description: 'uiFontDescription', custom: 'uiFontCustomLabel' }} />
      <FontSetting key={`code-${resets}`} field="codeFont" candidates={CODE_FONT_CANDIDATES}
        labels={{ title: 'codeFontTitle', description: 'codeFontDescription', custom: 'codeFontCustomLabel' }} />
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
      <div className={css.fontPreview} data-font-preview>
        <p>{t('fontPreviewText')}</p>
        <code>{t('fontPreviewCode')}</code>
      </div>
    </div>
    <div className={css.resetRow}>
      <button type="button" className={css.linkButton} disabled={unchanged} onClick={() => { setResets(resets + 1); change({ ...DEFAULT_APPEARANCE }) }}>
        {t('appearanceReset')}
      </button>
    </div>
  </section>
}

function changeSize(field: 'uiFontSize' | 'codeFontSize', value: number): void {
  const size = Math.min(FONT_SIZE_RANGE.max, Math.max(FONT_SIZE_RANGE.min, value))
  change(field === 'uiFontSize' ? { uiFontSize: size } : { codeFontSize: size })
}

/**
 * A font row: the default, the installed families worth offering, or names typed in.
 * @param props.field Preference the row stores.
 * @param props.candidates Families offered when installed.
 * @param props.labels Row title and description, and the name of the custom entry.
 * @returns The row, and below it the custom entry while that is chosen.
 */
function FontSetting({ field, candidates, labels }: {
  field: 'uiFont' | 'codeFont'
  candidates: readonly string[]
  labels: { title: SettingsMessage; description: SettingsMessage; custom: SettingsMessage }
}): JSX.Element {
  const t = useT()
  const prefs = usePrefs()
  const value = prefs[field]
  const installed = useMemo(() => installedFonts(candidates), [candidates])
  const listed = value === '' || installed.includes(value)
  const [editing, setEditing] = useState(false)
  const [draft, setDraft] = useState(listed ? '' : value)
  const [error, setError] = useState('')
  const custom = editing || !listed
  const store = (font: string): void => { change(field === 'uiFont' ? { uiFont: font } : { codeFont: font }) }
  const apply = (): void => {
    const font = normalizeFont(draft)
    if (font === undefined) { setError(t('fontInvalid')); return }
    setError('')
    setDraft(font)
    if (font !== value) store(font)
  }
  const missing = !listed && !(fontFamilies(value) ?? []).some(fontInstalled)
  const items = [
    { id: DEFAULT_FONT, label: t('fontDefault') },
    ...installed.map(name => ({ id: name, label: prefs.locale === 'zh' ? FONT_NAMES_ZH[name] ?? name : name, font: `"${name}"` })),
    { id: CUSTOM_FONT, label: t('fontCustom') },
  ]
  return <>
    <SettingRow title={t(labels.title)} description={t(labels.description)}>
      <Select label={t(labels.title)} className={css.fontSelect} value={custom ? CUSTOM_FONT : value === '' ? DEFAULT_FONT : value} items={items} onChange={(id) => {
        setError('')
        if (id === CUSTOM_FONT) { setEditing(true); if (draft === '') setDraft(value); return }
        setEditing(false)
        setDraft('')
        store(id === DEFAULT_FONT ? '' : id)
      }} />
    </SettingRow>
    {custom && <div className={css.customFont} data-custom-font={field}>
      <div className={css.customFontEntry}>
        <input className={css.input} value={draft} aria-label={t(labels.custom)} placeholder={t('fontCustomPlaceholder')}
          spellCheck={false} onChange={(event) => { setDraft(event.target.value); setError('') }}
          onKeyDown={(event) => { if (event.key === 'Enter' && !isComposingKey(event.nativeEvent)) apply() }} />
        <Button variant="outline" size="sm" onClick={apply}>{t('fontCustomApply')}</Button>
      </div>
      {error !== '' ? <p className={css.errorText}>{error}</p>
        : value !== '' && missing ? <p className={css.muted}>{t('fontMissing', { name: value })}</p>
          : <p className={css.muted}>{t('fontCustomHint')}</p>}
    </div>}
  </>
}
