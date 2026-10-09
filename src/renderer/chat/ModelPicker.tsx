/** Composer controls for the chat's model and reasoning level, and the context-window meter. */
import { useEffect, useState } from 'react'
import type { ContextUsage, ModelSelection, ModelsStatus, ThinkingLevel } from '../../shared/rpc.ts'
import { emit } from '../app/bus.ts'
import { host } from '../rpc.ts'
import { IconChevronDownOutlineRegular, IconSettingsOutlineRegular, Menu, toast, Tooltip, type MenuEntry } from '../ui/index.ts'
import { useChatT } from './messages.ts'
import { formatTokens } from './Transcript.tsx'
import css from './Composer.module.css'

let cached: ModelsStatus | undefined
const subscribers = new Set<(status: ModelsStatus) => void>()
host.on('models.changed', (status) => { cached = status; for (const subscriber of subscribers) subscriber(status) })

/** @returns Saved models, kept current from `models.changed`. */
export function useModels(): ModelsStatus | undefined {
  const [status, setStatus] = useState(cached)
  useEffect(() => {
    subscribers.add(setStatus)
    if (cached === undefined) {
      void host.call('models.status').then((next) => { cached ??= next; setStatus(cached) }, (error: unknown) => { console.error(error) })
    }
    return () => { subscribers.delete(setStatus) }
  }, [])
  return status
}

/**
 * Model and reasoning-level picker. With a chat it changes that chat; without one it changes the default for new chats.
 * @param props.sessionId Current chat, or `null` before the first message.
 * @param props.selection The chat's model, or `null` for the default.
 * @param props.disabled Whether changing is blocked.
 * @param props.openSignal Opens the menu whenever it changes to a non-zero value (the `/model` command).
 * @returns The picker.
 */
export function ModelPicker({ sessionId, selection, disabled = false, openSignal = 0 }: {
  sessionId: string | null
  selection: ModelSelection | null
  disabled?: boolean
  openSignal?: number
}): JSX.Element {
  const t = useChatT()
  const status = useModels()
  const [open, setOpen] = useState(false)
  useEffect(() => { if (openSignal !== 0) setOpen(true) }, [openSignal])
  const current = selection ?? status?.selected ?? null
  const setup = status?.models.find(model => model.provider === current?.provider && model.model === current?.model) ?? (current === null ? status?.models[0] : undefined)
  const levels = setup === undefined ? [] : status?.thinkingLevels[setup.provider] ?? ['off']
  const thinking = current?.thinking ?? setup?.thinking ?? 'off'
  const levelLabel: Record<ThinkingLevel, string> = { off: t('thinkingOff'), low: t('thinkingLow'), high: t('thinkingHigh'), max: t('thinkingMax') }

  const choose = (next: ModelSelection): void => {
    const call = sessionId === null ? host.call('models.select', next) : host.call('chat.setModel', { sessionId, ...next })
    call.catch((error: unknown) => { toast(error instanceof Error ? error.message : String(error)) })
  }

  const items: MenuEntry[] = []
  if (status !== undefined && status.models.length > 0) {
    items.push({ type: 'label', id: 'models', text: t('assistant') })
    for (const model of status.models) items.push({ id: `model:${model.provider}`, label: model.model })
    if (setup !== undefined && levels.length > 1) {
      items.push({ type: 'separator', id: 'sep-thinking' }, { type: 'label', id: 'thinking', text: t('thinking') })
      for (const level of levels) items.push({ id: `thinking:${level}`, label: levelLabel[level] })
    }
    items.push({ type: 'separator', id: 'sep-settings' })
  }
  items.push({ id: 'settings', label: t('configureModel'), icon: <IconSettingsOutlineRegular size={14} /> })

  const select = (id: string): void => {
    setOpen(false)
    if (id === 'settings') { emit('pane.show', { pane: 'settings', section: 'models' }); return }
    if (id.startsWith('model:')) {
      const model = status?.models.find(item => `model:${item.provider}` === id)
      if (model !== undefined) choose({ provider: model.provider, model: model.model, ...(model.thinking === undefined ? {} : { thinking: model.thinking }) })
      return
    }
    if (id.startsWith('thinking:') && setup !== undefined) choose({ provider: setup.provider, model: setup.model, thinking: id.slice(9) as ThinkingLevel })
  }

  const label = setup === undefined ? t('noModel') : levels.length > 1 ? `${setup.model} · ${levelLabel[thinking]}` : setup.model
  return (
    <Menu open={open} onClose={() => { setOpen(false) }} side="top" align="start" portal dense onSelect={select} items={items}
      selectedIds={setup === undefined ? [] : [`model:${setup.provider}`, `thinking:${thinking}`]}
      anchor={
        <button type="button" className={css.picker} disabled={disabled} aria-haspopup="menu" aria-expanded={open} onClick={() => { setOpen(value => !value) }}
          data-empty={setup === undefined || undefined}>
          <span className={css.pickerLabel}>{label}</span>
          <IconChevronDownOutlineRegular size={12} />
        </button>
      } />
  )
}

/**
 * Ring showing how full the context window is.
 * @param props.context Accounting for the next request, or `null` before the chat has a model.
 * @returns The meter, or nothing without accounting.
 */
export function ContextMeter({ context }: { context: ContextUsage | null }): JSX.Element | null {
  const t = useChatT()
  if (context === null || context.inputLimit <= 0) return null
  const ratio = Math.min(1, context.tokens / context.inputLimit)
  const percent = Math.round(ratio * 100)
  const radius = 7
  const circumference = 2 * Math.PI * radius
  const detail = [
    t('contextDetail', { tokens: formatTokens(context.tokens), window: formatTokens(context.inputLimit), kind: t(context.kind === 'exact' ? 'contextExact' : 'contextEstimated') }),
    `${t('contextSystem')} ${formatTokens(context.breakdown.system)} · ${t('contextTools')} ${formatTokens(context.breakdown.tools)} · ${t('contextMessages')} ${formatTokens(context.breakdown.messages)}`,
    t('contextCompactAt', { tokens: formatTokens(context.compactAt) }),
  ].join('\n')
  const level = context.tokens >= context.compactAt ? 'full' : ratio >= 0.7 ? 'high' : 'normal'
  return (
    <Tooltip label={detail} side="top" portal>
      <span className={css.meter} data-level={level} role="img" aria-label={t('contextUsage', { percent })}>
        <svg width="18" height="18" viewBox="0 0 18 18" aria-hidden="true">
          <circle cx="9" cy="9" r={radius} className={css.meterTrack} />
          <circle cx="9" cy="9" r={radius} className={css.meterValue} strokeDasharray={`${circumference * ratio} ${circumference}`} transform="rotate(-90 9 9)" />
        </svg>
        <span className={css.meterText}>{percent}%</span>
      </span>
    </Tooltip>
  )
}
