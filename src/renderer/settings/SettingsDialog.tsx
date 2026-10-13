/** Settings window: General, Models, Skills & MCP, Runtime and Memory sections. */
import { useId, useRef, useState } from 'react'
import { createPortal } from 'react-dom'
import clsx from 'clsx'
import type { WorkspaceId } from '../../shared/ide-files-protocol.ts'
import {
  IconCloseOutlineRegular, IconCodeOutlineMedium, IconDataOutlineMedium, IconDatabaseOutlineMedium, IconSettingsOutlineMedium, IconShieldOutlineMedium,
  IconSkillOutlineMedium,
} from '../ui/icons/index.tsx'
import { useModalLayer } from '../ui/useModalLayer.ts'
import { ExtensionsSection } from './ExtensionsSection.tsx'
import { GeneralSection } from './GeneralSection.tsx'
import { MemorySection } from './MemorySection.tsx'
import { useT } from './messages.ts'
import type { SettingsMessage } from './messages.ts'
import { ModelsSection } from './ModelsSection.tsx'
import { RuntimeSection } from './RuntimeSection.tsx'
import { SecuritySection } from './SecuritySection.tsx'
import css from './SettingsDialog.module.css'

/** Section identifiers, in navigation order. */
export type SettingsSection = 'general' | 'models' | 'extensions' | 'runtime' | 'memory' | 'security'

/** Inputs from the window layout. */
export interface SettingsDialogProps {
  open: boolean
  /** Section to show when the dialog opens. */
  section?: SettingsSection | undefined
  /** Current project for the Runtime, Memory and context-budget views. */
  workspace: { workspaceId: WorkspaceId; path: string; title: string } | null
  onClose(): void
}

const sections: readonly { id: SettingsSection; label: SettingsMessage; Icon: typeof IconSettingsOutlineMedium }[] = [
  { id: 'general', label: 'navGeneral', Icon: IconSettingsOutlineMedium },
  { id: 'models', label: 'navModels', Icon: IconDataOutlineMedium },
  { id: 'extensions', label: 'navExtensions', Icon: IconSkillOutlineMedium },
  { id: 'runtime', label: 'navRuntime', Icon: IconCodeOutlineMedium },
  { id: 'memory', label: 'navMemory', Icon: IconDatabaseOutlineMedium },
  { id: 'security', label: 'navSecurity', Icon: IconShieldOutlineMedium },
]

/**
 * Modal settings window with a section list on the left. Escape, the mask and the close button close it.
 * Opening it, or changing `section` while it is open, shows the requested section.
 * @param props Dialog inputs.
 * @returns The dialog, or nothing while closed.
 */
export function SettingsDialog({ open, section, workspace, onClose }: SettingsDialogProps): JSX.Element | null {
  const t = useT()
  const [active, setActive] = useState<SettingsSection>(section ?? 'general')
  /** Sections opened since the dialog opened; they stay mounted while hidden, so switching keeps unsaved edits. */
  const [visited, setVisited] = useState<ReadonlySet<SettingsSection>>(() => new Set())
  const [requested, setRequested] = useState({ open, section })
  if (requested.open !== open || requested.section !== section) {
    setRequested({ open, section })
    if (open) setActive(section ?? 'general')
    if (open && !requested.open) setVisited(new Set())
  }
  if (open && !visited.has(active)) setVisited(new Set([...visited, active]))
  const panel = useRef<HTMLDivElement>(null)
  const titleId = useId()
  useModalLayer(panel, open, onClose)
  if (!open) return null
  const project = workspace?.workspaceId ?? ''
  const pane = (id: SettingsSection, content: JSX.Element): JSX.Element | null =>
    id === active || visited.has(id) ? <div key={id} hidden={id !== active}>{content}</div> : null
  return createPortal(<div className={css.overlay} role="presentation">
    <div className={css.mask} aria-hidden="true" onClick={onClose} />
    <div ref={panel} tabIndex={-1} className={css.panel} role="dialog" aria-modal="true" aria-labelledby={titleId}
      data-settings-dialog data-section={active} data-shortcut-modal="settings">
      <nav className={css.nav} aria-labelledby={titleId}>
        <div className={css.navTitle} id={titleId}>{t('title')}</div>
        <div className={css.navList}>
          {sections.map(({ id, label, Icon }) => <button key={id} type="button" className={clsx(css.navCell, id === active && css.active)}
            aria-current={id === active ? 'page' : undefined} data-modal-autofocus={id === active ? '' : undefined}
            data-settings-nav={id} onClick={() => { setActive(id) }}>
            <Icon className={css.navIcon} size={16} />
            <span className={css.navLabel}>{t(label)}</span>
          </button>)}
        </div>
      </nav>
      <div className={css.content}>
        <div className={css.header}>
          <button type="button" className={css.close} aria-label={t('close')} onClick={onClose}><IconCloseOutlineRegular size={14} /></button>
        </div>
        <div className={css.options}>
          {pane('general', <GeneralSection />)}
          {pane('models', <ModelsSection workspace={workspace} />)}
          {pane('extensions', <ExtensionsSection key={project} workspace={workspace} />)}
          {pane('runtime', <RuntimeSection key={project} workspace={workspace} />)}
          {pane('memory', <MemorySection key={project} workspace={workspace} />)}
          {pane('security', <SecuritySection key={project} workspace={workspace} />)}
        </div>
      </div>
    </div>
  </div>, document.body)
}
