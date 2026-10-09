/** Run target selection: the open file by extension or a remembered per-file language, unless a profile is pinned. */
import type { IdeExecutionConfiguration, IdeExecutionLanguage, IdeRunConfiguration } from '../../shared/ide-execution-protocol.ts'
import type { IdeRootId } from '../../shared/ide-files-protocol.ts'
import { sourceLanguage } from './ide-model.ts'

/** Languages the run and debug commands accept, in menu order. */
export const runLanguages: readonly IdeExecutionLanguage[] = ['python', 'javascript', 'typescript', 'php', 'c', 'cpp']

/** Product names shown for each run language. */
export const runLanguageNames: Readonly<Record<IdeExecutionLanguage, string>> = {
  python: 'Python', javascript: 'JavaScript (Node.js)', typescript: 'TypeScript', php: 'PHP', c: 'C', cpp: 'C++',
}

/** The open file in the form a run configuration refers to it. */
export interface RunFile {
  readonly path: string
  readonly program: string
  readonly rootId?: IdeRootId
  readonly label: string
}

/** What the run button starts and why. */
export interface RunTarget {
  readonly configuration: IdeRunConfiguration
  /** pinned: an explicitly chosen profile; file: the open file's remembered settings; auto: the extension alone. */
  readonly mode: 'pinned' | 'file' | 'auto'
}

const empty: IdeExecutionConfiguration = { profiles: [], activeProfile: null, breakpoints: [], watches: [] }

/** @param path - file path. @returns the run language implied by its extension, if it is runnable. */
export function inferredRunLanguage(path: string): IdeExecutionLanguage | undefined {
  const language = sourceLanguage(path)
  return runLanguages.find(value => value === language)
}

/** @param profile - saved profile. @param file - open file. @returns whether the profile runs that file. */
export function runsFile(profile: IdeRunConfiguration, file: RunFile): boolean {
  return profile.program === file.program && profile.rootId === file.rootId
}

/**
 * Resolve the run button's configuration.
 * @param execution - workspace execution settings.
 * @param file - open file, if any.
 * @returns the pinned profile, the open file's saved settings, the extension's default, or undefined when nothing applies.
 */
export function runTarget(execution: IdeExecutionConfiguration | undefined, file: RunFile | undefined): RunTarget | undefined {
  const pinned = execution?.profiles.find(profile => profile.name === execution.activeProfile)
  if (pinned !== undefined) return { configuration: pinned, mode: 'pinned' }
  if (file === undefined) return undefined
  const saved = execution?.profiles.find(profile => runsFile(profile, file))
  if (saved !== undefined) return { configuration: saved, mode: 'file' }
  const language = inferredRunLanguage(file.path)
  if (language === undefined) return undefined
  return { configuration: { name: file.label, language, program: file.program, ...file.rootId === undefined ? {} : { rootId: file.rootId },
    terminal: true }, mode: 'auto' }
}

function withLanguage(profile: IdeRunConfiguration, language: IdeExecutionLanguage): IdeRunConfiguration {
  const { build, pythonModule, ...rest } = profile
  return { ...rest, language, ...build !== undefined && (language === 'c' || language === 'cpp') ? { build } : {},
    ...pythonModule !== undefined && language === 'python' ? { pythonModule } : {} }
}

function plain(profile: IdeRunConfiguration): boolean {
  return (profile.arguments?.length ?? 0) === 0 && Object.keys(profile.environment ?? {}).length === 0 && profile.executable === undefined
    && profile.build === undefined && profile.cwd === undefined && profile.pythonModule === undefined
}

/**
 * Run the open file with a chosen language, or return it to its extension's default, and unpin any profile.
 * @param execution - workspace execution settings.
 * @param file - open file.
 * @param choice - language to remember for this file, or 'auto'.
 * @returns updated settings; an automatic choice without other settings keeps no record.
 */
export function chooseFileLanguage(execution: IdeExecutionConfiguration | undefined, file: RunFile,
  choice: IdeExecutionLanguage | 'auto'): IdeExecutionConfiguration {
  const current = execution ?? empty
  const saved = current.profiles.find(profile => runsFile(profile, file))
  const others = current.profiles.filter(profile => profile !== saved)
  const language = choice === 'auto' ? inferredRunLanguage(file.path) : choice
  if (language === undefined) return { ...current, activeProfile: null }
  let name = file.label
  for (let suffix = 2; saved === undefined && others.some(profile => profile.name === name); suffix++) name = `${file.label} (${suffix})`
  const next = withLanguage(saved ?? { name, language, program: file.program, ...file.rootId === undefined ? {} : { rootId: file.rootId },
    terminal: true }, language)
  return { ...current, profiles: choice === 'auto' && plain(next) ? others : [...others, next], activeProfile: null }
}

/**
 * @param execution - workspace execution settings.
 * @param name - profile to pin, or null to run the open file.
 * @returns updated settings.
 */
export function pinRunProfile(execution: IdeExecutionConfiguration | undefined, name: string | null): IdeExecutionConfiguration {
  return { ...execution ?? empty, activeProfile: name }
}

/**
 * Select the Python interpreter sent to the workspace language server.
 * @param configuration Saved workspace run choices.
 * @returns The pinned Python profile's interpreter, else the first Python profile that names one.
 */
export function selectedPythonExecutable(configuration: IdeExecutionConfiguration | undefined): string | undefined {
  if (configuration === undefined) return undefined
  const active = configuration.profiles.find(profile => profile.name === configuration.activeProfile && profile.language === 'python')
  return active === undefined
    ? configuration.profiles.find(profile => profile.language === 'python' && profile.executable !== undefined)?.executable
    : active.executable
}
