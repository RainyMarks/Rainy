/** Settings window: General, Models, Skills & MCP, Runtime, Memory and Security sections, plus the shortcut list. */
import { useId, useRef, useState } from 'react'
import { createPortal } from 'react-dom'
import clsx from 'clsx'
import type { WorkspaceId } from '../../shared/ide-files-protocol.ts'
import { IconCloseOutlineRegular } from '../ui/icons/index.tsx'
import { useModalLayer } from '../ui/useModalLayer.ts'
import { ExtensionsSection } from './ExtensionsSection.tsx'
import { GeneralSection, ShortcutsSection } from './GeneralSection.tsx'
import { MemorySection } from './MemorySection.tsx'
import { useT } from './messages.ts'
import type { SettingsMessage } from './messages.ts'
import { ModelsSection } from './ModelsSection.tsx'
import { RuntimeSection } from './RuntimeSection.tsx'
import { SecuritySection } from './SecuritySection.tsx'
import css from './SettingsDialog.module.css'

/** Section identifiers: the navigation list in order, then the shortcut list at the foot of the navigation. */
export type SettingsSection = 'general' | 'models' | 'extensions' | 'runtime' | 'memory' | 'security' | 'shortcuts'

/** Where "Send feedback" leads; the desktop app opens it in the system browser. */
const FEEDBACK_URL = 'https://github.com/RainyMarks/RainyAgent/issues'

/** Inputs from the window layout. */
export interface SettingsDialogProps {
  open: boolean
  /** Section to show when the dialog opens. */
  section?: SettingsSection | undefined
  /** Current project for the Runtime, Memory and context-budget views. */
  workspace: { workspaceId: WorkspaceId; path: string; title: string } | null
  onClose(): void
}

const sections: readonly { id: SettingsSection; label: SettingsMessage }[] = [
  { id: 'general', label: 'navGeneral' },
  { id: 'models', label: 'navModels' },
  { id: 'extensions', label: 'navExtensions' },
  { id: 'runtime', label: 'navRuntime' },
  { id: 'memory', label: 'navMemory' },
  { id: 'security', label: 'navSecurity' },
]

/**
 * Modal settings window with a text-only section list on the left; its foot holds the shortcut list, the feedback link and the version.
 * Escape, the mask and the close button close it.
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
  const version = typeof window === 'undefined' ? undefined : window.__RAINY_AGENT__?.version
  const pane = (id: SettingsSection, content: JSX.Element): JSX.Element | null =>
    id === active || visited.has(id) ? <div key={id} hidden={id !== active}>{content}</div> : null
  return createPortal(<div className={css.overlay} role="presentation">
    <div className={css.mask} aria-hidden="true" onClick={onClose} />
    <div ref={panel} tabIndex={-1} className={css.panel} role="dialog" aria-modal="true" aria-labelledby={titleId}
      data-settings-dialog data-section={active} data-shortcut-modal="settings">
      <nav className={css.nav} aria-labelledby={titleId}>
        <div className={css.navTitle} id={titleId}>{t('title')}</div>
        <div className={css.navList}>
          {sections.map(({ id, label }) => <button key={id} type="button" className={clsx(css.navCell, id === active && css.active)}
            aria-current={id === active ? 'page' : undefined} data-modal-autofocus={id === active ? '' : undefined}
            data-settings-nav={id} onClick={() => { setActive(id) }}>
            <span className={css.navLabel}>{t(label)}</span>
          </button>)}
        </div>
        <div className={css.navFooter}>
          <button type="button" className={clsx(css.navCell, active === 'shortcuts' && css.active)} aria-current={active === 'shortcuts' ? 'page' : undefined}
            data-modal-autofocus={active === 'shortcuts' ? '' : undefined} data-settings-footer="shortcuts" onClick={() => { setActive('shortcuts') }}>
            <span className={css.navLabel}>{t('shortcutsTitle')}</span>
          </button>
          <a className={css.navCell} href={FEEDBACK_URL} target="_blank" rel="noreferrer" data-settings-footer="feedback">
            <span className={css.navLabel}>{t('navFeedback')}</span>
          </a>
          <div className={css.identity}>
            <img src="/rainy/icon.png" alt="" className={css.identityMark} />
            <div className={css.identityText}>
              <div className={css.identityName}>Rainy</div>
              {version !== undefined && <div className={css.identityVersion}>{t('appVersion', { version })}</div>}
            </div>
          </div>
        </div>
      </nav>
      <div className={css.content}>
        <button type="button" className={css.close} aria-label={t('close')} onClick={onClose}><IconCloseOutlineRegular size={16} /></button>
        <div className={css.options}>
          <div className={css.page}>
            {pane('general', <GeneralSection />)}
            {pane('models', <ModelsSection workspace={workspace} />)}
            {pane('extensions', <ExtensionsSection key={project} workspace={workspace} />)}
            {pane('runtime', <RuntimeSection key={project} workspace={workspace} />)}
            {pane('memory', <MemorySection key={project} workspace={workspace} />)}
            {pane('security', <SecuritySection key={project} workspace={workspace} />)}
            {pane('shortcuts', <ShortcutsSection />)}
          </div>
        </div>
      </div>
    </div>
  </div>, document.body)
}
