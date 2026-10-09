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
  return error instanceof Error ? error.message : String(error)
}

/** One operation at a time per section, with its outcome shown as a toast. */
export interface SettingsAction {
  /** Whether an operation is running. */
  busy: boolean
  /**
   * Run an operation unless another one is running.
   * @param operation The work; a rejection becomes an error toast.
   * @param success Toast text after it resolves.
   */
  run(operation: () => Promise<void>, success?: string): void
}

/** @returns The section's operation runner. */
export function useAction(): SettingsAction {
  const [busy, setBusy] = useState(false)
  const pending = useRef(false)
  const mounted = useRef(true)
  useEffect(() => { mounted.current = true; return () => { mounted.current = false } }, [])
  const run = useCallback((operation: () => Promise<void>, success?: string): void => {
    if (pending.current) return
    pending.current = true
    setBusy(true)
    void operation().then(() => { if (success !== undefined) toast(success, { tone: 'success' }) })
      .catch((error: unknown) => { toast(errorText(error)) })
      .finally(() => { pending.current = false; if (mounted.current) setBusy(false) })
  }, [])
  return { busy, run }
}

/** One option of a {@link Select}. */
export interface SelectItem<V extends string> { id: V; label: string; disabled?: boolean | undefined }

/**
 * A dropdown choice built on the shared menu.
 * @param props.label Accessible name of the trigger.
 * @param props.value Selected option id.
 * @param props.items Options in display order.
 * @param props.onChange Receives the chosen id.
 * @param props.disabled Whether the trigger is disabled.
 * @returns The trigger and its menu.
 */
export function Select<V extends string>({ label, value, items, onChange, disabled = false }: {
  label: string
  value: V
  items: readonly SelectItem<V>[]
  onChange(value: V): void
  disabled?: boolean | undefined
}): JSX.Element {
  const [open, setOpen] = useState(false)
  return <div className={css.select}>
    <Menu open={open} onClose={() => { setOpen(false) }} portal compact selectedId={value}
      anchor={<Button variant="outline" className={css.selectButton} aria-label={label} aria-haspopup="menu"
        aria-expanded={open} disabled={disabled} onClick={() => { setOpen(!open) }}>
        <span>{items.find(item => item.id === value)?.label ?? value}</span><IconChevronDownOutlineRegular size={14} />
      </Button>}
      items={items.map(item => ({ id: item.id, label: item.label, disabled: item.disabled === true }))}
      onSelect={(id) => {
        setOpen(false)
        const chosen = items.find(item => item.id === id)
        if (chosen !== undefined) onChange(chosen.id)
      }} />
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
