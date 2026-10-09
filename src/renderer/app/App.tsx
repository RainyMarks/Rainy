/** Root component: the window layout over the page's workbench, plus the toast host. */
import { useEffect } from 'react'
import { ToastHost } from '../ui/index.ts'
import { useViewportSize } from './appearance.ts'
import { WorkbenchLayout } from './Workbench.tsx'
import { sharedWorkbench, type Workbench } from './workbench.ts'

/**
 * Render the application window.
 * @param props.workbench Models to render; defaults to the page's shared workbench.
 * @returns The window.
 */
export function App({ workbench = sharedWorkbench() }: { workbench?: Workbench | undefined }) {
  const { width, height } = useViewportSize()
  useEffect(() => { workbench.start() }, [workbench])
  return <>
    <WorkbenchLayout workbench={workbench} viewportWidth={width} viewportHeight={height} />
    <ToastHost />
  </>
}
