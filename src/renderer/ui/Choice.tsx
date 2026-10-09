/** Token-styled select menu for forms and pickers. */
import { useState } from 'react'
import { Button } from './Button.tsx'
import { Menu } from './Menu.tsx'
import { IconChevronDownOutlineRegular } from './icons/index.tsx'
import css from './Choice.module.css'

/**
 * Render a keyboard-accessible choice whose popup uses the application theme.
 * @param props.label Accessible name of the trigger.
 * @param props.value Selected item id.
 * @param props.items Choices in display order.
 * @param props.onChange Receives the chosen id.
 * @param props.disabled Whether the trigger is disabled.
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
