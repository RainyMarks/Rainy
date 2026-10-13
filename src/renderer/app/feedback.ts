/** Page-wide failure notices for actions whose own controls show only success. */
import { describeDisconnection } from '../rpc.ts'
import { onClipboardRefused, toast } from '../ui/index.ts'
import { appMessages } from './messages.ts'

/**
 * Say when a copy is refused, and word requests lost with their connection in the interface language.
 * @returns A function that removes the clipboard notice.
 */
export function installFeedback(): () => void {
  describeDisconnection(() => appMessages.t('connectionLost'))
  return onClipboardRefused(() => { toast(appMessages.t('copyFailed'), { tone: 'error' }) })
}
