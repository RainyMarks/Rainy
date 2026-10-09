/** Model and extension pages rendered inside the shared settings dialog. */
import { useEffect, useRef, useState } from 'react'
import type { ReactNode } from 'react'
import { Button, IconLoadingOutlineRegular } from '@deepseek-ai/dsh-client-ui-primitives'
import type { HostObservable, InjectFace, PropsLocale, PropsRuntime } from '@deepseek-ai/dsh-client-ui-slots'
import type { IdeState } from './ide-model.ts'
import type { SettingsSnapshot } from './settings-controller.ts'
import type { BudgetPreview, RainyModelDiscovery, RainyModelSetup, SettingsOperations } from './settings-protocol.ts'
import { Choice } from './Choice.tsx'
import { StrataSettings } from './StrataSettings.tsx'
import { GlobalPromptSettings } from './GlobalPromptSettings.tsx'
import type { StrataActions, StrataSnapshot } from './strata-controller.ts'
import css from './SettingsSections.module.css'

/** Section callbacks and observable data supplied by the Rainy settings owner. */
export interface SettingsSectionInjected extends SettingsOperations, StrataActions {
  readonly pollMs: number
  readonly localModelContextWindow: number
  readonly apiModelContextWindow: number
  readonly notify: (text: string, success?: boolean) => void
  readonly hooks: {
    readonly settings: HostObservable<SettingsSnapshot>
    readonly ide: HostObservable<IdeState>
    readonly strata: HostObservable<StrataSnapshot>
  }
}
/** Derived props shared by the registered Rainy settings sections. */
export type SettingsSectionProps = PropsRuntime<'settings.section'> & PropsLocale<'rainy'> & InjectFace<SettingsSectionInjected>

/** Local operation lifetime; completion notifications are owned outside the settings dialog.
 * @param notify App-wide transient outcome reporter.
 * @returns Pending state and an exception-contained action launcher.
 */
export function useSettingsAction(notify: SettingsSectionInjected['notify']) {
  const [busy, setBusy] = useState(false)
  const pending = useRef(false)
  const mounted = useRef(true)
  useEffect(() => { mounted.current = true; return () => { mounted.current = false } }, [])
  const run = (operation: () => Promise<void>, success?: string): void => {
    if (pending.current) return
    pending.current = true
    setBusy(true)
    void operation().then(() => { if (success !== undefined) notify(success, true) })
      .catch((error: unknown) => { notify(error instanceof Error ? error.message : String(error)) })
      .finally(() => { pending.current = false; if (mounted.current) setBusy(false) })
  }
  return { busy, run }
}

/** Accessible page-level loading state.
 * @param props Localized loading label.
 * @returns Centered spinner.
 */
export function SettingsLoading({ label }: { label: string }) {
  return <div className={css.spinner} role="status" aria-label={label}><IconLoadingOutlineRegular size={20} /></div>
}

interface ModelForm {
  provider: string
  baseURL: string
  model: string
  context: string
  contextEdited: boolean
  output: string
  apiKey: string
  local: boolean
  api: NonNullable<RainyModelSetup['api']>
  thinking: NonNullable<RainyModelSetup['thinking']>
  thinkingFormat: NonNullable<RainyModelSetup['thinkingFormat']>
  maxTokensField: NonNullable<RainyModelSetup['maxTokensField']>
}

/** A new local model uses the configured window without inheriting another endpoint's credentials.
 * @param contextWindow Initial local-model context window.
 * @returns New model form with automatic output allocation.
 */
export function emptyModelForm(contextWindow: number): ModelForm {
  return { provider: '', baseURL: '', model: '', context: String(contextWindow), contextEdited: false, output: '', apiKey: '', local: true,
    api: 'openai-completions', thinking: 'off', thinkingFormat: 'openai', maxTokensField: 'max_tokens' }
}

function formOf(model: RainyModelSetup): ModelForm {
  return { provider: model.provider, baseURL: model.baseURL, model: model.model, context: String(model.contextWindow),
    contextEdited: true,
    output: model.maxTokens === undefined ? '' : String(model.maxTokens), apiKey: '', local: model.local,
    api: model.api ?? (model.local ? 'openai-completions' : 'openai-responses'), thinking: model.thinking ?? 'off',
    thinkingFormat: model.thinkingFormat ?? 'openai', maxTokensField: model.maxTokensField ?? 'max_tokens' }
}

/** Default model, global prompt, request accounting, and the optional local engine, in order of everyday use.
 * @param props Host settings operations and locale.
 * @returns Model settings inside the existing settings content column.
 */
export function ModelsSection({
  useSettings, useIde, refresh, configure, saveGlobalPrompt, discover, probe, previewBudget, notify, pollMs,
  localModelContextWindow, apiModelContextWindow, t,
  useStrata, strataRefresh, strataSave, strataStart, strataStop, strataChoose, strataConnect,
}: SettingsSectionProps) {
  const snapshot = useSettings(value => value)
  const strata = useStrata(value => value)
  const sessionId = useIde(value => value.data.lastSessionId)
  const workspaceId = useIde(value => value.workspace?.workspaceId)
  const [previewResult, setPreview] = useState<{ key: string; value: BudgetPreview }>()
  const [previewError, setPreviewError] = useState('')
  const [form, setForm] = useState(() => emptyModelForm(localModelContextWindow))
  const [selected, setSelected] = useState('')
  const [result, setResult] = useState('')
  const initialized = useRef(false)
  const { busy, run } = useSettingsAction(notify)
  useEffect(() => {
    void refresh()
    void strataRefresh()
    const timer = setInterval(() => { if (!document.hidden) { void refresh(); void strataRefresh() } }, pollMs)
    return () => { clearInterval(timer) }
  }, [refresh, strataRefresh, pollMs])
  useEffect(() => {
    const status = snapshot.status
    if (initialized.current || status === undefined) return
    initialized.current = true
    const saved = status.models.find(model => model.provider === status.selected?.provider && model.model === status.selected.model)
    if (saved !== undefined) { setForm(formOf(saved)); setSelected(`${saved.provider}/${saved.model}`) }
  }, [snapshot.status])
  const measured = snapshot.status?.budgets.find(value => value.sessionId === sessionId)
  const provider = snapshot.status?.selected?.provider
  const selectedModel = snapshot.status?.selected?.model
  const globalPrompt = snapshot.status?.globalPrompt.text
  const previewKey = JSON.stringify([workspaceId, sessionId, provider, selectedModel, globalPrompt])
  const preview = previewResult?.key === previewKey ? previewResult.value : undefined
  useEffect(() => {
    let current = true
    setPreview(undefined); setPreviewError('')
    if (workspaceId !== undefined && measured === undefined && provider && selectedModel)
      void previewBudget({ workspaceId, ...(sessionId === null ? {} : { sessionId }), provider, model: selectedModel })
        .then((value) => { if (current) setPreview({ key: previewKey, value }) })
        .catch((error: unknown) => { if (current) setPreviewError(error instanceof Error ? error.message : String(error)) })
    return () => { current = false }
  }, [workspaceId, sessionId, provider, selectedModel, measured, previewBudget, previewKey])
  const budget = measured ?? preview
  const connection = (): RainyModelDiscovery => {
    if (!form.provider.trim() || !form.baseURL.trim()) throw new Error(t('settingsRequiredDiscovery'))
    return { provider: form.provider.trim(), baseURL: form.baseURL.trim(), api: form.api,
      ...(form.apiKey.trim() === '' ? {} : { apiKey: form.apiKey }) }
  }
  const setup = (): RainyModelSetup => {
    if (!form.provider.trim() || !form.baseURL.trim() || !form.model.trim() || !form.context.trim()) throw new Error(t('settingsRequiredModel'))
    return { ...connection(), model: form.model.trim(), contextWindow: Number(form.context),
      ...(form.output.trim() === '' ? {} : { maxTokens: Number(form.output) }), local: form.local, thinking: form.thinking,
      ...(form.api === 'openai-completions' ? { thinkingFormat: form.thinkingFormat, maxTokensField: form.maxTokensField } : {}) }
  }
  const field = (key: 'provider' | 'baseURL' | 'model' | 'context' | 'output' | 'apiKey', label: string, type = 'text') =>
    <label className={css.field}><span>{label}</span><input className={css.input} type={type} value={form[key]}
      autoComplete={key === 'apiKey' ? 'new-password' : 'off'} onChange={(event) => {
        setForm({ ...form, [key]: event.target.value, ...key === 'context' ? { contextEdited: true } : {} })
      }} /></label>
  const choice = (label: string, value: string, items: readonly string[], change: (value: string) => void, labels?: readonly string[]) =>
    <label className={css.field}><span>{label}</span><Choice label={label} value={value} disabled={busy}
      items={items.map((id, index) => ({ id, label: labels?.[index] ?? id }))} onChange={change} /></label>
  return <section className={css.section} data-rainy-settings="models">
    <h2 className={css.heading}>{t('settingsModels')}</h2>
    {snapshot.error !== '' && <p className={`${css.notice} ${css.error}`} role="alert">{snapshot.error}</p>}
    <article className={css.card} data-rainy-default-model>
      <h3 className={css.subheading}>{t('settingsDefaultModel')}</h3>
      <label className={css.field}><span>{t('settingsSavedModels')}</span><Choice label={t('settingsSavedModels')} value={selected} disabled={busy}
        items={[{ id: '', label: t('settingsAddModel') }, ...snapshot.status?.models.map(model => ({ id: `${model.provider}/${model.model}`, label: `${model.provider}/${model.model}` })) ?? []]}
        onChange={(value) => {
          setSelected(value); setResult('')
          const saved = snapshot.status?.models.find(model => `${model.provider}/${model.model}` === value)
          setForm(saved === undefined ? emptyModelForm(localModelContextWindow) : formOf(saved))
        }} /></label>
      <div className={css.grid}>
        {choice(t('settingsRuntimeKind'), form.local ? 'local' : 'api', ['local', 'api'], (value) => {
          const local = value === 'local'
          setForm({ ...form, local, api: local ? 'openai-completions' : 'openai-responses',
            context: form.contextEdited ? form.context : String(local ? localModelContextWindow : apiModelContextWindow) })
        }, [t('settingsLocalModel'), t('settingsApiModel')])}
        {field('provider', t('settingsProvider'))}
        <div className={css.wide}>{field('baseURL', t('settingsBaseUrl'))}</div>
        <div className={css.wide}>{field('model', t('settingsModelId'))}</div>
        {field('context', t('settingsContext'), 'number')}{field('output', t('settingsOutput'), 'number')}
        <div className={css.wide}>{field('apiKey', t('settingsApiKey'), 'password')}</div>
      </div>
      <details><summary>{t('settingsCompatibility')}</summary><div className={`${css.grid} ${css.disclosed}`}>
        {choice(t('settingsProtocol'), form.api, ['openai-completions', 'openai-responses', 'anthropic-messages'], (value) => { setForm({ ...form, api: value as ModelForm['api'] }) })}
        {choice(t('settingsThinking'), form.thinking, ['off', 'low', 'high', 'max'], (value) => { setForm({ ...form, thinking: value as ModelForm['thinking'] }) }, [t('settingsThinkingOff'), 'low', 'high', 'max'])}
        {form.api === 'openai-completions' && <>
          {choice(t('settingsThinkingFormat'), form.thinkingFormat, ['openai', 'deepseek', 'qwen'], (value) => { setForm({ ...form, thinkingFormat: value as ModelForm['thinkingFormat'] }) })}
          {choice(t('settingsOutputField'), form.maxTokensField, ['max_tokens', 'max_completion_tokens'], (value) => { setForm({ ...form, maxTokensField: value as ModelForm['maxTokensField'] }) })}
        </>}
      </div></details>
      <div className={css.actions}>
        <Button variant="primary" disabled={busy} onClick={() => { run(async () => {
          const saved = await configure(setup()); setSelected(`${saved.provider}/${saved.model}`); setForm(current => ({ ...current, apiKey: '' }))
        }, t('settingsSaved')) }}>{t('settingsSaveModel')}</Button>
        <Button variant="outline" disabled={busy} onClick={() => { run(async () => {
          const found = await discover(connection()); setResult(found.length === 0 ? t('settingsNoModels') : found.map(model => model.id).join('\n'))
        }) }}>{t('settingsDiscoverModels')}</Button>
        <Button variant="outline" disabled={busy} onClick={() => { run(async () => {
          const tested = await probe(setup()); setResult(t('settingsProbeResult', { stream: t(tested.stream ? 'settingsPass' : 'settingsNotVerified'), tools: t(tested.toolCall ? 'settingsPass' : 'settingsNotVerified') }))
        }) }}>{t('settingsProbeModel')}</Button>
        {snapshot.status?.presets?.map(preset => <Button key={preset.name} disabled={busy} onClick={() => {
          setSelected(''); setForm(formOf(preset.model))
        }}>{t('settingsPreset', { name: preset.name })}</Button>)}
      </div>
      {result !== '' && <p className={css.notice} role="status">{result}</p>}
    </article>
    <GlobalPromptSettings saved={snapshot.status?.globalPrompt} busy={busy} run={run} saveGlobalPrompt={saveGlobalPrompt} t={t} />
    <article className={css.card} data-rainy-budget>
      <h3 className={css.subheading}>{t('settingsContextBudget')}</h3>
      <p className={css.notice}>{budget === undefined ? t('settingsEmptyBudget') : t('settingsBudget', {
        tokens: budget.tokens.toLocaleString(), window: budget.contextWindow.toLocaleString(), kind: t(budget.kind === 'exact' ? 'settingsExact' : 'settingsEstimated'),
        input: budget.inputLimit.toLocaleString(), output: budget.outputTokens.toLocaleString(),
        margin: budget.marginTokens.toLocaleString(),
      })}{budget?.compacting ? `\n${t('settingsCompacting')}` : ''}</p>
      {budget?.breakdown !== undefined && <p className={css.muted}>{t('settingsBudgetParts', budget.breakdown)}</p>}
      {budget?.error !== undefined && <p className={`${css.notice} ${css.error}`} role="alert">{budget.error}</p>}
      {measured === undefined && preview !== undefined && <p className={css.muted}>{t('settingsPreviewBudget')}
        {preview.limitations.map(code => <span key={code}> {t(code === 'before-dispatch-estimate' ? 'settingsPreviewDispatch' : 'settingsPreviewAttachments')}</span>)}</p>}
      {previewError !== '' && <p className={`${css.notice} ${css.error}`} role="alert">{previewError}</p>}
    </article>
    <StrataSettings snapshot={strata} t={t} strataRefresh={strataRefresh} strataSave={strataSave} strataStart={strataStart}
      strataStop={strataStop} strataChoose={strataChoose} strataConnect={async () => {
        const saved = await strataConnect()
        if (saved !== undefined) { setSelected(`${saved.provider}/${saved.model}`); setForm(formOf(saved)); setResult('') }
        return saved
      }} />
  </section>
}

/** Existing explicitly selected session extensions inside the unified settings shell.
 * @param props Host callbacks and selected project state.
 * @returns The extension editor with the existing enablement semantics.
 */
export function ExtensionsSection({ useSettings, useIde, refresh, catalog, extensions, notify, t }: SettingsSectionProps) {
  const status = useSettings(value => value.status)
  const currentSession = useIde(value => value.data.lastSessionId)
  const [sessionId, setSessionId] = useState(currentSession ?? '')
  const [selection, setSelection] = useState('')
  const [skills, setSkills] = useState<readonly { id: string }[]>([])
  const [loaded, setLoaded] = useState<string | undefined>()
  const { busy, run } = useSettingsAction(notify)
  useEffect(() => { void refresh() }, [refresh])
  const load = (): void => { run(async () => {
    const result = await catalog(sessionId)
    setSelection(JSON.stringify(result.selection, null, 2)); setSkills(result.skills); setLoaded(sessionId)
  }) }
  return <section className={css.section} data-rainy-settings="extensions">
    <h2 className={css.heading}>{t('extensions')}</h2><p className={css.muted}>{t('settingsExtensionNote')}</p>
    {(status?.sessions.length ?? 0) === 0 ? <p className={css.notice}>{t('settingsNoSession')}</p> : <>
      <label className={css.field}><span>{t('settingsSession')}</span><Choice label={t('settingsSession')} value={sessionId} disabled={busy}
        items={status?.sessions.map(session => ({ id: session.id, label: session.title || session.id })) ?? []}
        onChange={(id) => { setSessionId(id); setLoaded(undefined); setSelection(''); setSkills([]) }} /></label>
      <div className={css.actions}><Button variant="outline" disabled={busy || sessionId === ''} onClick={load}>{t('settingsExtensionRead')}</Button></div>
      {loaded === sessionId && <>
        <p className={css.muted}>{skills.length === 0 ? t('settingsNoSkills') : skills.map(skill => skill.id).join(' · ')}</p>
        <label className={css.field}><span>{t('settingsExtensionSelection')}</span><textarea className={css.textarea} value={selection}
          spellCheck={false} onChange={(event) => { setSelection(event.target.value) }} /></label>
        <div className={css.actions}><Button variant="primary" disabled={busy} onClick={() => { run(async () => {
          const parsed: unknown = JSON.parse(selection); await extensions(sessionId, parsed)
        }, t('settingsApplied')) }}>{t('settingsApplyExtensions')}</Button></div>
      </>}
    </>}
  </section>
}

/** Consistent label and control arrangement for project setting sections.
 * @param props Localized label and form control.
 * @returns One settings field.
 */
export function SettingsField({ label, children }: { label: string; children: ReactNode }) {
  return <label className={css.field}><span>{label}</span>{children}</label>
}
