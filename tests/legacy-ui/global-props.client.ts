/** Framework hooks for isolated Rainy component fixtures. */
import type { GlobalStandardProps } from '@deepseek-ai/dsh-client-ui-slots'

const unused = (): never => { throw new Error('This isolated Rainy component does not consume framework hooks') }

/** Fail if an isolated component starts reading an unprovided framework source. */
export const globalProps: GlobalStandardProps = {
  usePanelInfo: unused, useSessions: unused, useSessionStatus: unused,
  useSessionRetainInfo: unused, useResource: unused, useWorkspaces: unused,
}
