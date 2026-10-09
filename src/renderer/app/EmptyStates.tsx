/** Editor area without a project (welcome) and without an open file. */
import type { IdeWorkspace } from '../../shared/ide-files-protocol.ts'
import { Button, IconFolderCloseRegular, ShortcutKeys } from '../ui/index.ts'
import { useIdeT } from '../ide/messages.ts'
import css from './App.module.css'

/**
 * First view without a project: the two ways to open one, then recently registered projects.
 * @param props.workspaces Registered projects, most relevant first.
 * @param props.loading Whether a project is being opened.
 * @param props.nativeDirectory Whether the desktop folder picker is available.
 * @param props.pending Whether a folder choice is in progress.
 * @param props.open Opens the native folder picker.
 * @param props.browse Opens the in-app directory browser.
 * @param props.select Opens a recent project.
 * @returns The welcome view.
 */
export function Welcome({ workspaces, loading, nativeDirectory, pending, open, browse, select }: {
  workspaces: readonly IdeWorkspace[]
  loading: boolean
  nativeDirectory: boolean
  pending: boolean
  open: () => void
  browse: () => void
  select: (workspace: IdeWorkspace) => void
}) {
  const t = useIdeT()
  return <div className={css.welcome}>
    <div className={css.welcomeBody}>
      <img src="/rainy/icon.png" alt="" className={css.welcomeMark} />
      <h2 className={css.welcomeTitle}>{t('ideWelcomeTitle')}</h2>
      <p className={css.welcomeText}>{t('ideWelcomeDescription')}</p>
      <div className={css.welcomeActions}>
        {nativeDirectory && <Button variant="primary" disabled={pending} onClick={open}>{t('ideOpenFolder')}</Button>}
        <Button variant="outline" disabled={pending} onClick={browse}>{t('ideWslFolder')}</Button>
      </div>
      {workspaces.length > 0 && <section className={css.recent} aria-label={t('ideRecentProjects')}>
        <h3>{t('ideRecentProjects')}</h3>
        {workspaces.slice(0, 6).map(workspace => (
          <button key={workspace.workspaceId} type="button" className={css.recentItem} title={workspace.path}
            disabled={loading} onClick={() => { select(workspace) }}>
            <IconFolderCloseRegular size={14} />
            <span className={css.recentName}>{workspace.title}</span>
            <span className={css.recentPath}>{workspace.path}</span>
          </button>
        ))}
      </section>}
    </div>
  </div>
}

/**
 * A project without an open file offers search, a new file, and the assistant instead of a blank editor.
 * @param props.shortcut Quick-open key caps.
 * @param props.search Opens quick open.
 * @param props.create Creates a file.
 * @param props.agent Shows the AI pane; omitted while it is already shown.
 * @returns The view.
 */
export function NoFile({ shortcut, search, create, agent }: {
  shortcut: readonly string[]
  search: () => void
  create: () => void
  agent: (() => void) | undefined
}) {
  const t = useIdeT()
  return <div className={css.welcome}>
    <div className={css.welcomeBody}>
      <h2 className={css.welcomeTitle}>{t('ideNoFileTitle')}</h2>
      <p className={css.welcomeText}>{t('ideNoFile')}</p>
      <div className={css.shortcutList}>
        <button type="button" className={css.shortcutRow} onClick={search}>
          <span>{t('ideSearchFiles')}</span><ShortcutKeys keys={shortcut} className={css.shortcut} />
        </button>
        <button type="button" className={css.shortcutRow} onClick={create}><span>{t('ideCreateFile')}</span></button>
        {agent !== undefined && <button type="button" className={css.shortcutRow} onClick={agent}><span>{t('ideOpenAgent')}</span></button>}
      </div>
    </div>
  </div>
}
