/** Models section: saved models and the default, the model form, global prompt, context budget and the Strata engine. */
import { useEffect, useId, useRef, useState } from 'react'
import clsx from 'clsx'
import { DEFAULT_CONFIG } from '../../shared/config.ts'
import type {
  BudgetPreview, ModelApi, ModelDiscoveryInput, ModelSetup, ModelSetupInput, ModelsStatus, ThinkingLevel,
} from '../../shared/rpc.ts'
import type { Translate } from '../i18n.ts'
import { host } from '../rpc.ts'
import { Button } from '../ui/Button.tsx'
import { Pill } from '../ui/Pill.tsx'
import { SegmentedControl } from '../ui/SegmentedControl.tsx'
import { Tag } from '../ui/Tag.tsx'
import { toast } from '../ui/toasts.tsx'
import { GlobalPromptSettings } from './GlobalPromptSettings.tsx'
import { t as lookup, useT } from './messages.ts'
import type { SettingsMessage } from './messages.ts'
import { useModelsStatus } from './models-status.ts'
import { strataBridge } from './native.ts'
import { Field, Loading, Notice, Select, errorText, useAction } from './parts.tsx'
import type { SettingsWorkspace } from './parts.tsx'
import { StrataSettings } from './StrataSettings.tsx'
import { StrataController, useStrata } from './strata-controller.ts'
import css from './sections.module.css'

/** Unsaved model form fields; numbers stay text until submission. */
export interface ModelForm {
  provider: string
  baseURL: string
  model: string
  context: string
  /** Whether the context window was typed or loaded, so switching local/API keeps it. */
  contextEdited: boolean
  output: string
  apiKey: string
  local: boolean
  api: ModelApi
  thinking: ThinkingLevel
  thinkingFormat: NonNullable<ModelSetup['thinkingFormat']>
  maxTokensField: NonNullable<ModelSetup['maxTokensField']>
}

const apis: readonly ModelApi[] = ['openai-completions', 'openai-responses', 'anthropic-messages']
const thinkingLevels: readonly ThinkingLevel[] = ['off', 'low', 'high', 'max']
const thinkingFormats: readonly ModelForm['thinkingFormat'][] = ['openai', 'deepseek', 'qwen']
const maxTokensFields: readonly ModelForm['maxTokensField'][] = ['max_tokens', 'max_completion_tokens']

/**
 * @param contextWindow Initial context window of a new local model.
 * @returns An empty form for a new local model with automatic output allocation.
 */
export function emptyModelForm(contextWindow: number): ModelForm {
  return { provider: '', baseURL: '', model: '', context: String(contextWindow), contextEdited: false, output: '', apiKey: '', local: true,
    api: 'openai-completions', thinking: 'off', thinkingFormat: 'openai', maxTokensField: 'max_tokens' }
}

function formOf(model: ModelSetup): ModelForm {
  return { provider: model.provider, baseURL: model.baseURL, model: model.model, context: String(model.contextWindow), contextEdited: true,
    output: model.maxTokens === undefined ? '' : String(model.maxTokens), apiKey: '', local: model.local,
    api: model.api ?? (model.local ? 'openai-completions' : 'openai-responses'), thinking: model.thinking ?? 'off',
    thinkingFormat: model.thinkingFormat ?? 'openai', maxTokensField: model.maxTokensField ?? 'max_tokens' }
}

const keyOf = (model: { provider: string; model: string }): string => `${model.provider}/${model.model}`

function positiveInteger(text: string): number | undefined {
  const value = Number(text.trim())
  return Number.isInteger(value) && value > 0 ? value : undefined
}

function workbench(): { local: number; api: number; pollMs: number } {
  const config = typeof window === 'undefined' ? undefined : window.__RAINY_WORKBENCH_CONFIG__
  return {
    local: config?.localModelContextWindow ?? DEFAULT_CONFIG.localModelContextWindow,
    api: config?.apiModelContextWindow ?? DEFAULT_CONFIG.apiModelContextWindow,
    pollMs: config?.editorPollMs ?? DEFAULT_CONFIG.editorPollMs,
  }
}

type Result =
  | { kind: 'discover'; models: { id: string; contextWindow?: number | undefined }[] }
  | { kind: 'probe'; stream: boolean; toolCall: boolean; text?: string | undefined }

/**
 * Default model, model form, global prompt, context budget and the optional local engine, in order of everyday use.
 * @param props.workspace Project whose context budget is previewed.
 * @returns The section.
 */
export function ModelsSection({ workspace }: { workspace: SettingsWorkspace }): JSX.Element {
  const t = useT()
  const { status, error, refresh, accept } = useModelsStatus()
  const [config] = useState(workbench)
  const action = useAction()
  const { busy, run } = action
  const [form, setForm] = useState(() => emptyModelForm(config.local))
  const [editing, setEditing] = useState<string | undefined>()
  const [result, setResult] = useState<Result | undefined>()
  const [removing, setRemoving] = useState<string | undefined>()
  const [strata, setStrata] = useState<StrataController | undefined>()
  const initialized = useRef(false)
  const kindId = useId()

  useEffect(() => {
    if (initialized.current || status === undefined) return
    initialized.current = true
    const selected = status.selected
    const saved = selected === null ? undefined : status.models.find(model => keyOf(model) === keyOf(selected))
    if (saved !== undefined) { setForm(formOf(saved)); setEditing(keyOf(saved)) }
  }, [status])

  useEffect(() => {
    const controller = new StrataController(strataBridge(), {
      saved: () => lookup('strataSaved'), stopped: () => lookup('strataStopped'), connected: () => lookup('strataConnected'),
    }, (message, success) => { toast(message, success === true ? { tone: 'success' } : {}) }, async (selection) => {
      const latest = await refresh()
      const model = latest?.models.find(value => keyOf(value) === keyOf(selection))
      if (model === undefined) throw new Error(lookup('strataSelectionUnavailable'))
      return model
    })
    setStrata(controller)
    void controller.refresh()
    const timer = setInterval(() => { if (!document.hidden) void controller.refresh() }, config.pollMs)
    return () => { clearInterval(timer); controller.dispose() }
  }, [refresh, config.pollMs])

  const load = (next: ModelForm, key: string | undefined): void => {
    setForm(next); setEditing(key); setResult(undefined); setRemoving(undefined)
  }
  const connection = (): ModelDiscoveryInput => {
    if (!form.provider.trim() || !form.baseURL.trim()) throw new Error(t('settingsRequiredDiscovery'))
    return { provider: form.provider.trim(), baseURL: form.baseURL.trim(), api: form.api,
      ...(form.apiKey.trim() === '' ? {} : { apiKey: form.apiKey }) }
  }
  const setup = (): ModelSetupInput => {
    if (!form.provider.trim() || !form.baseURL.trim() || !form.model.trim() || !form.context.trim()) throw new Error(t('settingsRequiredModel'))
    const contextWindow = positiveInteger(form.context)
    const maxTokens = form.output.trim() === '' ? undefined : positiveInteger(form.output)
    if (contextWindow === undefined || (form.output.trim() !== '' && maxTokens === undefined)) throw new Error(t('settingsInvalidNumber'))
    return { ...connection(), model: form.model.trim(), contextWindow, ...(maxTokens === undefined ? {} : { maxTokens }),
      local: form.local, thinking: form.thinking,
      ...(form.api === 'openai-completions' ? { thinkingFormat: form.thinkingFormat, maxTokensField: form.maxTokensField } : {}) }
  }
  const select = (model: ModelSetup, thinking?: ThinkingLevel): void => {
    setRemoving(undefined)
    run(async () => {
      accept(await host.call('models.select', { provider: model.provider, model: model.model, ...(thinking === undefined ? {} : { thinking }) }))
    }, t('settingsApplied'))
  }
  const remove = (model: ModelSetup): void => {
    const key = keyOf(model)
    if (removing !== key) { setRemoving(key); return }
    setRemoving(undefined)
    run(async () => {
      accept(await host.call('models.remove', { provider: model.provider }))
      if (editing === key) load(emptyModelForm(config.local), undefined)
    }, t('modelRemoved', { name: key }))
  }
  const field = (key: 'provider' | 'baseURL' | 'model' | 'context' | 'output' | 'apiKey', label: string, options: { type?: string; wide?: boolean } = {}) =>
    <Field label={label} wide={options.wide}><input className={css.input} aria-label={label} type={options.type ?? 'text'} value={form[key]}
      autoComplete={key === 'apiKey' ? 'new-password' : 'off'} spellCheck={false}
      onChange={(event) => {
        const value = event.target.value
        setForm(current => ({ ...current, [key]: value, ...key === 'context' ? { contextEdited: true } : {} }))
      }} /></Field>
  const choice = <V extends string>(label: string, value: V, items: readonly V[], change: (value: V) => void, labels?: Partial<Record<V, string>>) =>
    <Field label={label}><Select label={label} value={value} disabled={busy}
      items={items.map(id => ({ id, label: labels?.[id] ?? id }))} onChange={change} /></Field>
  const thinkingLabels: Partial<Record<ThinkingLevel, string>> = { off: t('settingsThinkingOff') }
  const keyStored = status?.credentials.includes(form.provider.trim()) === true

  return <section className={css.section} data-settings-section="models">
    <h2 className={css.heading}>{t('settingsModels')}</h2>
    {error !== '' && <Notice tone="error">{error}</Notice>}
    <article className={css.card} data-default-model>
      <div className={css.row}>
        <h3 className={css.subheading}>{t('settingsDefaultModel')}</h3>
        <Button size="sm" variant="outline" disabled={busy} onClick={() => { load(emptyModelForm(config.local), undefined) }}>{t('settingsAddModel')}</Button>
      </div>
      {status === undefined ? error === '' && <Loading label={t('loading')} />
        : status.models.length === 0 ? <p className={css.muted}>{t('modelsEmpty')}</p>
          : <ul className={css.items}>{status.models.map((model) => {
            const key = keyOf(model)
            const isDefault = status.selected !== null && keyOf(status.selected) === key
            return <li key={key} className={clsx(css.item, editing === key && css.itemCurrent)} data-model={key}>
              <div className={css.itemText}>
                <div className={css.itemTitle}>
                  <span>{key}</span>
                  {isDefault && <Tag tone="success">{t('modelDefault')}</Tag>}
                  {status.credentials.includes(model.provider) && <Tag tone="neutral">{t('modelKeyStored')}</Tag>}
                </div>
                <div className={css.muted}>{t('modelContext', {
                  kind: t(model.local ? 'settingsLocalModel' : 'settingsApiModel'), context: model.contextWindow.toLocaleString('en-US'),
                })}</div>
              </div>
              <div className={css.actions}>
                {isDefault
                  ? <Select label={t('modelThinkingFor', { name: key })} value={status.selected?.thinking ?? model.thinking ?? 'off'} disabled={busy}
                    items={(status.thinkingLevels[model.provider] ?? thinkingLevels).map(id => ({ id, label: thinkingLabels[id] ?? id }))} onChange={(thinking) => { select(model, thinking) }} />
                  : <Button size="sm" variant="outline" disabled={busy} onClick={() => { select(model) }}>{t('modelUseDefault')}</Button>}
                <Button size="sm" disabled={busy} onClick={() => { load(formOf(model), key) }}>{t('modelEdit')}</Button>
                <Button size="sm" disabled={busy} onClick={() => { remove(model) }}>{t(removing === key ? 'modelRemoveConfirm' : 'modelRemove')}</Button>
              </div>
            </li>
          })}</ul>}
    </article>
    <article className={css.card} data-model-form>
      <h3 className={css.subheading}>{editing === undefined ? t('settingsAddModel') : t('modelEditTitle', { name: editing })}</h3>
      {status !== undefined && status.presets.length > 0 && <div className={css.actions}>
        {status.presets.map(preset => <Button key={preset.name} size="sm" disabled={busy}
          onClick={() => { load(formOf(preset.model), undefined) }}>{t('settingsPreset', { name: preset.name })}</Button>)}
      </div>}
      <SegmentedControl id={kindId} label={t('settingsRuntimeKind')} value={form.local ? 'local' : 'api'} disabled={busy}
        options={[{ value: 'local', label: t('settingsLocalModel') }, { value: 'api', label: t('settingsApiModel') }]}
        onChange={(value) => {
          const local = value === 'local'
          setForm(current => ({ ...current, local, api: local ? 'openai-completions' : 'openai-responses',
            context: current.contextEdited ? current.context : String(local ? config.local : config.api) }))
        }} />
      <div className={css.grid}>
        {field('provider', t('settingsProvider'))}
        {field('model', t('settingsModelId'))}
        {field('baseURL', t('settingsBaseUrl'), { wide: true })}
        {field('context', t('settingsContext'), { type: 'number' })}
        {field('output', t('settingsOutput'), { type: 'number' })}
        {field('apiKey', t(keyStored ? 'settingsApiKeyStored' : 'settingsApiKey'), { type: 'password', wide: true })}
      </div>
      <details className={css.disclosure}><summary>{t('settingsCompatibility')}</summary><div className={clsx(css.grid, css.disclosed)}>
        {choice(t('settingsProtocol'), form.api, apis, (api) => { setForm(current => ({ ...current, api })) })}
        {choice(t('settingsThinking'), form.thinking, thinkingLevels, (thinking) => { setForm(current => ({ ...current, thinking })) }, thinkingLabels)}
        {form.api === 'openai-completions' && <>
          {choice(t('settingsThinkingFormat'), form.thinkingFormat, thinkingFormats, (thinkingFormat) => { setForm(current => ({ ...current, thinkingFormat })) })}
          {choice(t('settingsOutputField'), form.maxTokensField, maxTokensFields, (maxTokensField) => { setForm(current => ({ ...current, maxTokensField })) })}
        </>}
      </div></details>
      <div className={css.actions}>
        <Button variant="primary" disabled={busy} onClick={() => { run(async () => {
          const saved = await host.call('models.configure', setup())
          setEditing(keyOf(saved)); setForm(current => ({ ...current, apiKey: '' })); setResult(undefined)
          await refresh()
        }, t('settingsSaved')) }}>{t('settingsSaveModel')}</Button>
        <Button variant="outline" disabled={busy} onClick={() => { run(async () => {
          setResult({ kind: 'discover', models: await host.call('models.discover', connection()) })
        }) }}>{t('settingsDiscoverModels')}</Button>
        <Button variant="outline" disabled={busy} onClick={() => { run(async () => {
          setResult({ kind: 'probe', ...await host.call('models.probe', setup()) })
        }) }}>{t('settingsProbeModel')}</Button>
      </div>
      {result !== undefined && <ModelResult result={result} form={form} t={t} choose={(model) => {
        setForm(current => ({ ...current, model: model.id,
          ...model.contextWindow !== undefined && !current.contextEdited ? { context: String(model.contextWindow) } : {} }))
      }} />}
    </article>
    <GlobalPromptSettings saved={status?.globalPrompt} action={action} accept={accept} />
    <BudgetCard workspace={workspace} status={status} />
    {strata !== undefined && <StrataCard controller={strata} onConnected={(saved) => { load(formOf(saved), keyOf(saved)) }} />}
  </section>
}

function ModelResult({ result, form, t, choose }: {
  result: Result
  form: ModelForm
  t: Translate<SettingsMessage>
  choose(model: { id: string; contextWindow?: number | undefined }): void
}): JSX.Element {
  if (result.kind === 'probe') {
    const verdict = (passed: boolean): string => t(passed ? 'settingsPass' : 'settingsNotVerified')
    return <Notice tone="status">{t('settingsProbeResult', { stream: verdict(result.stream), tools: verdict(result.toolCall) })}
      {result.text !== undefined && result.text !== '' ? `\n${t('settingsProbeReply', { text: result.text })}` : ''}</Notice>
  }
  if (result.models.length === 0) return <Notice tone="status">{t('settingsNoModels')}</Notice>
  return <div className={css.notice} role="status">
    <div>{t('settingsDiscovered', { count: result.models.length })}</div>
    <div className={css.pills}>{result.models.map(model => <Pill key={model.id} active={form.model === model.id}
      onClick={() => { choose(model) }}>{model.id}</Pill>)}</div>
  </div>
}

function StrataCard({ controller, onConnected }: { controller: StrataController; onConnected(model: ModelSetup): void }): JSX.Element {
  const snapshot = useStrata(controller)
  return <StrataSettings snapshot={snapshot} actions={{ ...controller.actions, strataConnect: async () => {
    const saved = await controller.actions.strataConnect()
    if (saved !== undefined) onConnected(saved)
    return saved
  } }} />
}

/**
 * Estimate of the next request in the current project, read again when the default model or global prompt changes.
 * @param props.workspace Current project.
 * @param props.status Model settings.
 * @returns The budget card.
 */
function BudgetCard({ workspace, status }: { workspace: SettingsWorkspace; status: ModelsStatus | undefined }): JSX.Element {
  const t = useT()
  const [preview, setPreview] = useState<{ key: string; value?: BudgetPreview; error?: string }>()
  const [reads, setReads] = useState(0)
  const selected = status?.selected ?? null
  const workspaceId = workspace?.workspaceId
  const hasModel = selected !== null && selected.provider !== '' && selected.model !== ''
  const model = selected === null ? undefined : status?.models.find(value => keyOf(value) === keyOf(selected))
  const key = JSON.stringify([workspaceId, selected, model, status?.globalPrompt.text, reads])
  useEffect(() => {
    if (workspaceId === undefined || !hasModel) return undefined
    let current = true
    host.call('budget.preview', { workspaceId }).then((value) => { if (current) setPreview({ key, value }) },
      (failure: unknown) => { if (current) setPreview({ key, error: errorText(failure) }) })
    return () => { current = false }
  }, [workspaceId, hasModel, key])
  const shown = preview?.key === key ? preview : undefined
  const budget = shown?.value
  const format = (value: number): string => value.toLocaleString('en-US')
  return <article className={css.card} data-budget>
    <div className={css.row}>
      <h3 className={css.subheading}>{t('settingsContextBudget')}</h3>
      {workspaceId !== undefined && hasModel && <Button size="sm" variant="outline" onClick={() => { setReads(value => value + 1) }}>{t('settingsRefresh')}</Button>}
    </div>
    {workspaceId === undefined ? <Notice>{t('settingsOpenProject')}</Notice>
      : !hasModel ? <Notice>{t('settingsEmptyBudget')}</Notice>
        : shown?.error !== undefined ? <Notice tone="error">{shown.error}</Notice>
          : budget === undefined ? <Loading label={t('loading')} />
            : <>
              <Notice>{t('settingsBudget', {
                tokens: format(budget.tokens), window: format(budget.contextWindow), kind: t(budget.kind === 'exact' ? 'settingsExact' : 'settingsEstimated'),
                input: format(budget.inputLimit), output: format(budget.outputTokens), margin: format(budget.marginTokens),
              })}</Notice>
              <div className={css.meter} aria-hidden="true">
                <span style={{ width: `${budget.inputLimit > 0 ? Math.min(100, budget.tokens * 100 / budget.inputLimit) : 0}%` }} />
              </div>
              <p className={css.muted}>{t('settingsBudgetParts', Object.fromEntries(
                Object.entries(budget.breakdown).map(([part, tokens]) => [part, format(tokens)])))}</p>
              <p className={css.muted}><span>{t('settingsPreviewBudget')}</span> <span>{t('settingsPreviewDispatch')}</span>
                {' '}<span>{t('settingsPreviewAttachments')}</span></p>
            </>}
  </article>
}
