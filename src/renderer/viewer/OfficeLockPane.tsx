/** Lazy-loaded Office `~$` owner lock pane. */
import { useEffect, useState } from 'react'
import type { IdeDirectory, IdeRootId, WorkspaceId } from '../../shared/ide-files-protocol.ts'
import { callIde } from '../ide/ide-api.ts'
import { lockedDocumentName } from '../ide/viewers/file-facts.ts'
import { OfficeLockViewer } from '../ide/viewers/OfficeViewers.tsx'

export function OfficeLockPane({ url, bytes, workspaceId, rootId, path, onOpenFile }: {
  url: string
  bytes: number
  workspaceId: WorkspaceId
  rootId: IdeRootId | undefined
  path: string
  onOpenFile: (path: string) => void
}): JSX.Element {
  const slash = path.lastIndexOf('/')
  const folder = slash < 0 ? '' : path.slice(0, slash)
  const lock = path.slice(slash + 1)
  const [locked, setLocked] = useState<string | undefined>()
  useEffect(() => {
    let cancelled = false
    void callIde({ op: 'files.list', workspaceId, rootId, path: folder }).then((listing) => {
      if (!cancelled) setLocked(lockedDocumentName(lock, (listing as IdeDirectory).entries))
    }, () => undefined)
    return () => { cancelled = true }
  }, [workspaceId, rootId, folder, lock])
  return <OfficeLockViewer url={url} size={bytes} document={locked ?? lock.slice(2)}
    onOpenDocument={locked === undefined ? undefined : () => { onOpenFile(folder === '' ? locked : `${folder}/${locked}`) }} />
}
