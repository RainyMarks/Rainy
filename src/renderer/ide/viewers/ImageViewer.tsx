/** Image viewer: fit or zoom, pan, pixel readout and "add to chat". */
import { useEffect, useLayoutEffect, useRef, useState, type KeyboardEvent, type PointerEvent } from 'react'
import { fileSizeText, toast } from '../../ui/index.ts'
import { useViewerT } from './messages.ts'
import { ToolActions } from './ToolActions.tsx'
import { useZoomKeys } from './zoom-keys.ts'
import css from './Viewers.module.css'

const ZOOM_STEPS = [0.05, 0.1, 0.25, 0.33, 0.5, 0.67, 0.75, 1, 1.25, 1.5, 2, 3, 4, 6, 8, 12, 16, 24, 32] as const
/** The pixel readout copies every pixel once; larger images show only coordinates. */
const MAX_READ_PIXELS = 50_000_000
/** The longest edge the composer sends to the model (`MAX_IMAGE_EDGE` in `native/crates/app/src/ui/composer.rs`); a
 *  larger image is scaled down here already, which keeps the message to the workbench small. */
const MAX_ATTACH_EDGE = 1568

/** An image for the AI composer: a PNG of what the viewer shows. */
export interface ImageAttachment {
  readonly name: string
  readonly mime: 'image/png'
  readonly base64: string
}

function hex2(value: number): string {
  return value.toString(16).padStart(2, '0').toUpperCase()
}

/**
 * @param props.url Byte route URL of the image, or an object URL of SVG text being edited.
 * @param props.name File name, used for the attachment; the zoom starts over for another file.
 * @param props.bytes File size.
 * @param props.onAttach Hand the image to the AI composer; without it there is no "add to chat".
 * @returns The viewer.
 */
export function ImageViewer({ url, name, bytes, onAttach }: { url: string; name: string; bytes: number; onAttach?: ((image: ImageAttachment) => void) | undefined }): JSX.Element {
  const t = useViewerT()
  const stage = useRef<HTMLDivElement | null>(null)
  const [stageElement, setStageElement] = useState<HTMLDivElement | null>(null)
  const image = useRef<HTMLImageElement>(null)
  const pixels = useRef<ImageData | undefined>(undefined)
  const pan = useRef<{ x: number; y: number; left: number; top: number } | undefined>(undefined)
  const [natural, setNatural] = useState<{ width: number; height: number } | undefined>()
  const [failed, setFailed] = useState(false)
  const [zoom, setZoom] = useState<number | 'fit'>('fit')
  const [fit, setFit] = useState(1)
  const [panning, setPanning] = useState(false)
  const [pixel, setPixel] = useState<{ x: number; y: number; rgba: readonly [number, number, number, number] | undefined } | undefined>()

  // A new address of the same file (an edited SVG) keeps the zoom; the size is measured again once it loads.
  useEffect(() => {
    setFailed(false)
    pixels.current = undefined
  }, [url])
  useEffect(() => {
    setNatural(undefined); setZoom('fit'); setPixel(undefined)
  }, [name])

  useLayoutEffect(() => {
    const element = stage.current
    if (element === null || natural === undefined) return
    const measure = (): void => {
      setFit(Math.min(1, Math.max(0.01, (element.clientWidth - 24) / natural.width), Math.max(0.01, (element.clientHeight - 24) / natural.height)))
    }
    measure()
    const observer = new ResizeObserver(measure)
    observer.observe(element)
    return () => { observer.disconnect() }
  }, [natural])

  const scale = zoom === 'fit' ? fit : zoom

  const readPixels = (): ImageData | undefined => {
    if (pixels.current !== undefined) return pixels.current
    const element = image.current
    if (element === null || natural === undefined || natural.width * natural.height > MAX_READ_PIXELS) return undefined
    const canvas = document.createElement('canvas')
    canvas.width = natural.width
    canvas.height = natural.height
    const context = canvas.getContext('2d', { willReadFrequently: true })
    if (context === null) return undefined
    context.drawImage(element, 0, 0)
    pixels.current = context.getImageData(0, 0, canvas.width, canvas.height)
    return pixels.current
  }

  const zoomTo = (next: number, clientX?: number, clientY?: number): void => {
    const element = stage.current
    if (element === null || natural === undefined) { setZoom(next); return }
    const rect = element.getBoundingClientRect()
    const x = (clientX ?? rect.left + rect.width / 2) - rect.left
    const y = (clientY ?? rect.top + rect.height / 2) - rect.top
    const ratio = next / scale
    const left = (element.scrollLeft + x) * ratio - x
    const top = (element.scrollTop + y) * ratio - y
    setZoom(next)
    requestAnimationFrame(() => { element.scrollLeft = left; element.scrollTop = top })
  }
  const step = (direction: 1 | -1, clientX?: number, clientY?: number): void => {
    const next = direction > 0 ? ZOOM_STEPS.find(value => value > scale * 1.001) : [...ZOOM_STEPS].reverse().find(value => value < scale * 0.999)
    if (next !== undefined) zoomTo(next, clientX, clientY)
  }

  useZoomKeys(stageElement, { zoomIn: (x, y) => { step(1, x, y) }, zoomOut: (x, y) => { step(-1, x, y) }, reset: () => { setZoom('fit') } }, !failed)
  const onKeyDown = (event: KeyboardEvent<HTMLDivElement>): void => {
    if (event.ctrlKey || event.metaKey || event.altKey) return
    const actions: Record<string, () => void> = { '+': () => { step(1) }, '=': () => { step(1) }, '-': () => { step(-1) },
      '0': () => { setZoom('fit') }, '1': () => { zoomTo(1) } }
    const action = actions[event.key]
    if (action !== undefined) { event.preventDefault(); action() }
  }
  const pannable = (): boolean => {
    const element = stage.current
    return element !== null && (element.scrollWidth > element.clientWidth || element.scrollHeight > element.clientHeight)
  }
  const onPointerDown = (event: PointerEvent<HTMLDivElement>): void => {
    const element = stage.current
    if (event.button !== 0 || element === null || !pannable()) return
    pan.current = { x: event.clientX, y: event.clientY, left: element.scrollLeft, top: element.scrollTop }
    element.setPointerCapture(event.pointerId)
    setPanning(true)
  }
  const onPointerMove = (event: PointerEvent<HTMLDivElement>): void => {
    const element = stage.current
    if (pan.current !== undefined && element !== null) {
      element.scrollLeft = pan.current.left - (event.clientX - pan.current.x)
      element.scrollTop = pan.current.top - (event.clientY - pan.current.y)
      return
    }
    const target = image.current
    if (target === null || natural === undefined) return
    const rect = target.getBoundingClientRect()
    const x = Math.floor((event.clientX - rect.left) / rect.width * natural.width)
    const y = Math.floor((event.clientY - rect.top) / rect.height * natural.height)
    if (x < 0 || y < 0 || x >= natural.width || y >= natural.height) { setPixel(undefined); return }
    const source = readPixels()
    const offset = (y * natural.width + x) * 4
    setPixel({ x, y, rgba: source === undefined ? undefined : [source.data[offset]!, source.data[offset + 1]!, source.data[offset + 2]!, source.data[offset + 3]!] })
  }
  const onPointerUp = (event: PointerEvent<HTMLDivElement>): void => {
    if (pan.current === undefined) return
    pan.current = undefined
    stage.current?.releasePointerCapture(event.pointerId)
    setPanning(false)
  }

  // The workbench says when the image is in the composer; this only encodes it.
  const attach = async (): Promise<void> => {
    const source = image.current
    if (source === null || natural === undefined || onAttach === undefined) return
    try {
      const shrink = Math.min(1, MAX_ATTACH_EDGE / Math.max(natural.width, natural.height))
      const canvas = document.createElement('canvas')
      canvas.width = Math.max(1, Math.round(natural.width * shrink))
      canvas.height = Math.max(1, Math.round(natural.height * shrink))
      const context = canvas.getContext('2d')
      if (context === null) throw new Error(t('imageBroken'))
      context.imageSmoothingQuality = 'high'
      context.drawImage(source, 0, 0, canvas.width, canvas.height)
      const blob = await new Promise<Blob | null>((resolve) => { canvas.toBlob(resolve, 'image/png') })
      if (blob === null) throw new Error(t('imageBroken'))
      const raw = new Uint8Array(await blob.arrayBuffer())
      let binary = ''
      for (let offset = 0; offset < raw.length; offset += 0x8000) binary += String.fromCharCode(...raw.subarray(offset, offset + 0x8000))
      const stem = name.replace(/\.[^.]*$/u, '') || 'image'
      onAttach({ name: `${stem}.png`, mime: 'image/png', base64: btoa(binary) })
    } catch (error) {
      toast(t('imageAttachFailed', { error: error instanceof Error ? error.message : String(error) }), { tone: 'error' })
    }
  }

  const size = natural === undefined ? undefined : { width: Math.max(1, Math.round(natural.width * scale)), height: Math.max(1, Math.round(natural.height * scale)) }
  const pixelated = scale > 1 || undefined
  return (
    <div className={css.viewer} data-viewer="image">
      <div className={css.toolbar}>
        <button type="button" className={css.toolButton} title={t('zoomOut')} aria-label={t('zoomOut')} disabled={failed} onClick={() => { step(-1) }}>−</button>
        <span>{t('zoomLevel', { percent: Math.round(scale * 100) })}</span>
        <button type="button" className={css.toolButton} title={t('zoomIn')} aria-label={t('zoomIn')} disabled={failed} onClick={() => { step(1) }}>+</button>
        <button type="button" className={css.toolButton} aria-pressed={zoom === 'fit'} disabled={failed} onClick={() => { setZoom('fit') }}>{t('zoomFit')}</button>
        <button type="button" className={css.toolButton} title={t('zoomActual')} aria-pressed={zoom === 1} disabled={failed} onClick={() => { zoomTo(1) }}>1:1</button>
        <span className={css.grow} />
        {onAttach !== undefined && <ToolActions actions={[
          { id: 'image-attach', label: t('imageAttach'), title: t('imageAttachTitle'), disabled: failed || natural === undefined, onSelect: () => { void attach() } },
        ]} />}
      </div>
      <div ref={(element) => { stage.current = element; setStageElement(element) }} className={`${css.stage} ${css.checker}`} tabIndex={0} onKeyDown={onKeyDown}
        onPointerDown={onPointerDown} onPointerMove={onPointerMove} onPointerUp={onPointerUp} onPointerCancel={onPointerUp}
        onPointerLeave={() => { if (pan.current === undefined) setPixel(undefined) }}>
        {failed ? <div className={css.message}>{t('imageBroken')}</div> : (
          <div className={css.imageStage} data-pannable={zoom !== 'fit' || undefined} data-panning={panning || undefined}>
            <img ref={image} className={css.picture} src={url} alt={name} draggable={false}
              data-pixelated={pixelated} style={size}
              onLoad={(event) => {
                const element = event.currentTarget
                setNatural({ width: element.naturalWidth || 300, height: element.naturalHeight || 150 })
              }}
              onError={() => { setFailed(true) }} />
          </div>
        )}
      </div>
      <div className={css.status}>
        {natural !== undefined && <span>{t('imageSize', { width: natural.width, height: natural.height })}</span>}
        <span>{fileSizeText(bytes)}</span>
        {pixel !== undefined && <span>
          {t('imagePixel', { x: pixel.x, y: pixel.y })}
          {pixel.rgba !== undefined && <> <span className={css.swatch} style={{ background: `rgba(${pixel.rgba[0]}, ${pixel.rgba[1]}, ${pixel.rgba[2]}, ${pixel.rgba[3] / 255})` }} />
            {' '}<code>#{hex2(pixel.rgba[0])}{hex2(pixel.rgba[1])}{hex2(pixel.rgba[2])}{hex2(pixel.rgba[3])}</code>
            {' '}<code>rgba({pixel.rgba.join(', ')})</code></>}
        </span>}
      </div>
    </div>
  )
}
