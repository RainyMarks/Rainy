/** The Git pane: what changed in each repository of the project, staging, discarding and committing, with an AI commit message. */
import { useState } from 'react'
import clsx from 'clsx'
import type { GitChangeKind, GitFileChange, GitRepository } from '../../shared/git-protocol.ts'
import type { IdeRootId, WorkspaceId } from '../../shared/ide-files-protocol.ts'
import type { IdeModel, IdeState } from '../ide/ide-model.ts'
import { fileKey } from '../ide/ide-paths.ts'
import { host } from '../rpc.ts'
import {
  Button, FileTypeIcon, IconAction, IconBranchOutlineRegular, IconPlusOutlineRegular, IconRefreshOutlineRegular, IconSparkleRegular,
  isComposingKey, Modal, toast,
} from '../ui/index.ts'
import { refreshGit, useGitStatus } from './git-store.ts'
import { useGitT } from './messages.ts'
import css from './GitView.module.css'

type GitT = ReturnType<typeof useGitT>

const LETTERS: Record<GitChangeKind, string> = { modified: 'M', added: 'A', deleted: 'D', renamed: 'R', copied: 'C', untracked: 'U', conflicted: '!' }
const KIND_LABELS = {
  modified: 'gitKindModified', added: 'gitKindAdded', deleted: 'gitKindDeleted', renamed: 'gitKindRenamed', copied: 'gitKindCopied',
  untracked: 'gitKindUntracked', conflicted: 'gitKindConflicted',
} as const

/** Paths a change touches; a rename also names where it came from, so both sides move together. */
function pathsOf(changes: readonly GitFileChange[]): string[] {
  return [...new Set(changes.flatMap(change => change.from === undefined ? [change.path] : [change.path, change.from]))]
}

const byPath = (left: GitFileChange, right: GitFileChange): number => left.path < right.path ? -1 : left.path > right.path ? 1 : 0

/**
 * The Git pane.
 * @param props The IDE model and state.
 * @returns The pane.
 */
export function GitView({ model, state }: { model: IdeModel; state: IdeState }): JSX.Element {
  const t = useGitT()
  const workspace = state.workspace
  const git = useGitStatus(workspace?.workspaceId ?? null, true)
  if (workspace === null) return <div className={css.view}><p className={css.notice}>{t('gitNoWorkspace')}</p></div>
  const status = git.status
  return (
    <div className={css.view} data-git-view>
      {git.error !== undefined && <p className={clsx(css.notice, css.error)} role="alert">{git.error}</p>}
      {status?.available === false && <p className={css.notice} data-git-missing>{t('gitMissing')}</p>}
      {status?.repositories.map(repository => (
        <RepositoryView key={repository.rootId} workspaceId={workspace.workspaceId} repository={repository} model={model}
          titled={status.repositories.length + status.plain.length > 1} t={t} />
      ))}
      {status?.plain.map(root => <PlainRoot key={root.rootId} workspaceId={workspace.workspaceId} rootId={root.rootId} title={root.title} t={t} />)}
    </div>
  )
}

function PlainRoot({ workspaceId, rootId, title, t }: { workspaceId: WorkspaceId; rootId: IdeRootId; title: string; t: GitT }): JSX.Element {
  const [busy, setBusy] = useState(false)
  return (
    <div className={css.plain} data-git-plain>
      <p className={css.notice}>{t('gitNotRepository', { title })}</p>
      <Button size="sm" variant="outline" disabled={busy} onClick={() => {
        setBusy(true)
        void host.call('git.init', { workspaceId, rootId }).then(() => refreshGit())
          .catch((error: unknown) => { toast(error instanceof Error ? error.message : String(error), { tone: 'error' }) })
          .finally(() => { setBusy(false) })
      }}>{t('gitInit')}</Button>
    </div>
  )
}

function RepositoryView({ workspaceId, repository, model, titled, t }: {
  workspaceId: WorkspaceId; repository: GitRepository; model: IdeModel; titled: boolean; t: GitT
}): JSX.Element {
  const [message, setMessage] = useState('')
  const [busy, setBusy] = useState(false)
  const [writing, setWriting] = useState(false)
  const [discarding, setDiscarding] = useState<readonly GitFileChange[] | undefined>()
  const staged = repository.files.filter(file => file.staged !== undefined).sort(byPath)
  const unstaged = repository.files.filter(file => file.unstaged !== undefined).sort(byPath)
  const stageAll = staged.length === 0 && unstaged.length > 0
  const run = (operation: () => Promise<unknown>, after?: () => void): void => {
    setBusy(true)
    void operation().then(() => { after?.() }).catch((error: unknown) => { toast(error instanceof Error ? error.message : String(error), { tone: 'error' }) })
      .finally(() => { setBusy(false); void refreshGit() })
  }
  const call = (method: 'git.stage' | 'git.unstage' | 'git.discard', changes: readonly GitFileChange[]): void => {
    if (changes.length === 0) return
    run(() => host.call(method, { workspaceId, rootId: repository.rootId, paths: pathsOf(changes) }), method === 'git.discard' ? () => { model.pollNow() } : undefined)
  }
  const commit = (): void => {
    if (message.trim() === '' || busy || (staged.length === 0 && unstaged.length === 0)) return
    run(() => host.call('git.commit', { workspaceId, rootId: repository.rootId, message, stageAll }).then(({ commit: id }) => {
      setMessage('')
      toast(t('gitCommitted', { commit: id }))
    }))
  }
  const generate = (): void => {
    if (writing) return
    setWriting(true)
    void host.call('git.message', { workspaceId, rootId: repository.rootId }).then((reply) => { setMessage(reply.message) })
      .catch((error: unknown) => { toast(error instanceof Error ? error.message : String(error), { tone: 'error' }) })
      .finally(() => { setWriting(false) })
  }
  const open = (change: GitFileChange, kind: GitChangeKind): void => {
    const path = fileKey(change.path, repository.rootId)
    // A deleted file has nothing to open; a new one has nothing to compare with.
    if (kind === 'deleted') return
    const shown = kind === 'untracked' || kind === 'added' ? model.openFile(path) : model.diff(path)
    void shown.catch((error: unknown) => { model.fail(error) })
  }
  const row = (change: GitFileChange, kind: GitChangeKind, section: 'staged' | 'unstaged'): JSX.Element => {
    const slash = change.path.lastIndexOf('/')
    const label = t(KIND_LABELS[kind])
    return (
      <li key={`${section}:${change.path}`} className={css.row} data-git-file={change.path} data-git-section={section}>
        <button type="button" className={css.fileButton} title={change.from === undefined ? `${change.path} · ${label}` : `${change.path} · ${t('gitRenamedFrom', { from: change.from })}`}
          onClick={() => { open(change, kind) }}>
          <FileTypeIcon path={change.path} size={14} />
          <span className={clsx(css.fileName, kind === 'deleted' && css.deleted)}>{change.path.slice(slash + 1)}</span>
          {slash > 0 && <span className={css.directory}>{change.path.slice(0, slash)}</span>}
        </button>
        <span className={css.rowActions}>
          {kind !== 'deleted' && <IconAction label={t('gitOpenFile')} onClick={() => { void model.openFile(fileKey(change.path, repository.rootId)).catch((error: unknown) => { model.fail(error) }) }}>
            <span className={css.glyph} aria-hidden="true">↗</span>
          </IconAction>}
          {section === 'unstaged' && <IconAction label={t('gitDiscard')} disabled={busy} onClick={() => { setDiscarding([change]) }}>
            <span className={css.glyph} aria-hidden="true">↶</span>
          </IconAction>}
          {section === 'unstaged'
            ? <IconAction label={t('gitStage')} disabled={busy} onClick={() => { call('git.stage', [change]) }}><IconPlusOutlineRegular size={14} /></IconAction>
            : <IconAction label={t('gitUnstage')} disabled={busy} onClick={() => { call('git.unstage', [change]) }}><span className={css.glyph} aria-hidden="true">−</span></IconAction>}
        </span>
        <span className={css.letter} data-kind={kind} title={label}>{LETTERS[kind]}</span>
      </li>
    )
  }
  const branch = repository.branch ?? (repository.head === null ? t('gitNoCommits') : `${t('gitDetached')} ${repository.head}`)
  return (
    <section className={css.repository} data-git-repository={repository.rootId}>
      <div className={css.branchRow}>
        <span className={css.branch} title={t('gitBranchTitle', { branch })}>
          <IconBranchOutlineRegular size={14} />
          {titled && <span className={css.rootTitle}>{repository.title} ·</span>}
          <span className={css.branchName}>{branch}</span>
          {repository.ahead > 0 && <span className={css.sync} title={t('gitAhead', { count: repository.ahead })}>↑{repository.ahead}</span>}
          {repository.behind > 0 && <span className={css.sync} title={t('gitBehind', { count: repository.behind })}>↓{repository.behind}</span>}
        </span>
        <IconAction label={t('gitRefresh')} onClick={() => { void refreshGit() }}><IconRefreshOutlineRegular size={14} /></IconAction>
      </div>
      <div className={css.commitBox}>
        <textarea className={css.message} rows={3} value={message} aria-label={t('gitMessageLabel')} placeholder={t('gitMessagePlaceholder')} data-git-message
          spellCheck={false} onChange={(event) => { setMessage(event.target.value) }}
          onKeyDown={(event) => {
            if (event.key === 'Enter' && event.ctrlKey && !isComposingKey(event.nativeEvent)) { event.preventDefault(); commit() }
          }} />
        <div className={css.commitActions}>
          <Button size="sm" variant="primary" className={css.commitButton} data-git-commit
            disabled={busy || message.trim() === '' || (staged.length === 0 && unstaged.length === 0)} onClick={commit}>
            {stageAll ? t('gitCommitAll') : t('gitCommit')}
          </Button>
          <IconAction label={writing ? t('gitGenerating') : t('gitGenerate')} disabled={writing || (staged.length === 0 && unstaged.length === 0)} onClick={generate}>
            <IconSparkleRegular size={14} className={clsx(writing && css.spinning)} />
          </IconAction>
        </div>
      </div>
      {repository.truncated && <p className={css.notice}>{t('gitTruncated')}</p>}
      {staged.length > 0 && <div className={css.section}>
        <div className={css.sectionHeader}>
          <span className={css.sectionTitle}>{t('gitStaged')}</span>
          <span className={css.sectionActions}>
            <IconAction label={t('gitUnstageAll')} disabled={busy} onClick={() => { call('git.unstage', staged) }}><span className={css.glyph} aria-hidden="true">−</span></IconAction>
          </span>
          <span className={css.count}>{staged.length}</span>
        </div>
        <ul className={css.list}>{staged.map(change => row(change, change.staged!, 'staged'))}</ul>
      </div>}
      <div className={css.section}>
        <div className={css.sectionHeader}>
          <span className={css.sectionTitle}>{t('gitChanges')}</span>
          {unstaged.length > 0 && <span className={css.sectionActions}>
            <IconAction label={t('gitDiscardAll')} disabled={busy} onClick={() => { setDiscarding(unstaged) }}><span className={css.glyph} aria-hidden="true">↶</span></IconAction>
            <IconAction label={t('gitStageAll')} disabled={busy} onClick={() => { call('git.stage', unstaged) }}><IconPlusOutlineRegular size={14} /></IconAction>
          </span>}
          <span className={css.count}>{unstaged.length}</span>
        </div>
        {unstaged.length === 0 && staged.length === 0 ? <p className={css.notice}>{t('gitNoChanges')}</p>
          : <ul className={css.list}>{unstaged.map(change => row(change, change.unstaged!, 'unstaged'))}</ul>}
      </div>
      <Modal open={discarding !== undefined} onClose={() => { setDiscarding(undefined) }} title={t('gitDiscardTitle')} closeLabel={t('close')}
        description={t('gitDiscardBody', { count: discarding?.length ?? 0 })}
        footer={<>
          <Button variant="outline" onClick={() => { setDiscarding(undefined) }}>{t('cancel')}</Button>
          <Button variant="primary" onClick={() => { const changes = discarding ?? []; setDiscarding(undefined); call('git.discard', changes) }}>{t('gitDiscard')}</Button>
        </>} />
    </section>
  )
}
