/** Settings window: General, Models, Skills & MCP, Runtime and Memory sections. */
import { useId, useRef, useState } from 'react'
import { createPortal } from 'react-dom'
import clsx from 'clsx'
import type { WorkspaceId } from '../../shared/ide-files-protocol.ts'
import {
  IconCloseOutlineRegular, IconCodeOutlineMedium, IconDataOutlineMedium, IconDatabaseOutlineMedium, IconSettingsOutlineMedium,
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
import css from './SettingsDialog.module.css'

/** Section identifiers, in navigation order. */
export type SettingsSection = 'general' | 'models' | 'extensions' | 'runtime' | 'memory'

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
  const [requested, setRequested] = useState({ open, section })
  if (requested.open !== open || requested.section !== section) {
    setRequested({ open, section })
    if (open) setActive(section ?? 'general')
  }
  const panel = useRef<HTMLDivElement>(null)
  const titleId = useId()
  useModalLayer(panel, open, onClose)
  if (!open) return null
  const project = workspace?.workspaceId ?? ''
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
          {active === 'general' && <GeneralSection />}
          {active === 'models' && <ModelsSection workspace={workspace} />}
          {active === 'extensions' && <ExtensionsSection key={project} workspace={workspace} />}
          {active === 'runtime' && <RuntimeSection key={project} workspace={workspace} />}
          {active === 'memory' && <MemorySection key={project} workspace={workspace} />}
        </div>
      </div>
    </div>
  </div>, document.body)
}
