/**
 * The project tour: what the AI wrote about how to run the project, what to read first, how its key features flow
 * through the code and where to make common changes, with every file and line one click from the editor.
 */
import type { ReactNode } from 'react'
import type { MapExplanation, TourPlace } from '../../shared/map-protocol.ts'
import { useLocale } from '../i18n.ts'
import { Button, IconSparkleRegular } from '../ui/index.ts'
import type { useMapT } from './messages.ts'
import css from './Tour.module.css'

type Translate = ReturnType<typeof useMapT>

/** Text with `code` spans, as the model writes identifiers and commands. */
export function Inline({ text }: { text: string }): JSX.Element {
  const parts = text.split('`')
  // An odd count of backticks leaves the last one, unclosed, as text.
  if (parts.length % 2 === 0) {
    const tail = parts.pop()!
    parts[parts.length - 1] += `\`${tail}`
  }
  return <>{parts.map((part, index) => index % 2 === 1
    ? <code key={index} className={css.code}>{part}</code>
    : <span key={index}>{part}</span>)}</>
}

/** The link to a place: a file opens in the editor at its line, a directory shows in the structure view. */
function PlaceLink({ place, isDirectory, onOpenFile, onShowDirectory }: {
  place: TourPlace
  isDirectory: (path: string) => boolean
  onOpenFile: (path: string, line?: number) => void
  onShowDirectory: (path: string) => void
}): JSX.Element | null {
  const path = place.path
  if (path === undefined) return null
  const directory = isDirectory(path)
  const name = path.slice(path.lastIndexOf('/') + 1)
  const label = directory ? `${path}/` : place.line === undefined ? name : `${name}:${place.line}`
  return (
    <button type="button" className={css.place} title={directory ? path : `${path}${place.line === undefined ? '' : `:${place.line}`}`}
      onClick={() => { if (directory) onShowDirectory(path); else onOpenFile(path, place.line) }}>
      {label}
    </button>
  )
}

/**
 * The tour of a project.
 * @param props The explanation (absent before the AI wrote one), whether it is being written, and actions.
 * @returns The tour, or an invitation to have it written.
 */
export function TourView({ explanation, explaining, isDirectory, onGenerate, onOpenFile, onShowDirectory, t }: {
  explanation: MapExplanation | undefined
  explaining: boolean
  /** Whether a project path is a directory rather than a file. */
  isDirectory: (path: string) => boolean
  onGenerate: () => void
  onOpenFile: (path: string, line?: number) => void
  onShowDirectory: (path: string) => void
  t: Translate
}): JSX.Element {
  const locale = useLocale()
  const generate = (label: string, variant: 'primary' | 'outline'): JSX.Element => (
    <Button size="sm" variant={variant} icon={<IconSparkleRegular size={14} />} disabled={explaining} onClick={onGenerate}>
      {explaining ? t('tourGenerating') : label}
    </Button>
  )
  if (explanation === undefined) {
    return (
      <div className={css.root} data-map-tour>
        <div className={css.invite}>
          <p>{t('tourIntro')}</p>
          {generate(t('tourGenerate'), 'primary')}
        </div>
      </div>
    )
  }
  const place = (item: TourPlace): ReactNode => (
    <PlaceLink place={item} isDirectory={isDirectory} onOpenFile={onOpenFile} onShowDirectory={onShowDirectory} />
  )
  const start = explanation.start ?? []
  const flows = explanation.flows ?? []
  const tasks = explanation.tasks ?? []
  const run = explanation.run ?? []
  const complete = start.length > 0 || flows.length > 0 || tasks.length > 0
  // The parts that are folders: single files and the loose files of a folder say less than the places above.
  const modules = Object.entries(explanation.modules).filter(([id]) => isDirectory(id))
  const time = new Date(explanation.generatedAt).toLocaleString(locale === 'zh' ? 'zh-CN' : 'en-US')
  return (
    <div className={css.root} data-map-tour>
      <article className={css.article}>
        <p className={css.summary}><Inline text={explanation.summary} /></p>
        <div className={css.meta}>
          <span>{t('tourMeta', { model: explanation.model, time })}</span>
          {explanation.drift !== undefined && <span className={css.drift}>{t('tourDrift', explanation.drift)}</span>}
          {generate(t('tourRegenerate'), 'outline')}
        </div>
        {!complete && <p className={css.notice}>{t('tourOld')}</p>}
        {run.length > 0 && <section>
          <h3 className={css.heading}>{t('tourRun')}</h3>
          <ul className={css.bullets}>{run.map(item => <li key={item}><Inline text={item} /></li>)}</ul>
        </section>}
        {start.length > 0 && <section>
          <h3 className={css.heading}>{t('tourStart')}</h3>
          <ol className={css.steps}>{start.map((item, index) => (
            <li key={index}>{place(item)}<span className={css.stepText}><Inline text={item.text} /></span></li>
          ))}</ol>
        </section>}
        {flows.length > 0 && <section>
          <h3 className={css.heading}>{t('tourFlows')}</h3>
          {flows.map(flow => (
            <div key={flow.title} className={css.flow}>
              <h4 className={css.flowTitle}><Inline text={flow.title} /></h4>
              <ol className={css.timeline}>{flow.steps.map((step, index) => (
                <li key={index}><span className={css.stepText}><Inline text={step.text} /></span>{place(step)}</li>
              ))}</ol>
            </div>
          ))}
        </section>}
        {tasks.length > 0 && <section>
          <h3 className={css.heading}>{t('tourTasks')}</h3>
          <dl className={css.tasks}>{tasks.map(task => (
            <div key={task.task} className={css.task}>
              <dt><Inline text={task.task} /></dt>
              {task.places.map((item, index) => (
                <dd key={index}>{place(item)}<span className={css.stepText}><Inline text={item.text} /></span>
                  {item.path === undefined && <span className={css.missing}>{t('tourNoFile')}</span>}</dd>
              ))}
            </div>
          ))}</dl>
        </section>}
        {modules.length > 0 && <section>
          <h3 className={css.heading}>{t('tourModules')}</h3>
          <dl className={css.modules}>{modules.map(([id, line]) => (
            <div key={id} className={css.module}>
              <dt><button type="button" className={css.place} title={t('tourModuleTitle')} onClick={() => { onShowDirectory(id) }}>{`${id}/`}</button></dt>
              <dd>{line}</dd>
            </div>
          ))}</dl>
        </section>}
      </article>
    </div>
  )
}
