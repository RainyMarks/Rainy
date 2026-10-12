/** Audio and video files in the browser's own player; byte ranges from the Host make seeking work. */
import { useEffect, useState } from 'react'
import { fileSizeText } from '../../ui/index.ts'
import { useViewerT } from './messages.ts'
import css from './Viewers.module.css'

function duration(seconds: number): string {
  if (!Number.isFinite(seconds)) return '—'
  const whole = Math.round(seconds)
  const hours = Math.floor(whole / 3600)
  const minutes = Math.floor(whole / 60) % 60
  const rest = String(whole % 60).padStart(2, '0')
  return hours > 0 ? `${hours}:${String(minutes).padStart(2, '0')}:${rest}` : `${minutes}:${rest}`
}

/**
 * @param props.url Byte route URL.
 * @param props.kind Player element.
 * @param props.bytes File size.
 * @param props.onHex Switch the tab to the hex view.
 * @returns The viewer.
 */
export function MediaViewer({ url, kind, bytes, onHex }: { url: string; kind: 'audio' | 'video'; bytes: number; onHex: () => void }): JSX.Element {
  const t = useViewerT()
  const [facts, setFacts] = useState<{ duration: number; width: number; height: number } | undefined>()
  const [failed, setFailed] = useState(false)
  useEffect(() => { setFacts(undefined); setFailed(false) }, [url])
  const loaded = (element: HTMLMediaElement): void => {
    setFacts({ duration: element.duration, width: element instanceof HTMLVideoElement ? element.videoWidth : 0, height: element instanceof HTMLVideoElement ? element.videoHeight : 0 })
  }
  return (
    <div className={css.viewer} data-viewer={kind}>
      <div className={css.stage}>
        {failed
          ? <div className={css.message}><span>{t('mediaUnsupported')}</span><button type="button" className={css.toolButton} onClick={onHex}>{t('viewHexTitle')}</button></div>
          : <div className={css.mediaStage}>
            {kind === 'video'
              ? <video className={css.media} src={url} controls preload="metadata" onLoadedMetadata={event => { loaded(event.currentTarget) }} onError={() => { setFailed(true) }} />
              : <audio src={url} controls preload="metadata" onLoadedMetadata={event => { loaded(event.currentTarget) }} onError={() => { setFailed(true) }} />}
          </div>}
      </div>
      <div className={css.status}>
        {facts !== undefined && <span>{t('mediaDuration', { duration: duration(facts.duration) })}</span>}
        {facts !== undefined && facts.width > 0 && <span>{t('imageSize', { width: facts.width, height: facts.height })}</span>}
        <span>{fileSizeText(bytes)}</span>
      </div>
    </div>
  )
}
