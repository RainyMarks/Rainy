/** Message input: `@` references, `/` commands, images, queue/steer while busy, and the model controls. */
import { useEffect, useLayoutEffect, useRef, useState, type ClipboardEvent, type DragEvent, type KeyboardEvent } from 'react'
import clsx from 'clsx'
import type { ImageContent } from '@earendil-works/pi-ai'
import type { WorkspaceId } from '../../shared/ide-files-protocol.ts'
import type { CompletionItem, ContextUsage, ModelSelection, QueuedMessage } from '../../shared/rpc.ts'
import { getPrefs } from '../prefs.ts'
import { host } from '../rpc.ts'
import {
  IconCloseOutlineRegular, IconCompactOutlineRegular, IconFolderOpenOutlineRegular, IconNewChatOutlineRegular, IconPaperclipOutlineRegular,
  IconPaperPlaneOutlineRegular, IconQueueOutlineRegular, IconSparkleRegular, IconStopFillRegular, FileTypeIcon, toast, Tooltip,
} from '../ui/index.ts'
import { useChatT } from './messages.ts'
import { ContextMeter, ModelPicker } from './ModelPicker.tsx'
import css from './Composer.module.css'

/** Most images one message carries. */
const MAX_IMAGES = 8
/** Longest image edge sent to the model; larger images are scaled down. */
const MAX_IMAGE_EDGE = 1568
/** Largest image file accepted before scaling. */
const MAX_IMAGE_BYTES = 20 * 1024 * 1024
/** Encoded size above which an image is re-encoded as JPEG even when it fits `MAX_IMAGE_EDGE`. */
const REENCODE_BYTES = 3.5 * 1024 * 1024
const IMAGE_TYPES = new Set(['image/png', 'image/jpeg', 'image/gif', 'image/webp'])
/** Second Esc within this window stops the run. */
const DOUBLE_ESC_MS = 800

/** Unsent text per chat (`''` key for the new-chat composer). */
const drafts = new Map<string, string>()

type Command = 'compact' | 'model' | 'new'
type Suggestion = { kind: 'command'; command: Command; description: string } | { kind: 'completion'; item: CompletionItem }
interface Trigger { kind: '@' | '/'; start: number; query: string }

function readAsDataUrl(blob: Blob): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader()
    reader.onload = () => { resolve(String(reader.result)) }
    reader.onerror = () => { reject(reader.error ?? new Error('read failed')) }
    reader.readAsDataURL(blob)
  })
}

/**
 * Turn an image file into model input, scaling it down when its longest edge exceeds `MAX_IMAGE_EDGE`.
 * @param file Pasted or dropped image.
 * @returns Base64 image content.
 */
async function toImage(file: File): Promise<ImageContent> {
  const bitmap = await createImageBitmap(file)
  const scale = Math.min(1, MAX_IMAGE_EDGE / Math.max(bitmap.width, bitmap.height))
  if (scale === 1 && file.size <= REENCODE_BYTES) {
    bitmap.close()
    const url = await readAsDataUrl(file)
    return { type: 'image', mimeType: file.type, data: url.slice(url.indexOf(',') + 1) }
  }
  const canvas = document.createElement('canvas')
  canvas.width = Math.max(1, Math.round(bitmap.width * scale))
  canvas.height = Math.max(1, Math.round(bitmap.height * scale))
  const context = canvas.getContext('2d')
  if (context === null) throw new Error('canvas unavailable')
  context.fillStyle = '#fff'
  context.fillRect(0, 0, canvas.width, canvas.height)
  context.drawImage(bitmap, 0, 0, canvas.width, canvas.height)
  bitmap.close()
  const url = canvas.toDataURL('image/jpeg', 0.9)
  return { type: 'image', mimeType: 'image/jpeg', data: url.slice(url.indexOf(',') + 1) }
}

function quotePath(path: string): string {
  return /\s/.test(path) ? `"${path.replace(/"/g, '')}"` : path
}

function findTrigger(text: string, caret: number): Trigger | undefined {
  const before = text.slice(0, caret)
  const command = /^\/(\w*)$/.exec(before)
  if (command !== null) return { kind: '/', start: 0, query: command[1]! }
  const reference = /(^|\s)@("[^"]*|[^\s@"]*)$/.exec(before)
  if (reference !== null) return { kind: '@', start: before.length - reference[2]!.length - 1, query: reference[2]!.replace(/^"/, '') }
  return undefined
}

/** Inputs of the composer. */
export interface ComposerProps {
  sessionId: string | null
  workspaceId: WorkspaceId | null
  /** The chat is running or compacting. */
  busy: boolean
  model: ModelSelection | null
  context: ContextUsage | null
  queue: readonly QueuedMessage[]
  /** Text the pane asks the composer to show (for example after a failed send); consumed once per change of `seed.id`. */
  seed?: { id: number; text: string } | undefined
  onSend(text: string, images: ImageContent[], mode: 'queue' | 'steer'): Promise<boolean>
  onStop(): void
  onCompact(): void
  onNewChat(): void
}

/** Composer for one chat or for the next new chat. */
export function Composer({ sessionId, workspaceId, busy, model, context, queue, seed, onSend, onStop, onCompact, onNewChat }: ComposerProps): JSX.Element {
  const t = useChatT()
  const draftKey = sessionId ?? ''
  const [text, setText] = useState(() => drafts.get(draftKey) ?? '')
  const [images, setImages] = useState<ImageContent[]>([])
  const [trigger, setTrigger] = useState<Trigger | undefined>()
  const [suggestions, setSuggestions] = useState<Suggestion[]>([])
  const [active, setActive] = useState(0)
  const [dragging, setDragging] = useState(false)
  const [sending, setSending] = useState(false)
  const input = useRef<HTMLTextAreaElement>(null)
  const lastEscape = useRef(0)
  const completionSeq = useRef(0)

  useEffect(() => {
    setText(drafts.get(draftKey) ?? '')
    setImages([])
    setTrigger(undefined)
    input.current?.focus()
  }, [draftKey])
  useEffect(() => {
    if (seed === undefined) return
    setText(seed.text)
    input.current?.focus()
  }, [seed?.id])
  useEffect(() => {
    if (text === '') drafts.delete(draftKey)
    else drafts.set(draftKey, text)
  }, [text, draftKey])
  useLayoutEffect(() => {
    const element = input.current
    if (element === null) return
    element.style.height = 'auto'
    element.style.height = `${Math.min(element.scrollHeight, 280)}px`
  }, [text])

  // Suggestions for the active trigger.
  useEffect(() => {
    if (trigger === undefined) { setSuggestions([]); return }
    if (trigger.kind === '/') {
      const commands: { command: Command; description: string }[] = [
        { command: 'compact', description: t('commandCompact') },
        { command: 'model', description: t('commandModel') },
        { command: 'new', description: t('commandNew') },
      ]
      setSuggestions(commands.filter(entry => entry.command.startsWith(trigger.query.toLowerCase()))
        .filter(entry => entry.command !== 'compact' || sessionId !== null)
        .map(entry => ({ kind: 'command', ...entry })))
      setActive(0)
      return
    }
    const seq = ++completionSeq.current
    const timer = window.setTimeout(() => {
      host.call('chat.complete', { ...(sessionId === null ? {} : { sessionId }), workspaceId, query: trigger.query }).then((items) => {
        if (seq !== completionSeq.current) return
        setSuggestions(items.map(item => ({ kind: 'completion', item })))
        setActive(0)
      }, (error: unknown) => { console.error(error) })
    }, 80)
    return () => { window.clearTimeout(timer) }
  }, [trigger?.kind, trigger?.query, sessionId, workspaceId, t])

  const updateTrigger = (value: string, caret: number): void => { setTrigger(findTrigger(value, caret)) }
  const replaceRange = (start: number, end: number, insert: string): void => {
    const next = text.slice(0, start) + insert + text.slice(end)
    setText(next)
    const caret = start + insert.length
    requestAnimationFrame(() => {
      const element = input.current
      if (element === null) return
      element.focus()
      element.setSelectionRange(caret, caret)
      updateTrigger(next, caret)
    })
  }

  const [modelMenu, setModelMenu] = useState(0)
  const runCommand = (command: Command): void => {
    setText('')
    setTrigger(undefined)
    if (command === 'compact') onCompact()
    else if (command === 'new') onNewChat()
    else setModelMenu(value => value + 1)
  }

  const accept = (suggestion: Suggestion): void => {
    if (suggestion.kind === 'command') { runCommand(suggestion.command); return }
    if (trigger === undefined) return
    const caret = input.current?.selectionStart ?? text.length
    const { item } = suggestion
    const continues = item.kind === 'directory'
    replaceRange(trigger.start, caret, `@${item.insert}${continues ? '' : ' '}`)
    if (!continues) setTrigger(undefined)
  }

  const addImages = async (files: readonly File[]): Promise<void> => {
    const accepted: ImageContent[] = []
    for (const file of files) {
      if (!IMAGE_TYPES.has(file.type)) { toast(t('imageUnsupported', { name: file.name })); continue }
      if (file.size > MAX_IMAGE_BYTES) { toast(t('imageTooLarge', { name: file.name })); continue }
      try {
        accepted.push(await toImage(file))
      } catch (error) {
        toast(`${file.name}: ${error instanceof Error ? error.message : String(error)}`)
      }
    }
    setImages((current) => {
      const next = [...current, ...accepted]
      if (next.length > MAX_IMAGES) toast(t('imageLimit', { count: MAX_IMAGES }))
      return next.slice(0, MAX_IMAGES)
    })
  }

  const send = (mode: 'queue' | 'steer'): void => {
    const trimmed = text.trim()
    if (trimmed === '' && images.length === 0) return
    const command = /^\/(compact|model|new)$/.exec(trimmed)?.[1] as Command | undefined
    if (command !== undefined && images.length === 0) { runCommand(command); return }
    const sentText = text
    const sentImages = images
    setText('')
    setImages([])
    setTrigger(undefined)
    setSending(true)
    void onSend(sentText.trim(), sentImages, mode).then((ok) => {
      if (ok) return
      // Restore what the user typed so a failed send loses nothing.
      setText(current => current === '' ? sentText : current)
      setImages(current => current.length === 0 ? sentImages : current)
    }).finally(() => { setSending(false) })
  }

  const onKeyDown = (event: KeyboardEvent<HTMLTextAreaElement>): void => {
    if (event.nativeEvent.isComposing || event.keyCode === 229) return
    if (suggestions.length > 0 && trigger !== undefined) {
      if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
        event.preventDefault()
        const step = event.key === 'ArrowDown' ? 1 : -1
        setActive(index => (index + step + suggestions.length) % suggestions.length)
        return
      }
      if ((event.key === 'Enter' && !event.shiftKey) || event.key === 'Tab') {
        event.preventDefault()
        accept(suggestions[active] ?? suggestions[0]!)
        return
      }
      if (event.key === 'Escape') { event.preventDefault(); setTrigger(undefined); return }
    }
    if (event.key === 'Escape' && busy) {
      const now = Date.now()
      if (now - lastEscape.current < DOUBLE_ESC_MS) { lastEscape.current = 0; onStop() } else lastEscape.current = now
      return
    }
    if (event.key === 'Enter' && !event.shiftKey) {
      event.preventDefault()
      const preferred = getPrefs().busyEnter
      const alternate = event.ctrlKey || event.metaKey
      send(busy ? (alternate ? (preferred === 'queue' ? 'steer' : 'queue') : preferred) : 'queue')
    }
  }

  const onPaste = (event: ClipboardEvent<HTMLTextAreaElement>): void => {
    const files = [...event.clipboardData.files].filter(file => file.type.startsWith('image/'))
    if (files.length === 0) return
    event.preventDefault()
    void addImages(files)
  }

  const onDrop = (event: DragEvent<HTMLDivElement>): void => {
    setDragging(false)
    const files = [...event.dataTransfer.files]
    if (files.length === 0) return
    event.preventDefault()
    const imageFiles = files.filter(file => file.type.startsWith('image/'))
    const paths = files.filter(file => !file.type.startsWith('image/')).map(file => window.__RAINY_HOST_PATHS__?.pathFor(file) ?? '').filter(path => path !== '')
    if (imageFiles.length > 0) void addImages(imageFiles)
    if (paths.length > 0) {
      const caret = input.current?.selectionStart ?? text.length
      const prefix = caret > 0 && !/\s$/.test(text.slice(0, caret)) ? ' ' : ''
      replaceRange(caret, caret, `${prefix}${paths.map(path => `@${quotePath(path)}`).join(' ')} `)
    }
  }

  const preferred = getPrefs().busyEnter
  const placeholder = busy ? t(preferred === 'queue' ? 'placeholderBusy' : 'placeholderBusySteer') : t('placeholder')
  const canSend = (text.trim() !== '' || images.length > 0) && !sending
  const showSuggestions = trigger !== undefined && suggestions.length > 0

  return (
    <div className={css.root}>
      {queue.length > 0 && (
        <ul className={css.queue} aria-label={t('queued')}>
          {queue.map(item => (
            <li key={item.id} className={css.queueItem}>
              <span className={css.queueMode} data-mode={item.mode}>{item.mode === 'steer' ? t('steering') : t('queued')}</span>
              <span className={css.queueText}>{item.text === '' ? `[${item.imageCount}]` : item.text}</span>
              {sessionId !== null && (
                <button type="button" className={css.iconButton} aria-label={t('removeQueued')} onClick={() => {
                  host.call('chat.unqueue', { sessionId, id: item.id }).catch((error: unknown) => { toast(error instanceof Error ? error.message : String(error)) })
                }}><IconCloseOutlineRegular size={12} /></button>
              )}
            </li>
          ))}
        </ul>
      )}
      <div className={clsx(css.box, dragging && css.dragging)}
        onDragOver={(event) => { if (event.dataTransfer.types.includes('Files')) { event.preventDefault(); setDragging(true) } }}
        onDragLeave={() => { setDragging(false) }} onDrop={onDrop}>
        {showSuggestions && (
          <ul className={css.suggestions} role="listbox" aria-label={trigger?.kind === '/' ? t('commandsTitle') : t('referencesFiles')}>
            {suggestions.map((suggestion, index) => (
              <li key={suggestion.kind === 'command' ? suggestion.command : `${suggestion.item.kind}:${suggestion.item.insert}`} role="option"
                aria-selected={index === active} className={css.suggestion} data-active={index === active || undefined}
                onMouseDown={(event) => { event.preventDefault(); accept(suggestion) }} onMouseEnter={() => { setActive(index) }}>
                {suggestion.kind === 'command' ? (
                  <>
                    <span className={css.suggestionIcon}>{suggestion.command === 'compact' ? <IconCompactOutlineRegular size={14} />
                      : suggestion.command === 'new' ? <IconNewChatOutlineRegular size={14} /> : <IconSparkleRegular size={14} />}</span>
                    <span className={css.suggestionLabel}>/{suggestion.command}</span>
                    <span className={css.suggestionDetail}>{suggestion.description}</span>
                  </>
                ) : (
                  <>
                    <span className={css.suggestionIcon}>{suggestion.item.kind === 'directory' ? <IconFolderOpenOutlineRegular size={14} />
                      : suggestion.item.kind === 'session' ? <IconQueueOutlineRegular size={14} /> : <FileTypeIcon path={suggestion.item.label} size={14} />}</span>
                    <span className={css.suggestionLabel}>{suggestion.item.label}</span>
                    {suggestion.item.detail !== undefined && <span className={css.suggestionDetail}>{suggestion.item.detail}</span>}
                  </>
                )}
              </li>
            ))}
          </ul>
        )}
        {images.length > 0 && (
          <div className={css.attachments}>
            {images.map((image, index) => (
              <div key={index} className={css.attachment}>
                <img src={`data:${image.mimeType};base64,${image.data}`} alt="" />
                <button type="button" className={css.removeAttachment} aria-label={t('removeImage')}
                  onClick={() => { setImages(current => current.filter((_, item) => item !== index)) }}>
                  <IconCloseOutlineRegular size={10} />
                </button>
              </div>
            ))}
          </div>
        )}
        <textarea ref={input} className={css.input} value={text} rows={1} placeholder={placeholder} spellCheck={false}
          onChange={(event) => { setText(event.target.value); updateTrigger(event.target.value, event.target.selectionStart) }}
          onSelect={(event) => { updateTrigger(event.currentTarget.value, event.currentTarget.selectionStart) }}
          onBlur={() => { window.setTimeout(() => { setTrigger(undefined) }, 120) }}
          onKeyDown={onKeyDown} onPaste={onPaste} data-chat-input />
        <div className={css.toolbar}>
          <ModelPicker sessionId={sessionId} selection={model} openSignal={modelMenu} />
          <label className={css.iconButton}>
            <Tooltip label={t('attachImage')} side="top" portal><IconPaperclipOutlineRegular size={16} /></Tooltip>
            <input type="file" accept={[...IMAGE_TYPES].join(',')} multiple hidden aria-label={t('attachImage')}
              onChange={(event) => { const files = [...(event.target.files ?? [])]; event.target.value = ''; void addImages(files) }} />
          </label>
          <span className={css.spacer} />
          <ContextMeter context={context} />
          {busy && (
            <Tooltip label={t('stopHint')} side="top" portal>
              <button type="button" className={clsx(css.sendButton, css.stopButton)} aria-label={t('stop')} onClick={onStop}>
                <IconStopFillRegular size={14} />
              </button>
            </Tooltip>
          )}
          {(!busy || canSend) && (
            <button type="button" className={css.sendButton} aria-label={busy ? t(preferred === 'queue' ? 'queue' : 'steer') : t('send')} disabled={!canSend}
              onClick={() => { send(busy ? preferred : 'queue') }}>
              <IconPaperPlaneOutlineRegular size={16} />
            </button>
          )}
        </div>
      </div>
    </div>
  )
}
