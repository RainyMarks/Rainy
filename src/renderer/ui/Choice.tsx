/** Token-styled select menu shared by Rainy's forms and project selectors. */
import { useState } from 'react'
import { Button, Menu, IconChevronDownOutlineRegular } from '@deepseek-ai/dsh-client-ui-primitives'
import css from './SettingsSections.module.css'

/** Render a keyboard-accessible choice whose expanded popup uses the application theme.
 * @param props Field name, selected key, choices, and change callback.
 * @returns A labeled menu trigger and its portaled options.
 */
export function Choice({ label, value, items, onChange, disabled = false }: {
  label: string
  value: string
  items: readonly { id: string; label: string; disabled?: boolean | undefined }[]
  onChange: (value: string) => void
  disabled?: boolean | undefined
}) {
  const [open, setOpen] = useState(false)
  return <div className={css.choice}>
    <Menu open={open} onClose={() => { setOpen(false) }} portal compact
      anchor={<Button variant="outline" className={css.choiceButton} aria-label={label} aria-haspopup="menu"
        aria-expanded={open} disabled={disabled} onClick={() => { setOpen(!open) }}>
        <span>{items.find(item => item.id === value)?.label ?? value}</span><IconChevronDownOutlineRegular size={14} />
      </Button>}
      items={items.map(({ disabled: unavailable, ...item }) => ({
        ...item, ...(unavailable === undefined ? {} : { disabled: unavailable }),
      }))}
      onSelect={(id) => { setOpen(false); onChange(id) }} />
  </div>
}
