/** Image viewer: fit or zoom, pan, pixel readout, channel and bit-plane views for stego work, and "add to chat". */
import { useEffect, useLayoutEffect, useRef, useState, type KeyboardEvent, type PointerEvent, type WheelEvent } from 'react'
import { emit } from '../../app/bus.ts'
import { fileSizeText, toast } from '../../ui/index.ts'
import { useViewerT } from './messages.ts'
import css from './Viewers.module.css'

const ZOOM_STEPS = [0.05, 0.1, 0.25, 0.33, 0.5, 0.67, 0.75, 1, 1.25, 1.5, 2, 3, 4, 6, 8, 12, 16, 24, 32] as const
/** Channel views hold every pixel twice; larger images keep the plain view. */
const MAX_CHANNEL_PIXELS = 50_000_000

type Plane = 'r' | 'g' | 'b' | 'a'
/** What the stage shows: the image itself, one channel in gray, one bit of one channel, or a whole-image transform. */
export type ImageChannel = 'original' | 'red' | 'green' | 'blue' | 'alpha' | 'invert' | 'gray' | `bit-${Plane}-${number}`

/** Every view in the order the previous/next buttons step through. */
export const IMAGE_CHANNELS: readonly ImageChannel[] = [
  'original', 'red', 'green', 'blue', 'alpha', 'invert', 'gray',
  ...(['r', 'g', 'b', 'a'] as const).flatMap(plane => [7, 6, 5, 4, 3, 2, 1, 0].map(bit => `bit-${plane}-${bit}` as const)),
]

/**
 * Compute a channel view.
 * @param source Original RGBA pixels.
 * @param channel View to compute; `original` returns a copy.
 * @returns New pixels of the same size.
 */
export function channelPixels(source: ImageData, channel: ImageChannel): ImageData {
  const input = source.data
  const output = new ImageData(source.width, source.height)
  const data = output.data
  const bit = /^bit-([rgba])-(\d)$/u.exec(channel)
  const plane = bit === null ? -1 : 'rgba'.indexOf(bit[1]!)
  const mask = bit === null ? 0 : 1 << Number(bit[2])
  const single = { red: 0, green: 1, blue: 2, alpha: 3 }[channel as 'red' | 'green' | 'blue' | 'alpha'] ?? -1
  for (let index = 0; index < input.length; index += 4) {
    let red = input[index]!, green = input[index + 1]!, blue = input[index + 2]!, alpha = input[index + 3]!
    if (plane >= 0) { red = green = blue = (input[index + plane]! & mask) !== 0 ? 255 : 0; alpha = 255 }
    else if (single >= 0) { red = green = blue = input[index + single]!; alpha = 255 }
    else if (channel === 'invert') { red = 255 - red; green = 255 - green; blue = 255 - blue }
    else if (channel === 'gray') { red = green = blue = Math.round(0.299 * red + 0.587 * green + 0.114 * blue) }
    data[index] = red; data[index + 1] = green; data[index + 2] = blue; data[index + 3] = alpha
  }
  return output
}

function hex2(value: number): string {
  return value.toString(16).padStart(2, '0').toUpperCase()
}

/**
 * @param props.url Byte route URL of the image.
 * @param props.name File name, used for the attachment.
 * @param props.bytes File size.
 * @returns The viewer.
 */
export function ImageViewer({ url, name, bytes }: { url: string; name: string; bytes: number }): JSX.Element {
  const t = useViewerT()
  const stage = useRef<HTMLDivElement>(null)
  const image = useRef<HTMLImageElement>(null)
  const view = useRef<HTMLCanvasElement>(null)
  const pixels = useRef<ImageData | undefined>(undefined)
  const pan = useRef<{ x: number; y: number; left: number; top: number } | undefined>(undefined)
  const [natural, setNatural] = useState<{ width: number; height: number } | undefined>()
  const [failed, setFailed] = useState(false)
  const [zoom, setZoom] = useState<number | 'fit'>('fit')
  const [fit, setFit] = useState(1)
  const [panning, setPanning] = useState(false)
  const [pixel, setPixel] = useState<{ x: number; y: number; rgba: readonly [number, number, number, number] | undefined } | undefined>()
  const [channel, setChannel] = useState<ImageChannel>('original')

  useEffect(() => {
    setNatural(undefined); setFailed(false); setZoom('fit'); setPixel(undefined); setChannel('original')
    pixels.current = undefined
  }, [url])

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
    if (element === null || natural === undefined || natural.width * natural.height > MAX_CHANNEL_PIXELS) return undefined
    const canvas = document.createElement('canvas')
    canvas.width = natural.width
    canvas.height = natural.height
    const context = canvas.getContext('2d', { willReadFrequently: true })
    if (context === null) return undefined
    context.drawImage(element, 0, 0)
    pixels.current = context.getImageData(0, 0, canvas.width, canvas.height)
    return pixels.current
  }

  useEffect(() => {
    const canvas = view.current
    if (channel === 'original' || canvas === null || natural === undefined) return
    const source = readPixels()
    if (source === undefined) { toast(t('channelTooLarge')); setChannel('original'); return }
    canvas.width = source.width
    canvas.height = source.height
    canvas.getContext('2d')?.putImageData(channelPixels(source, channel), 0, 0)
  }, [channel, natural])

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
  const cycle = (direction: 1 | -1): void => {
    const index = IMAGE_CHANNELS.indexOf(channel)
    setChannel(IMAGE_CHANNELS[(index + direction + IMAGE_CHANNELS.length) % IMAGE_CHANNELS.length]!)
  }

  const onWheel = (event: WheelEvent<HTMLDivElement>): void => {
    if (!event.ctrlKey && !event.metaKey) return
    event.preventDefault()
    step(event.deltaY < 0 ? 1 : -1, event.clientX, event.clientY)
  }
  const onKeyDown = (event: KeyboardEvent<HTMLDivElement>): void => {
    if (event.ctrlKey || event.metaKey || event.altKey) return
    const actions: Record<string, () => void> = { '+': () => { step(1) }, '=': () => { step(1) }, '-': () => { step(-1) },
      '0': () => { setZoom('fit') }, '1': () => { zoomTo(1) }, '[': () => { cycle(-1) }, ']': () => { cycle(1) } }
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
    const target = channel === 'original' ? image.current : view.current
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

  const attach = async (): Promise<void> => {
    const source = channel === 'original' ? image.current : view.current
    if (source === null || natural === undefined) return
    try {
      const canvas = document.createElement('canvas')
      canvas.width = natural.width
      canvas.height = natural.height
      canvas.getContext('2d')?.drawImage(source, 0, 0)
      const blob = await new Promise<Blob | null>((resolve) => { canvas.toBlob(resolve, 'image/png') })
      if (blob === null) throw new Error('PNG encoding failed')
      const stem = name.replace(/\.[^.]*$/u, '') || 'image'
      emit('pane.show', { pane: 'ai' })
      emit('chat.attach', { files: [new File([blob], `${stem}${channel === 'original' ? '' : `-${channel}`}.png`, { type: 'image/png' })] })
      toast(t('imageAttached'), { tone: 'success' })
    } catch (error) {
      toast(t('imageAttachFailed', { error: error instanceof Error ? error.message : String(error) }))
    }
  }

  const channelLabel = (value: ImageChannel): string => {
    const bit = /^bit-([rgba])-(\d)$/u.exec(value)
    if (bit !== null) return t('channelBit', { channel: bit[1]!.toUpperCase(), bit: bit[2]! })
    return t(({ original: 'channelOriginal', red: 'channelRed', green: 'channelGreen', blue: 'channelBlue', alpha: 'channelAlpha',
      invert: 'channelInvert', gray: 'channelGray' } as const)[value as 'original'])
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
        <span className={css.separator} />
        <span>{t('imageChannel')}</span>
        <button type="button" className={css.toolButton} aria-label="◀" disabled={failed || natural === undefined} onClick={() => { cycle(-1) }}>◀</button>
        <select aria-label={t('imageChannel')} value={channel} disabled={failed || natural === undefined}
          onChange={(event) => { setChannel(event.target.value as ImageChannel) }}>
          {IMAGE_CHANNELS.map(value => <option key={value} value={value}>{channelLabel(value)}</option>)}
        </select>
        <button type="button" className={css.toolButton} aria-label="▶" disabled={failed || natural === undefined} onClick={() => { cycle(1) }}>▶</button>
        <span className={css.grow} />
        <button type="button" className={css.toolButton} title={t('imageAttachTitle')} disabled={failed || natural === undefined} onClick={() => { void attach() }}>{t('imageAttach')}</button>
      </div>
      <div ref={stage} className={`${css.stage} ${css.checker}`} tabIndex={0} onWheel={onWheel} onKeyDown={onKeyDown}
        onPointerDown={onPointerDown} onPointerMove={onPointerMove} onPointerUp={onPointerUp} onPointerCancel={onPointerUp}
        onPointerLeave={() => { if (pan.current === undefined) setPixel(undefined) }}>
        {failed ? <div className={css.message}>{t('imageBroken')}</div> : (
          <div className={css.imageStage} data-pannable={zoom !== 'fit' || undefined} data-panning={panning || undefined}>
            <img ref={image} className={css.picture} src={url} alt={name} draggable={false} hidden={channel !== 'original'}
              data-pixelated={pixelated} style={size}
              onLoad={(event) => {
                const element = event.currentTarget
                setNatural({ width: element.naturalWidth || 300, height: element.naturalHeight || 150 })
              }}
              onError={() => { setFailed(true) }} />
            {channel !== 'original' && <canvas ref={view} className={css.picture} data-pixelated={pixelated} style={size} aria-label={channelLabel(channel)} />}
          </div>
        )}
      </div>
      <div className={css.status}>
        {natural !== undefined && <span>{t('imageSize', { width: natural.width, height: natural.height })}</span>}
        <span>{fileSizeText(bytes)}</span>
        {channel !== 'original' && <span>{channelLabel(channel)}</span>}
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
