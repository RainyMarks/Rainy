/** Lazy-loaded project map pane: keeps `session.stream` / `chat.changes` subscriptions isolated to `?view=map`. */
import type { WorkspaceId } from '../../shared/ide-files-protocol.ts'
import type { SessionId } from '../../shared/rpc.ts'
import { useAiActivity } from '../map/activity.ts'
import { ProjectMapView } from '../map/ProjectMap.tsx'
import { useChanges } from '../review/changes-store.ts'

export function MapPane({ workspaceId, sessionId, openFile, ask }: {
  workspaceId: WorkspaceId
  sessionId: string | null
  openFile: (key: string, line?: number) => void
  ask: (text: string) => void
}): JSX.Element {
  const changes = useChanges(sessionId)
  const activity = useAiActivity()
  return (
    <ProjectMapView workspaceId={workspaceId} visible sessionId={sessionId as SessionId | null} changes={changes} activity={activity}
      openFile={openFile} ask={ask} />
  )
}
