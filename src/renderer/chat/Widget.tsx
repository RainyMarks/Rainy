/** An interactive answer from `show_widget`, shown inline in the chat: the model's page runs in a sandboxed frame with an
 *  opaque origin and no network, receives the theme, streams in while the model writes it, and may ask the page to send
 *  a chat message (only right after a click in it) or open an http(s) link. */
import { useEffect, useMemo, useRef, useState } from 'react'
import type { ToolCall } from '@earendil-works/pi-ai'
import {
  clampHeight, frameMessage, partialHtml, standaloneHtml, WIDGET_FRAME_PATH, WIDGET_MESSAGE, WIDGET_PROMPT_MAX_CHARS, widgetArgs,
  type WidgetHostMessage, type WidgetTheme,
} from '../../shared/widget-protocol.ts'
import { host } from '../rpc.ts'
import { IconAction, IconCopyOutlineRegular, IconRefreshOutlineRegular, toast, writeClipboard } from '../ui/index.ts'
import { useChatT } from './messages.ts'
import css from './Widget.module.css'

/** Theme variable of the frame → token of the page it takes its value from. */
const THEME_TOKENS: Readonly<Record<string, string>> = {
  bg: '--rainy-alias-bg-base', surface: '--rainy-alias-bg-layer-1', 'surface-2': '--rainy-alias-bg-layer-2', text: '--rainy-alias-label-primary',
  'text-2': '--rainy-alias-label-secondary', 'text-3': '--rainy-alias-label-tertiary', border: '--rainy-alias-border-l2',
  'border-strong': '--rainy-alias-border-l1', accent: '--rainy-alias-link', success: '--rainy-alias-state-success-primary',
  warning: '--rainy-alias-state-warn-primary', danger: '--rainy-alias-state-error-primary', control: '--rainy-alias-control-fill',
  'control-hover': '--rainy-alias-control-fill-hover', 'code-bg': '--rainy-alias-markdown-code-block', font: '--rainy-font-family',
  'font-mono': '--rainy-font-family-code',
}
/** A message to the frame before it is tagged. */
type Outgoing = WidgetHostMessage extends infer Message ? Message extends WidgetHostMessage ? Omit<Message, 'tag'> : never : never

/** Previews are sent at most this often while the model writes; the frame morphs each into the last. */
const PREVIEW_INTERVAL_MS = 120
/** A widget may send one message on the user's behalf per this many milliseconds. */
const PROMPT_INTERVAL_MS = 2000

/** @returns The page's current theme as frame variables. */
export function currentTheme(): WidgetTheme {
  const dark = document.body.hasAttribute('data-rainy-dark')
  const style = getComputedStyle(document.body)
  const vars: Record<string, string> = {}
  for (const [name, token] of Object.entries(THEME_TOKENS)) {
    const value = style.getPropertyValue(token).trim()
    if (value !== '') vars[name] = value
  }
  vars['accent-text'] = dark ? '#111113' : '#ffffff'
  return { dark, vars }
}

/**
 * @param props.sessionId Chat the widget belongs to; messages it sends go there.
 * @param props.call The `show_widget` call, possibly still streaming.
 * @param props.complete The call's arguments are final.
 * @param props.cut The reply stopped before the call was finished.
 * @returns The widget.
 */
export function Widget({ sessionId, call, complete, cut = false }: {
  sessionId: string
  call: ToolCall
  complete: boolean
  cut?: boolean | undefined
}): JSX.Element {
  const t = useChatT()
  const args = useMemo(() => widgetArgs(call.arguments), [call.arguments])
  const frame = useRef<HTMLIFrameElement>(null)
  const [run, setRun] = useState(0)
  const [ready, setReady] = useState(false)
  const [height, setHeight] = useState<number | undefined>()
  const [shown, setShown] = useState(false)
  const [errors, setErrors] = useState<string[]>([])
  const sentFinal = useRef(false)
  const lastPreview = useRef(0)
  const lastPrompt = useRef(0)
  const latest = useRef({ args, complete, sessionId })
  latest.current = { args, complete, sessionId }

  const post = (message: Outgoing): void => {
    frame.current?.contentWindow?.postMessage({ ...message, tag: WIDGET_MESSAGE }, '*')
  }

  // A new run starts with a fresh frame.
  useEffect(() => {
    setReady(false)
    setShown(false)
    setErrors([])
    setHeight(undefined)
    sentFinal.current = false
  }, [run])

  // Messages from this widget's frame only: it is the source window, whatever origin the sandbox gives it.
  useEffect(() => {
    const listener = (event: MessageEvent): void => {
      if (event.source === null || event.source !== frame.current?.contentWindow) return
      const message = frameMessage(event.data)
      if (message === undefined) return
      switch (message.type) {
        case 'ready':
          post({ type: 'theme', theme: currentTheme() })
          setReady(true)
          return
        case 'resize':
          setHeight(clampHeight(message.height))
          return
        case 'error':
          setErrors(previous => previous.length >= 3 || previous.includes(message.message) ? previous : [...previous, message.message])
          return
        case 'prompt': {
          const text = message.text.trim().slice(0, WIDGET_PROMPT_MAX_CHARS)
          // A click inside the frame gives it focus; a script cannot send messages for the user without one.
          const clicked = document.activeElement === frame.current
          const now = Date.now()
          if (!clicked || text === '' || now - lastPrompt.current < PROMPT_INTERVAL_MS) {
            post({ type: 'reply', id: message.id, ok: false, error: t('widgetNeedsClick') })
            if (!clicked) toast(t('widgetNeedsClick'))
            return
          }
          lastPrompt.current = now
          host.call('chat.send', { sessionId: latest.current.sessionId, text, mode: 'queue' }).then(
            () => { post({ type: 'reply', id: message.id, ok: true }); toast(t('widgetSent')) },
            (error: unknown) => { post({ type: 'reply', id: message.id, ok: false, error: error instanceof Error ? error.message : String(error) }) },
          )
          return
        }
        case 'link': {
          const ok = /^https?:\/\/[^\s]+$/iu.test(message.url)
          if (ok) window.open(message.url, '_blank', 'noopener,noreferrer')
          post({ type: 'reply', id: message.id, ok, ...(ok ? {} : { error: 'only http(s) links open' }) })
          return
        }
      }
    }
    window.addEventListener('message', listener)
    return () => { window.removeEventListener('message', listener) }
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [t])

  // The theme follows the page: dark mode, color scheme, accent and fonts are attributes and styles of the body and root.
  useEffect(() => {
    if (!ready) return
    const observer = new MutationObserver(() => { post({ type: 'theme', theme: currentTheme() }) })
    observer.observe(document.body, { attributes: true, attributeFilter: ['data-rainy-dark', 'data-color-scheme', 'data-accent', 'style', 'class'] })
    observer.observe(document.documentElement, { attributes: true, attributeFilter: ['style', 'class'] })
    return () => { observer.disconnect() }
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [ready])

  // Stream the content in: previews while the model writes, then the finished answer once.
  useEffect(() => {
    if (!ready || sentFinal.current) return
    if (complete) {
      sentFinal.current = true
      post({ type: 'final', css: args.css, html: args.html, script: cut ? '' : args.script })
      setShown(true)
      return
    }
    if (args.html === '') return
    const send = (): void => {
      lastPreview.current = Date.now()
      const current = latest.current.args
      post({ type: 'preview', css: current.css, html: partialHtml(current.html) })
      setShown(true)
    }
    const wait = PREVIEW_INTERVAL_MS - (Date.now() - lastPreview.current)
    if (wait <= 0) { send(); return }
    const timer = setTimeout(() => { if (!latest.current.complete) send() }, wait)
    return () => { clearTimeout(timer) }
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [ready, complete, args, cut])

  const copy = (): void => {
    void writeClipboard(standaloneHtml(args)).then((copied) => { if (copied) toast(t('widgetCopied')) })
  }
  const fix = (message: string): void => {
    host.call('chat.send', { sessionId, text: t('widgetFixPrompt', { title: args.title || t('widgetUntitled'), message }), mode: 'queue' })
      .catch((error: unknown) => { toast(error instanceof Error ? error.message : String(error), { tone: 'error' }) })
  }

  const frameHeight = height ?? args.height
  return (
    <figure className={css.widget} data-widget={call.id} data-state={complete ? 'done' : 'streaming'}>
      <figcaption className={css.caption}>
        <span className={css.title}>{args.title || t('widgetUntitled')}</span>
        {complete && <>
          <IconAction label={t('widgetRerun')} onClick={() => { setRun(value => value + 1) }}><IconRefreshOutlineRegular size={14} /></IconAction>
          <IconAction label={t('widgetCopy')} onClick={copy}><IconCopyOutlineRegular size={14} /></IconAction>
        </>}
      </figcaption>
      <div className={css.stage} style={{ height: frameHeight }}>
        <iframe key={run} ref={frame} className={css.frame} src={WIDGET_FRAME_PATH} sandbox="allow-scripts" title={args.title || t('widgetLabel')}
          data-shown={shown || undefined} />
        {!shown && <div className={css.placeholder}><span className={css.spinner} aria-hidden />{t('widgetGenerating')}</div>}
      </div>
      {cut && <p className={css.note}>{t('widgetIncomplete')}</p>}
      {errors.map(message => (
        <p key={message} className={css.error} role="alert">
          <span>{t('widgetError', { message })}</span>
          <button type="button" className={css.fix} onClick={() => { fix(message) }}>{t('widgetFix')}</button>
        </p>
      ))}
    </figure>
  )
}
