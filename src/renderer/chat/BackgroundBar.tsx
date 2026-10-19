/** Commands the agent left running in the background, above the composer, with a way to stop each one. */
import { useState } from 'react'
import type { BackgroundCommand } from '../../shared/rpc.ts'
import { host } from '../rpc.ts'
import { Button, StateDot, toast } from '../ui/index.ts'
import { useChatT } from './messages.ts'
import css from './BackgroundBar.module.css'

/**
 * @param props.sessionId Chat the commands belong to.
 * @param props.commands The chat's background commands; the bar is hidden while there are none.
 * @returns The bar.
 */
export function BackgroundBar({ sessionId, commands }: { sessionId: string; commands: readonly BackgroundCommand[] }): JSX.Element | null {
  const t = useChatT()
  const [stopping, setStopping] = useState<string | undefined>()
  if (commands.length === 0) return null
  const stop = (command: BackgroundCommand): void => {
    setStopping(command.id)
    host.call('chat.stopCommand', { sessionId, id: command.id })
      .then(() => { toast(t('backgroundStopToast', { id: command.id }), { tone: 'success' }) })
      .catch((error: unknown) => { toast(error instanceof Error ? error.message : String(error), { tone: 'error' }) })
      .finally(() => { setStopping(undefined) })
  }
  return (
    <section className={css.bar} data-background aria-label={t('backgroundTitle', { count: commands.length })}>
      <div className={css.title}>{t('backgroundTitle', { count: commands.length })}</div>
      {commands.map(command => (
        <div key={command.id} className={css.row} data-command={command.id}>
          <StateDot state={command.state === 'running' ? 'ongoing' : command.exitCode === 0 ? 'done' : 'error'} />
          <span className={css.id}>{command.id}</span>
          <span className={css.what} title={command.command}>{command.description || command.command}</span>
          <span className={css.state}>
            {command.state === 'running'
              ? command.pending > 0 ? `${t('backgroundRunning')} · ${t('backgroundUnread', { bytes: command.pending })}` : t('backgroundRunning')
              : command.signal !== null ? t('backgroundStopped') : t('backgroundExited', { code: command.exitCode ?? '?' })}
          </span>
          {command.state === 'running' && (
            <Button size="sm" variant="outline" disabled={stopping === command.id} aria-label={t('backgroundStop', { id: command.id })}
              onClick={() => { stop(command) }}>{t('backgroundStop', { id: command.id })}</Button>
          )}
        </div>
      ))}
    </section>
  )
}
