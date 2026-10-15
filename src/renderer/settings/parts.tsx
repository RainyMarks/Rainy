/** Controls and helpers shared by the settings sections. */
import { useCallback, useEffect, useRef, useState } from 'react'
import type { ReactNode } from 'react'
import clsx from 'clsx'
import type { WorkspaceId } from '../../shared/ide-files-protocol.ts'
import { Button } from '../ui/Button.tsx'
import { Menu } from '../ui/Menu.tsx'
import { IconChevronDownOutlineRegular, IconLoadingOutlineRegular } from '../ui/icons/index.tsx'
import { toast } from '../ui/toasts.tsx'
import css from './sections.module.css'

/** The project Settings works on; `null` before a folder is opened. */
export type SettingsWorkspace = { workspaceId: WorkspaceId; path: string; title: string } | null

/**
 * @param error A rejection value.
 * @returns Its message text.
 */
export function errorText(error: unknown): string {
  // Electron prefixes errors thrown by the desktop process; the reason after it is already written for the user.
  return (error instanceof Error ? error.message : String(error)).replace(/^Error invoking remote method '[^']+': (?:Error: )?/u, '')
}

/** One operation at a time per section; success shows as a toast, a failure stays beside the control that started it. */
export interface SettingsAction {
  /** Whether an operation is running. */
  busy: boolean
  /** The latest failure and the place that started it; cleared when the next operation starts. */
  failure: { place: string | undefined; text: string } | undefined
  /**
   * Run an operation unless another one is running.
   * @param operation The work.
   * @param success Toast text after it resolves.
   * @param place Where {@link ActionFailure} shows a rejection; without one it becomes an error toast.
   */
  run(operation: () => Promise<void>, success?: string, place?: string): void
}

/** @returns The section's operation runner. */
export function useAction(): SettingsAction {
  const [busy, setBusy] = useState(false)
  const [failure, setFailure] = useState<SettingsAction['failure']>()
  const pending = useRef(false)
  const mounted = useRef(true)
  useEffect(() => { mounted.current = true; return () => { mounted.current = false } }, [])
  const run = useCallback((operation: () => Promise<void>, success?: string, place?: string): void => {
    if (pending.current) return
    pending.current = true
    setBusy(true)
    setFailure(undefined)
    void operation().then(() => { if (success !== undefined) toast(success, { tone: 'success' }) })
      .catch((error: unknown) => {
        const text = errorText(error)
        if (place === undefined || !mounted.current) toast(text, { tone: 'error' })
        else setFailure({ place, text })
      })
      .finally(() => { pending.current = false; if (mounted.current) setBusy(false) })
  }, [])
  return { busy, failure, run }
}

/**
 * The failure of an operation started at `place`, shown until the next operation starts.
 * @param props.action The section's runner.
 * @param props.place The place name passed to `run`.
 * @returns The error notice, or nothing.
 */
export function ActionFailure({ action, place }: { action: SettingsAction; place: string }): JSX.Element | null {
  return action.failure?.place === place ? <Notice tone="error">{action.failure.text}</Notice> : null
}

/** One option of a {@link Select}. */
export interface SelectItem<V extends string> {
  id: V
  label: string
  disabled?: boolean | undefined
  /** CSS font family the label is shown in, such as the font the option names. */
  font?: string | undefined
}

/**
 * A dropdown choice built on the shared menu.
 * @param props.label Accessible name of the trigger.
 * @param props.value Selected option id.
 * @param props.items Options in display order.
 * @param props.onChange Receives the chosen id.
 * @param props.disabled Whether the trigger is disabled.
 * @param props.className Extra class of the wrapper, such as a fixed width.
 * @returns The trigger and its menu.
 */
export function Select<V extends string>({ label, value, items, onChange, disabled = false, className }: {
  label: string
  value: V
  items: readonly SelectItem<V>[]
  onChange(value: V): void
  disabled?: boolean | undefined
  className?: string | undefined
}): JSX.Element {
  const [open, setOpen] = useState(false)
  const chosen = items.find(item => item.id === value)
  const labelled = (item: SelectItem<V>): ReactNode => item.font === undefined ? item.label : <span style={{ fontFamily: item.font }}>{item.label}</span>
  return <div className={clsx(css.select, className)}>
    <Menu open={open} onClose={() => { setOpen(false) }} portal compact selectedId={value}
      anchor={<Button variant="outline" className={css.selectButton} aria-label={label} aria-haspopup="menu"
        aria-expanded={open} disabled={disabled} onClick={() => { setOpen(!open) }}>
        <span style={chosen?.font === undefined ? undefined : { fontFamily: chosen.font }}>{chosen?.label ?? value}</span>
        <IconChevronDownOutlineRegular size={14} />
      </Button>}
      items={items.map(item => ({ id: item.id, label: labelled(item), disabled: item.disabled === true }))}
      onSelect={(id) => {
        setOpen(false)
        const picked = items.find(item => item.id === id)
        if (picked !== undefined) onChange(picked.id)
      }} />
  </div>
}

/** One option of a {@link Choice}. */
export interface ChoiceItem<V extends string> { id: V; label: string }

/**
 * A segmented choice among two or three options; the option buttons report the current value as their pressed state.
 * @param props.label Accessible name of the group.
 * @param props.value Selected option id.
 * @param props.items Options in display order.
 * @param props.onChange Receives a newly chosen id; choosing the current option does nothing.
 * @param props.disabled Whether every option is disabled.
 * @returns The group.
 */
export function Choice<V extends string>({ label, value, items, onChange, disabled = false }: {
  label: string
  value: V
  items: readonly ChoiceItem<V>[]
  onChange(value: V): void
  disabled?: boolean | undefined
}): JSX.Element {
  return <div className={css.choices} role="group" aria-label={label}>
    {items.map(item => <button key={item.id} type="button" className={css.choice} aria-pressed={item.id === value} disabled={disabled}
      onClick={() => { if (item.id !== value) onChange(item.id) }}>{item.label}</button>)}
  </div>
}

/**
 * A labelled form control.
 * @param props.label Visible label.
 * @param props.wide Whether the field spans both grid columns.
 * @param props.children The control.
 * @returns The field.
 */
export function Field({ label, wide = false, children }: { label: string; wide?: boolean; children: ReactNode }): JSX.Element {
  return <label className={clsx(css.field, wide && css.wide)}><span>{label}</span>{children}</label>
}

/**
 * A settings row: title and description on the left, the control on the right.
 * @param props.title Row title.
 * @param props.description Optional supporting sentence.
 * @param props.children The control.
 * @returns The row.
 */
export function SettingRow({ title, description, children }: { title: string; description?: string | undefined; children?: ReactNode }): JSX.Element {
  return <div className={css.settingRow}>
    <div className={css.settingText}>
      <div className={css.settingTitle}>{title}</div>
      {description !== undefined && <div className={css.settingDescription}>{description}</div>}
    </div>
    {children}
  </div>
}

/**
 * A text block for status, hints and failures.
 * @param props.tone `error` marks the text as an alert, `status` as a live region.
 * @param props.children Text.
 * @returns The notice.
 */
export function Notice({ tone, children }: { tone?: 'error' | 'status' | undefined; children: ReactNode }): JSX.Element {
  return <p className={clsx(css.notice, tone === 'error' && css.error)}
    role={tone === 'error' ? 'alert' : tone === 'status' ? 'status' : undefined}>{children}</p>
}

/**
 * Centered spinner for a section that is still reading its data.
 * @param props.label Accessible name.
 * @returns The spinner.
 */
export function Loading({ label }: { label: string }): JSX.Element {
  return <div className={css.spinner} role="status" aria-label={label}><IconLoadingOutlineRegular size={20} /></div>
}

/**
 * Format a byte count for download sizes.
 * @param bytes Size in bytes.
 * @returns Text such as `36MB` or `1.5GB`.
 */
export function fileSizeText(bytes: number): string {
  if (bytes < 1024) return `${bytes}B`
  const kb = bytes / 1024
  if (kb < 1024) return `${kb < 10 ? kb.toFixed(1) : Math.round(kb)}KB`
  const mb = kb / 1024
  if (mb < 1024) return `${mb < 10 ? mb.toFixed(1) : Math.round(mb)}MB`
  const gb = mb / 1024
  return `${gb < 10 ? gb.toFixed(1) : Math.round(gb)}GB`
}
