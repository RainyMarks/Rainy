/** The command palette: type part of a command's name and run it, with its shortcut shown beside it. */
import { useEffect, useMemo, useRef, useState } from 'react'
import clsx from 'clsx'
import { IconCloseOutlineRegular, isComposingKey, Modal, ShortcutKeys } from '../ui/index.ts'
import { rankCommands, recentCommands, rememberCommand, type PaletteCommand } from './commands.ts'
import { useAppT } from './messages.ts'
import ide from '../ide/Ide.module.css'
import css from './CommandPalette.module.css'

/**
 * The palette, shown while `query` is set.
 * @param props.query Text to start with, or `undefined` while closed.
 * @param props.commands Every command.
 * @param props.onClose Closes the palette.
 * @returns The dialog, or nothing while closed.
 */
export function CommandPalette({ query, commands, onClose }: { query: string | undefined; commands: readonly PaletteCommand[]; onClose: () => void }): JSX.Element | null {
  return query === undefined ? null : <OpenPalette initial={query} commands={commands} onClose={onClose} />
}

function OpenPalette({ initial, commands, onClose }: { initial: string; commands: readonly PaletteCommand[]; onClose: () => void }): JSX.Element {
  const t = useAppT()
  const [query, setQuery] = useState(initial)
  const [selected, setSelected] = useState(0)
  const [recent] = useState(recentCommands)
  const list = useRef<HTMLDivElement>(null)
  const shown = useMemo(() => rankCommands(commands, query, recent), [commands, query, recent])
  useEffect(() => {
    const option = list.current?.children[selected]
    if (option instanceof HTMLElement) option.scrollIntoView({ block: 'nearest' })
  }, [selected, shown])
  const run = (command: PaletteCommand | undefined): void => {
    if (command === undefined || command.disabled === true) return
    rememberCommand(command.id)
    onClose()
    // The dialog closes first, so a command that opens another dialog or moves focus is not undone by the close.
    requestAnimationFrame(() => { command.run() })
  }
  return <Modal open headless className={ide.quickDialog} title={t('paletteTitle')} onClose={onClose}>
    <div className={ide.quickSearch}>
      <span className={css.prompt} aria-hidden="true">&gt;</span>
      <input className={ide.quickInput} role="combobox" data-modal-autofocus aria-expanded="true" aria-controls="rainy-palette-commands"
        aria-activedescendant={shown.length === 0 ? undefined : `rainy-palette-command-${selected}`} aria-label={t('paletteTitle')}
        placeholder={t('palettePlaceholder')} value={query} data-palette-input
        onChange={(event) => { setQuery(event.target.value); setSelected(0) }}
        onKeyDown={(event) => {
          if (isComposingKey(event.nativeEvent)) return
          if (event.key === 'ArrowDown') { event.preventDefault(); setSelected(Math.min(shown.length - 1, selected + 1)) }
          else if (event.key === 'ArrowUp') { event.preventDefault(); setSelected(Math.max(0, selected - 1)) }
          else if (event.key === 'Enter') { event.preventDefault(); run(shown[selected]) }
        }} />
      <button type="button" className={`${ide.button} ${ide.quickClose}`} aria-label={t('paletteClose')} onClick={onClose}><IconCloseOutlineRegular size={14} /></button>
    </div>
    <div ref={list} id="rainy-palette-commands" role="listbox" aria-label={t('paletteTitle')} className={ide.quickFiles} data-palette-list>
      {shown.map((command, index) => (
        <button key={command.id} type="button" id={`rainy-palette-command-${index}`} role="option" aria-selected={selected === index}
          aria-disabled={command.disabled === true} className={clsx(ide.quickFile, css.command, command.disabled === true && css.disabled)}
          data-command={command.id} onMouseMove={() => { if (selected !== index) setSelected(index) }} onClick={() => { run(command) }}>
          <span className={css.label}><span className={css.category}>{command.category}: </span>{command.label}</span>
          {command.keys !== undefined && <ShortcutKeys keys={command.keys} className={css.keys} />}
        </button>
      ))}
    </div>
    {shown.length === 0 && <div className={ide.quickNotice}>{t('paletteEmpty')}</div>}
  </Modal>
}
