/** Multi-root project file tree: folders first, then names; root headers when several folders are mounted. */
import type { IdeFileEntry, IdeRootId } from '../../shared/ide-files-protocol.ts'
import { FileTypeIcon, IconAction, IconChevronDownOutlineRegular, IconChevronRightOutlineRegular, IconCloseOutlineRegular,
  IconFolderCloseRegular } from '../ui/index.ts'
import type { IdeState } from './ide-model.ts'
import { fileKey, fileLabel, workspaceRoots } from './ide-paths.ts'
import { useIdeT } from './messages.ts'
import css from './Ide.module.css'

function isFolder(entry: IdeFileEntry): boolean {
  return entry.kind === 'directory' || entry.targetKind === 'directory'
}

function Level({ state, directory, level, selected, select, activate }: {
  state: IdeState
  directory: string
  level: number
  selected: string | undefined
  select: (entry: IdeFileEntry) => void
  activate: (entry: IdeFileEntry) => void
}) {
  const entries = [...(state.directories[directory]?.entries ?? [])].sort((left, right) =>
    Number(isFolder(right)) - Number(isFolder(left)) || left.name.localeCompare(right.name))
  return entries.map((entry) => {
    const folder = isFolder(entry)
    const expanded = state.data.expandedPaths.includes(entry.path)
    return (
      <div key={entry.path}>
        <div className={css.treeRow} data-selected={selected === entry.path || undefined}>
          <button
            type="button"
            className={`${css.button} ${css.treeButton}`}
            style={{ paddingLeft: 9 + level * 14 }}
            role="treeitem"
            aria-expanded={folder ? expanded : undefined}
            aria-selected={selected === entry.path}
            title={fileLabel(state.workspace, entry.path)}
            disabled={state.phase === 'loading' || entry.outsideWorkspace || entry.kind === 'other'}
            onClick={() => { select(entry); activate(entry) }}
            onContextMenu={(event) => { event.preventDefault(); select(entry) }}
          >
            <span className={css.treeIcon} aria-hidden>
              {folder ? (expanded ? <IconChevronDownOutlineRegular size={14} /> : <IconChevronRightOutlineRegular size={14} />)
                : <FileTypeIcon path={entry.name} size={16} />}
            </span>
            <span className={css.treeName}>{entry.name}</span>
          </button>
        </div>
        {folder && expanded && (
          <Level state={state} directory={entry.path} level={level + 1} selected={selected} select={select} activate={activate} />
        )}
      </div>
    )
  })
}

/**
 * Render every mounted root of the selected project.
 * @param props.state Workspace snapshot with listed directories.
 * @param props.selected Selected entry path (a renderer file key).
 * @param props.selectedRootId Root whose header is selected while no entry is.
 * @param props.select Selects an entry and its root.
 * @param props.selectRoot Selects a root header.
 * @param props.activate Opens a file or toggles a folder.
 * @param props.removeRoot Detaches a secondary root.
 * @returns The tree.
 */
export function FileTree({ state, selected, selectedRootId, select, selectRoot, activate, removeRoot }: {
  state: IdeState
  selected: string | undefined
  selectedRootId: IdeRootId | undefined
  select: (entry: IdeFileEntry, rootId: IdeRootId) => void
  selectRoot: (rootId: IdeRootId) => void
  activate: (entry: IdeFileEntry) => void
  removeRoot: (rootId: IdeRootId) => void
}) {
  const t = useIdeT()
  const roots = state.workspace === null ? [] : workspaceRoots(state.workspace)
  return (
    <div className={css.tree} role="tree" aria-label={t('ideExplorer')} aria-busy={state.phase === 'loading'}>
      {roots.map(root => <div key={root.rootId}>
        {roots.length > 1 && <div className={css.rootHeader}>
          <button type="button" className={`${css.button} ${css.rootName}`} title={root.path}
            data-active={(selectedRootId ?? 'primary') === root.rootId || undefined}
            onClick={() => { selectRoot(root.rootId) }}>
            <IconFolderCloseRegular size={14} /><span>{root.title}</span>
          </button>
          {!root.primary && <IconAction label={`${t('ideRemoveRoot')} ${root.title}`} onClick={() => { removeRoot(root.rootId) }}>
            <IconCloseOutlineRegular size={12} />
          </IconAction>}
        </div>}
        {!root.primary && state.phase === 'ready' && state.directories[fileKey('', root.rootId)] === undefined
          && <p className={`${css.notice} ${css.error}`} role="status">{t('ideUnavailableRoot')}</p>}
        <Level state={state} directory={fileKey('', root.rootId)} level={roots.length > 1 ? 1 : 0}
          selected={selected} select={(entry) => { select(entry, root.rootId) }} activate={activate} />
      </div>)}
    </div>
  )
}
