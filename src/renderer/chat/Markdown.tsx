/** Assistant markdown: GFM, math, highlighted code blocks with editor actions, and local file links. */
import { Children, isValidElement, memo, useCallback, useMemo, useState, type ReactElement, type ReactNode } from 'react'
import ReactMarkdown, { type Components } from 'react-markdown'
import remarkGfm from 'remark-gfm'
import remarkMath from 'remark-math'
import rehypeKatex from 'rehype-katex'
import clsx from 'clsx'
import { emit } from '../app/bus.ts'
import { CodeToolbar, highlightLines, Tooltip, writeClipboard } from '../ui/index.ts'
import cardCss from '../ui/CodeCard.module.css'
import { useChatT } from './messages.ts'
import css from './Markdown.module.css'

function textOf(node: ReactNode): string {
  if (typeof node === 'string' || typeof node === 'number') return String(node)
  if (Array.isArray(node)) return node.map(textOf).join('')
  if (isValidElement<{ children?: ReactNode }>(node)) return textOf(node.props.children)
  return ''
}

/** A fenced code block with copy, wrap, and the "open in editor" / "compare with current file" actions. */
export function CodeBlock({ code, language, actions = true }: { code: string; language: string | undefined; actions?: boolean }): JSX.Element {
  const t = useChatT()
  const [copied, setCopied] = useState(false)
  const [wrapped, setWrapped] = useState(false)
  const lines = useMemo(() => highlightLines(code, language), [code, language])
  const onCopy = useCallback(() => {
    void writeClipboard(code).then((ok) => {
      if (!ok) return
      setCopied(true)
      window.setTimeout(() => { setCopied(false) }, 1000)
    })
  }, [code])
  return (
    <div className={clsx(cardCss.card, css.code)} data-code-wrap={wrapped}>
      <CodeToolbar lang={language} labels={{ codeLabel: t('code'), wrapLabel: t('wrap'), unwrapLabel: t('unwrap') }}
        copyLabel={t('copy')} copiedLabel={t('copied')} copied={copied} wrapped={wrapped} onCopy={onCopy} onWrap={() => { setWrapped(value => !value) }}
        actions={actions ? <>
          <Tooltip label={t('openInEditor')} side="top" portal>
            <button type="button" className={css.action} onClick={() => { emit('editor.snippet', { code, language, compare: false }) }}>{t('openInEditor')}</button>
          </Tooltip>
          <Tooltip label={t('compareWithFile')} side="top" portal>
            <button type="button" className={css.action} onClick={() => { emit('editor.snippet', { code, language, compare: true }) }}>{t('compareWithFile')}</button>
          </Tooltip>
        </> : undefined} />
      <pre className={css.pre}><code>
        {lines === undefined ? code : lines.map((line, index) => (
          <span key={index} className={css.line}>
            {line.map((span, spanIndex) => <span key={spanIndex} className={span.className}>{span.text}</span>)}
            {index < lines.length - 1 ? '\n' : ''}
          </span>
        ))}
      </code></pre>
    </div>
  )
}

/**
 * Whether a link target is a local path the editor can open.
 * @param href Link target.
 * @returns The path and line, or `undefined` for web links.
 */
export function localTarget(href: string): { path: string; line?: number | undefined } | undefined {
  if (/^(https?|mailto):/i.test(href)) return undefined
  let path = href.replace(/^file:\/\//i, '')
  let line: number | undefined
  const anchor = /#L(\d+)(?:-L?\d+)?$/.exec(path) ?? /:(\d+)(?::\d+)?$/.exec(path)
  if (anchor !== null) { line = Number(anchor[1]); path = path.slice(0, anchor.index) }
  try { path = decodeURIComponent(path) } catch (_error) { /* A malformed escape keeps the literal path. */ }
  if (path === '' || path.startsWith('#')) return undefined
  return { path, ...(line === undefined ? {} : { line }) }
}

const components: Components = {
  pre({ children }) {
    const child = Children.toArray(children)[0]
    if (isValidElement<{ className?: string; children?: ReactNode }>(child)) {
      const language = /language-([\w+#.-]+)/.exec(child.props.className ?? '')?.[1]
      return <CodeBlock code={textOf(child.props.children).replace(/\n$/, '')} language={language} />
    }
    return <pre>{children}</pre>
  },
  a({ href, children }) {
    const target = href === undefined ? undefined : localTarget(href)
    if (target !== undefined) {
      return <a href={href} onClick={(event) => { event.preventDefault(); emit('editor.open', target) }}>{children}</a>
    }
    return <a href={href} target="_blank" rel="noreferrer">{children}</a>
  },
  table({ children }) {
    return <div className={css.tableScroll}><table>{children}</table></div>
  },
}

/** Rendered markdown; memoized because transcripts re-render while streaming. */
export const Markdown = memo(function Markdown({ text, className }: { text: string; className?: string | undefined }): ReactElement {
  return (
    <div className={clsx(css.markdown, className)}>
      <ReactMarkdown remarkPlugins={[remarkGfm, remarkMath]} rehypePlugins={[rehypeKatex]} components={components}>{text}</ReactMarkdown>
    </div>
  )
})
