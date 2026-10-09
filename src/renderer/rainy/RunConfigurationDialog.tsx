/** Named launch configuration editor backed by the current workspace's durable state. */
import { useEffect, useState } from 'react'
import { z } from 'zod'
import { Button, Modal } from '@deepseek-ai/dsh-client-ui-primitives'
import type { TranslateNS } from '@deepseek-ai/dsh-client-locale/client'
import type { IdeExecutionLanguage, IdeRunConfiguration } from '../ide-execution-protocol.ts'
import type { IdeModel, IdeState } from './ide-model.ts'
import { sourceLanguage } from './ide-model.ts'
import css from './IdeShell.module.css'
import { fileLabel, fileReference, workspaceRoots } from './ide-paths.ts'
import type { IdeRootId } from '../ide-files-protocol.ts'
import { Choice } from './Choice.tsx'
import { runLanguageNames, runTarget, runsFile, type RunFile } from './run-target.ts'

interface Props {
  readonly open: boolean
  readonly close: () => void
  readonly state: IdeState
  readonly model: IdeModel
  readonly t: TranslateNS<'rainy'>
}
const languages: readonly IdeExecutionLanguage[] = ['python', 'javascript', 'typescript', 'php', 'c', 'cpp']

/** Edit arguments, environment, executable, and single-file or CMake build settings.
 * @param props Workspace configurations and localized controls.
 * @returns A configuration dialog.
 */
export function RunConfigurationDialog({ open, close, state, model, t }: Props) {
  const [profile, setProfile] = useState<IdeRunConfiguration>({
    name: '',
    language: 'python',
    program: '',
    terminal: true,
  })
  const [argumentsText, setArguments] = useState('[]')
  const [environmentText, setEnvironment] = useState('{}')
  const [flagsText, setFlags] = useState('[]')
  const [buildKind, setBuildKind] = useState<'single-file' | 'cmake'>('single-file')
  const [buildDirectory, setBuildDirectory] = useState('build')
  const [target, setTarget] = useState('')
  const [executable, setExecutable] = useState('')
  const [configurePreset, setConfigurePreset] = useState('')
  const [buildPreset, setBuildPreset] = useState('')
  const [error, setError] = useState('')
  const load = (next: IdeRunConfiguration): void => {
    setProfile(next)
    setArguments(JSON.stringify(next.arguments ?? []))
    setEnvironment(JSON.stringify(next.environment ?? {}))
    setFlags(JSON.stringify(next.build?.kind === 'single-file' ? (next.build.flags ?? []) : []))
    setBuildKind(next.build?.kind ?? 'single-file')
    setBuildDirectory(next.build?.kind === 'cmake' ? next.build.buildDirectory : 'build')
    setTarget(next.build?.kind === 'cmake' ? next.build.target : '')
    setExecutable(next.build?.kind === 'cmake' ? next.build.executable : '')
    setConfigurePreset(next.build?.kind === 'cmake' ? (next.build.configurePreset ?? '') : '')
    setBuildPreset(next.build?.kind === 'cmake' ? (next.build.buildPreset ?? '') : '')
    setError('')
  }
  useEffect(() => {
    if (!open) return
    const path = state.data.activePath ?? ''
    const language = sourceLanguage(path)
    // The pinned profile, else the open file's saved or extension-derived settings.
    load(
      runTarget(state.data.execution, openFile())?.configuration ?? {
        name: fileLabel(state.workspace, path),
        program: fileReference(path).path,
        rootId: fileReference(path).rootId,
        language: languages.find(entry => entry === language) ?? 'python',
        terminal: true,
      },
    )
  }, [open])
  const openFile = (): RunFile | undefined => {
    const path = state.data.activePath
    if (path === null) return undefined
    const reference = fileReference(path)
    return { path, label: fileLabel(state.workspace, path), program: reference.path,
      ...reference.rootId === undefined ? {} : { rootId: reference.rootId } }
  }
  const field = (label: Parameters<Props['t']>[0], value: string, set: (value: string) => void) => (
    <label className={css.field}>
      {t(label)}
      <input
        className={css.input}
        value={value}
        onChange={(event) => {
          set(event.target.value)
        }}
      />
    </label>
  )
  const save = (): void => {
    try {
      const args: unknown = JSON.parse(argumentsText)
      const environment: unknown = JSON.parse(environmentText)
      const flags: unknown = JSON.parse(flagsText)
      const compiled = profile.language === 'c' || profile.language === 'cpp'
      if (
        profile.name.trim() === '' ||
        profile.program.trim() === '' ||
        (compiled && buildKind === 'cmake' && (buildDirectory === '' || target === '' || executable === ''))
      )
        throw new Error(t('ideProfileInvalid'))
      const configuration: IdeRunConfiguration = {
        ...profile,
        name: profile.name.trim(),
        program: profile.program.trim(),
        arguments: z.array(z.string()).parse(args),
        environment: z.record(z.string(), z.string()).parse(environment),
        build: !compiled
          ? undefined
          : buildKind === 'single-file'
            ? { kind: 'single-file', flags: z.array(z.string()).parse(flags) }
            : {
              kind: 'cmake',
              buildDirectory,
              target,
              executable,
              ...(configurePreset.trim() ? { configurePreset: configurePreset.trim() } : {}),
              ...(buildPreset.trim() ? { buildPreset: buildPreset.trim() } : {}),
            },
      }
      const current = state.data.execution ?? { profiles: [], activeProfile: null, breakpoints: [], watches: [] }
      const file = openFile()
      // Settings for the open file apply whenever it is open; a profile for another program is pinned until the user unpins it.
      model.execution({
        ...current,
        profiles: [...current.profiles.filter(entry => entry.name !== configuration.name), configuration],
        activeProfile: file !== undefined && runsFile(configuration, file) ? null : configuration.name,
      })
      close()
    } catch (error) {
      setError(error instanceof Error ? error.message : t('ideProfileInvalid'))
    }
  }
  return (
    <Modal
      open={open}
      className={`${css.runDialog}`}
      contentClassName={`${css.dialogContent}`}
      title={t('ideRunProfiles')}
      closeLabel={t('ideClose')}
      onClose={close}
      footer={
        <>
          <Button onClick={close}>{t('ideCancel')}</Button>
          <Button onClick={save}>{t('ideSave')}</Button>
        </>
      }
    >
      <div className={css.form}>
        {state.workspace !== null && workspaceRoots(state.workspace).length > 1 && <label className={css.field}>
          {t('ideWorkspace')}<Choice label={t('ideWorkspace')} value={profile.rootId ?? 'primary'}
            items={workspaceRoots(state.workspace).map(root => ({ id: root.rootId, label: root.title }))}
            onChange={(rootId) => { setProfile({ ...profile, rootId: rootId === 'primary' ? undefined : rootId as IdeRootId }) }} />
        </label>}
        {(state.data.execution?.profiles.length ?? 0) > 0 && (
          <Choice
            label={t('ideRunProfiles')}
            value={profile.name}
            items={[{ id: profile.name, label: profile.name }, ...(state.data.execution?.profiles ?? [])
              .filter(entry => entry.name !== profile.name).map(entry => ({ id: entry.name, label: entry.name }))]}
            onChange={(value) => {
              const selected = state.data.execution?.profiles.find(entry => entry.name === value)
              if (selected !== undefined) load(selected)
            }}
          />
        )}
        {field('ideRunProfile', profile.name, (name) => {
          setProfile({ ...profile, name })
        })}
        <label className={css.field}>
          {t('ideLanguage')}
          <Choice
            label={t('ideLanguage')}
            value={profile.language}
            items={languages.map(language => ({ id: language, label: runLanguageNames[language] }))}
            onChange={(value) => {
              const language = languages.find(language => language === value)
              if (language !== undefined) setProfile({ ...profile, language, pythonModule: language === 'python' ? profile.pythonModule : undefined })
            }}
          />
        </label>
        {field('ideProgram', profile.program, (program) => {
          setProfile({ ...profile, program })
        })}
        {(profile.language === 'c' || profile.language === 'cpp') && (
          <>
            <label className={css.field}>
              {t('ideBuild')}
              <Choice
                label={t('ideBuild')}
                value={buildKind}
                items={[{ id: 'single-file', label: t('ideSingleFile') }, { id: 'cmake', label: t('ideCmake') }]}
                onChange={(value) => {
                  setBuildKind(value === 'cmake' ? 'cmake' : 'single-file')
                }}
              />
            </label>
            {buildKind === 'cmake' && (
              <>
                {field('ideBuildDirectory', buildDirectory, setBuildDirectory)}
                {field('ideBuildTarget', target, setTarget)}
                {field('ideBuildExecutable', executable, setExecutable)}
              </>
            )}
          </>
        )}
        <details className={css.advanced} key={`${open}:${profile.language}`}>
          <summary>{t('ideAdvanced')}</summary>
          <div className={css.form}>
            {profile.language === 'python' && field('idePythonModule', profile.pythonModule ?? '', (pythonModule) => {
              setProfile({ ...profile, pythonModule: pythonModule || undefined })
            })}
            {field('ideCwd', profile.cwd ?? '', (cwd) => {
              setProfile({ ...profile, cwd: cwd || undefined })
            })}
            {field('ideArguments', argumentsText, setArguments)}
            {field('ideEnvironment', environmentText, setEnvironment)}
            {field('ideExecutable', profile.executable ?? '', (executable) => {
              setProfile({ ...profile, executable: executable || undefined })
            })}
            {(profile.language === 'c' || profile.language === 'cpp') && (
              buildKind === 'single-file' ? field('ideBuildFlags', flagsText, setFlags) : <>
                {field('ideConfigurePreset', configurePreset, setConfigurePreset)}
                {field('ideBuildPreset', buildPreset, setBuildPreset)}
              </>
            )}
          </div>
        </details>
        {error && (
          <div className={css.error} role="alert">
            {error}
          </div>
        )}
      </div>
    </Modal>
  )
}
