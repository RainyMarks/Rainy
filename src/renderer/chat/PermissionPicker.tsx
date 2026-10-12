/** Composer control for what the agent may do without asking: read-only, ask, or full auto. */
import { useState } from 'react'
import type { PermissionMode } from '../../shared/rpc.ts'
import { emit } from '../app/bus.ts'
import { IconChevronDownOutlineRegular, IconSettingsOutlineRegular, Menu, type MenuEntry } from '../ui/index.ts'
import { useChatT } from './messages.ts'
import css from './Composer.module.css'

const MODES: readonly PermissionMode[] = ['read-only', 'ask', 'auto']

/**
 * @param props.mode The chat's mode, or the mode the next chat starts in.
 * @param props.onChange The user picked a mode.
 * @returns The picker.
 */
export function PermissionPicker({ mode, onChange }: { mode: PermissionMode; onChange: (mode: PermissionMode) => void }): JSX.Element {
  const t = useChatT()
  const [open, setOpen] = useState(false)
  const label: Record<PermissionMode, string> = { 'read-only': t('permissionReadOnly'), ask: t('permissionAsk'), auto: t('permissionAuto') }
  const hint: Record<PermissionMode, string> = { 'read-only': t('permissionReadOnlyHint'), ask: t('permissionAskHint'), auto: t('permissionAutoHint') }
  const items: MenuEntry[] = [
    { type: 'label', id: 'title', text: t('permissionTitle') },
    ...MODES.map(value => ({
      id: value,
      label: <span className={css.modeItem}><span>{label[value]}</span><span className={css.modeHint}>{hint[value]}</span></span>,
    })),
    { type: 'separator', id: 'sep-settings' },
    { id: 'settings', label: t('permissionManage'), icon: <IconSettingsOutlineRegular size={14} /> },
  ]
  const select = (id: string): void => {
    setOpen(false)
    if (id === 'settings') emit('pane.show', { pane: 'settings', section: 'security' })
    else if (MODES.includes(id as PermissionMode)) onChange(id as PermissionMode)
  }
  return (
    <Menu open={open} onClose={() => { setOpen(false) }} side="top" align="start" portal dense onSelect={select} items={items} selectedIds={[mode]}
      anchor={
        <button type="button" className={css.picker} data-permission={mode} title={hint[mode]} aria-haspopup="menu" aria-expanded={open}
          aria-label={`${t('permissionTitle')}: ${label[mode]}`} onClick={() => { setOpen(value => !value) }}>
          <span className={css.pickerLabel}>{label[mode]}</span>
          <IconChevronDownOutlineRegular size={12} />
        </button>
      } />
  )
}
