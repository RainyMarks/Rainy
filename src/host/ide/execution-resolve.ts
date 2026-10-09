/** Resolve human launch choices into explicit native commands and workspace-contained source paths. */
import { randomUUID } from 'node:crypto'
import { lstat, mkdir, readFile, realpath, rm, stat, unlink } from 'node:fs/promises'
import { posix, win32 } from 'node:path'
import { assertNever } from '../../shared/brand.ts'
import type { IdeCommandSpec, IdeResolvedRunSpec, IdeRunConfiguration } from '../../shared/ide-execution-protocol.ts'
import type { IdeRootId, WorkspaceId } from '../../shared/ide-files-protocol.ts'
import type { ResolvedWorkspaceEnvironment } from '../runtime/environments.ts'
import { resolveCmakeBuild } from './execution-cmake.ts'
import type { IdeSubprocess } from './execution-process.ts'
import { ideRunConfigurationSchema } from './execution-schema.ts'

/** Absolute paths supplied by the packaged IDE resource resolver. */
export interface IdeExecutionResources {
  readonly resourceRoot: string
  readonly node: string
  readonly tsxImport: string
}

/** Canonical project directory resolved from the project catalog. */
export interface IdeExecutionWorkspace {
  readonly workspaceId: WorkspaceId
  readonly root: string
}

/** Filesystem operations in the Host's execution world. */
export interface IdeExecutionFiles {
  /** Canonicalize an existing path. @param path - absolute native path. @returns its real path. */
  realpath(path: string): Promise<string>
  /** Inspect a path. @param path - absolute native path. @returns its kind or absent. */
  kind(path: string): Promise<'file' | 'directory' | 'other' | undefined>
  /** Read a project text file. @param path - absolute native path. @returns UTF-8 text. */
  readText(path: string): Promise<string>
  /**
   * Create a private owned build directory under a workspace.
   * @param root - workspace root.
   * @param path - exclusive child.
   * @returns creation completion.
   */
  createBuildDirectory(root: string, path: string): Promise<void>
  /**
   * Remove an owned private build directory without following a replaced root link.
   * @param root - workspace root.
   * @param path - owned child.
   * @returns cleanup completion.
   */
  removeBuildDirectory(root: string, path: string): Promise<void>
}

/** Dependencies needed by the pure command-resolution layer. */
export interface IdeRunResolverOptions {
  readonly subprocess: Pick<IdeSubprocess, 'resolveExecutable' | 'terminalEnvironment'>
  readonly resources: IdeExecutionResources
  readonly files: IdeExecutionFiles
  readonly resolveWorkspace: (id: WorkspaceId, rootId?: IdeRootId) => Promise<IdeExecutionWorkspace>
  readonly resolveEnvironment?: (id: WorkspaceId) => ResolvedWorkspaceEnvironment
  readonly maxConfigurationBytes: number
}

/** A resolved command plus private build-directory ownership and normalized caller choices. */
export interface ResolvedIdeRun {
  readonly spec: IdeResolvedRunSpec
  readonly configuration: IdeRunConfiguration
  readonly ownedBuildDirectory?: string
}

function absent(error: unknown): boolean {
  return error instanceof Error && 'code' in error && error.code === 'ENOENT'
}

/**
 * Check canonical POSIX containment, including the root itself.
 * @param root - canonical parent.
 * @param path - canonical candidate.
 * @returns whether the candidate is contained.
 */
export function containsIdePath(root: string, path: string): boolean {
  const api = idePathApi(root)
  const relative = api.relative(root, path)
  return relative === '' || (!api.isAbsolute(relative) && relative !== '..' && !relative.startsWith(`..${api.sep}`))
}

/** @param root - absolute path in the execution world. @returns the matching host path operations. */
export function idePathApi(root: string): typeof posix { return /^[A-Za-z]:[\\/]|^\\\\/u.test(root) ? win32 : posix }

function buildDirectory(root: string, path: string): void {
  const relative = idePathApi(root).relative(root, path).replaceAll('\\', '/')
  if (!/^\.rainy-ide\/build\/[0-9a-f-]{36}$/.test(relative)) throw new Error('Invalid owned IDE build directory.')
}

/** Node filesystem implementation for the selected native Host. */
export const nodeIdeExecutionFiles: IdeExecutionFiles = {
  realpath,
  async kind(path) {
    try {
      const info = await stat(path)
      return info.isFile() ? 'file' : info.isDirectory() ? 'directory' : 'other'
    } catch (error) {
      if (absent(error)) {
        try {
          return (await lstat(path)).isSymbolicLink() ? 'other' : undefined
        } catch (linkError) {
          if (absent(linkError)) return undefined
          throw linkError
        }
      }
      throw error
    }
  },
  readText: path => readFile(path, 'utf8'),
  async createBuildDirectory(root, path) {
    buildDirectory(root, path)
    const api = idePathApi(root)
    const parent = api.dirname(path)
    for (const candidate of [api.join(root, '.rainy-ide'), parent]) {
      let info
      try {
        info = await lstat(candidate)
      } catch (error) {
        if (!absent(error)) throw error
      }
      if (info?.isSymbolicLink()) throw new Error('The IDE build directory cannot be a symbolic link.')
      await mkdir(candidate, { mode: 0o700, recursive: true })
      if (!containsIdePath(root, await realpath(candidate))) throw new Error('The IDE build directory escapes the workspace.')
    }
    await mkdir(path, { mode: 0o700 })
  },
  async removeBuildDirectory(root, path) {
    buildDirectory(root, path)
    let parent: string
    try {
      parent = await realpath(idePathApi(root).dirname(path))
    } catch (error) {
      if (absent(error)) return
      throw error
    }
    if (!containsIdePath(root, parent)) throw new Error('The IDE build parent moved outside the workspace.')
    let info
    try {
      info = await lstat(path)
    } catch (error) {
      if (absent(error)) return
      throw error
    }
    if (info.isSymbolicLink()) {
      await unlink(path)
      return
    }
    if (!containsIdePath(root, await realpath(path))) throw new Error('The IDE build directory moved outside the workspace.')
    await rm(path, { recursive: true })
  },
}

/**
 * Validate a native absolute executable path without translating between execution worlds.
 * @param path - proposed absolute native path.
 * @returns the accepted path.
 */
export function absoluteIdePath(path: string): string {
  const api = idePathApi(path)
  if (!api.isAbsolute(path) || path.includes('\0') || api === posix && path.includes('\\')) throw new Error('IDE execution requires an absolute native path.')
  return api.normalize(path)
}

/**
 * Resolve a workspace-relative input and reject symlink escapes.
 * @param files - selected-target filesystem.
 * @param root - canonical workspace.
 * @param value - relative or absolute workspace path.
 * @param expected - required existing kind, or absent for an output path.
 * @returns canonical or contained future path.
 */
export async function resolveIdeWorkspacePath(
  files: IdeExecutionFiles,
  root: string,
  value: string,
  expected?: 'file' | 'directory',
): Promise<string> {
  const api = idePathApi(root)
  if (!value || value.includes('\0') || api === posix && (value.includes('\\') || /^[A-Za-z]:/.test(value)))
    throw new Error('Use a path in the selected workspace execution target.')
  if (api === win32 && value.startsWith('/') && !value.startsWith('//')) throw new Error('A Linux path cannot be used in a Windows workspace.')
  const candidate = api.resolve(root, value)
  if (!containsIdePath(root, candidate)) throw new Error('The requested path is outside the selected workspace.')
  const kind = await files.kind(candidate)
  if (kind !== undefined) {
    const canonical = absoluteIdePath(await files.realpath(candidate))
    if (!containsIdePath(root, canonical)) throw new Error('The requested path resolves outside the selected workspace.')
    if (expected && kind !== expected) throw new Error(`The requested path must be a ${expected}.`)
    return canonical
  }
  if (expected) throw new Error(`The requested ${expected} does not exist: ${value}`)
  let parent = api.dirname(candidate)
  while ((await files.kind(parent)) === undefined) {
    const next = api.dirname(parent)
    if (next === parent) throw new Error('The workspace filesystem root is unavailable.')
    parent = next
  }
  const canonicalParent = absoluteIdePath(await files.realpath(parent))
  if (!containsIdePath(root, canonicalParent)) throw new Error('The output path resolves outside the selected workspace.')
  return candidate
}

/**
 * Resolve a configured executable or an explicit language default.
 * @param provider - process provider.
 * @param configured - caller-supplied absolute path.
 * @param fallback - resolver-owned language command.
 * @param environment - deliberate child environment.
 * @returns canonical executable path.
 */
export async function resolveIdeExecutable(
  provider: Pick<IdeSubprocess, 'resolveExecutable'>,
  configured: string | undefined,
  fallback: string,
  environment: Readonly<Record<string, string>>,
): Promise<string> {
  if (configured !== undefined) absoluteIdePath(configured)
  return absoluteIdePath(await provider.resolveExecutable(configured ?? fallback, environment))
}

/**
 * Resolve one launch without spawning or creating files.
 * @param options - native providers and resources.
 * @param workspaceId - selected registered workspace.
 * @param input - validated human configuration.
 * @returns explicit build and launch commands.
 */
export async function resolveIdeRun(
  options: IdeRunResolverOptions,
  workspaceId: WorkspaceId,
  input: IdeRunConfiguration,
): Promise<ResolvedIdeRun> {
  const configuration = ideRunConfigurationSchema.parse(input)
  const world = await options.subprocess.terminalEnvironment()
  const workspace = await options.resolveWorkspace(workspaceId, configuration.rootId)
  const root = absoluteIdePath(await options.files.realpath(absoluteIdePath(workspace.root)))
  const pathApi = idePathApi(root)
  if ((world.platform === 'windows') !== (pathApi === win32)) throw new Error('The workspace path belongs to another execution target.')
  if (configuration.executable && idePathApi(configuration.executable) !== pathApi) throw new Error('The configured executable belongs to another execution target.')
  if ((await options.files.kind(root)) !== 'directory') throw new Error('The selected workspace is not a directory.')
  const cwd = await resolveIdeWorkspacePath(options.files, root, configuration.cwd ?? '.', 'directory')
  const runtime = options.resolveEnvironment?.(workspaceId)
  const environment: Readonly<Record<string, string>> = { ...runtime?.environment, ...(configuration.environment ?? {}) }
  const arguments_ = [...(configuration.arguments ?? [])]
  const program = await resolveIdeWorkspacePath(options.files, root, configuration.program, 'file')
  let launch: IdeCommandSpec
  let build: readonly IdeCommandSpec[] = []
  let ownedBuildDirectory: string | undefined
  let compiledDirectory: string | undefined
  switch (configuration.language) {
    case 'python': {
      const executable = await resolveIdeExecutable(options.subprocess, configuration.executable ?? runtime?.executables.python,
        world.platform === 'windows' ? 'python.exe' : 'python3', environment)
      launch = {
        argv: [executable, '-u', ...(configuration.pythonModule ? ['-m', configuration.pythonModule] : [program]), ...arguments_],
        cwd,
        environment,
      }
      break
    }
    case 'javascript':
    case 'typescript': {
      const executable = await resolveIdeExecutable(
        options.subprocess,
        configuration.executable ?? runtime?.executables.node,
        absoluteIdePath(options.resources.node),
        environment,
      )
      const runtimeArguments = ['--enable-source-maps']
      if (configuration.language === 'typescript') {
        const loader = absoluteIdePath(options.resources.tsxImport)
        if ((await options.files.kind(loader)) !== 'file') throw new Error('The bundled TypeScript execution loader is unavailable.')
        runtimeArguments.push('--import', loader)
      }
      launch = { argv: [executable, ...runtimeArguments, program, ...arguments_], cwd, environment }
      break
    }
    case 'php': {
      let executable: string
      try {
        executable = await resolveIdeExecutable(options.subprocess, configuration.executable ?? runtime?.executables.php,
          world.platform === 'windows' ? 'php.exe' : 'php', environment)
      } catch (error) {
        if (configuration.executable !== undefined || world.platform !== 'windows') throw error
        throw new Error('未找到 PHP。请在「设置 → 运行环境 → 可选组件」中下载 PHP，或在运行方式中指定已安装的 php.exe。', { cause: error })
      }
      launch = { argv: [executable, program, ...arguments_], cwd, environment }
      break
    }
    case 'c':
    case 'cpp': {
      if (configuration.build?.kind === 'cmake') {
        const executable = configuration.executable ?? runtime?.executables[configuration.language]
        const configured = executable === undefined ? configuration : { ...configuration, executable }
        const resolved = await resolveCmakeBuild(options, root, cwd, configured, environment)
        build = resolved.commands
        compiledDirectory = resolved.directory
        launch = { argv: [resolved.executable, ...arguments_], cwd, environment }
      } else {
        const compiler = await resolveIdeExecutable(
          options.subprocess,
          configuration.executable ?? runtime?.executables[configuration.language],
          configuration.language === 'c' ? 'gcc' : 'g++',
          environment,
        )
        ownedBuildDirectory = await resolveIdeWorkspacePath(options.files, root, `.rainy-ide/build/${randomUUID()}`)
        const executable = pathApi.join(ownedBuildDirectory, world.platform === 'windows' ? 'program.exe' : 'program')
        const flags = configuration.build?.kind === 'single-file' ? (configuration.build.flags ?? []) : []
        build = [{ argv: [compiler, '-g', '-O0', '-fno-omit-frame-pointer', ...flags, program, '-o', executable], cwd, environment }]
        compiledDirectory = ownedBuildDirectory
        launch = { argv: [executable, ...arguments_], cwd, environment }
      }
      break
    }
    default:
      return assertNever(configuration.language)
  }
  return {
    configuration,
    ...(ownedBuildDirectory ? { ownedBuildDirectory } : {}),
    spec: {
      ...(configuration.rootId === undefined ? {} : { rootId: configuration.rootId }),
      workspaceId,
      workspaceRoot: root,
      language: configuration.language,
      program,
      name: configuration.name,
      launch,
      build,
      ...(compiledDirectory ? { buildDirectory: compiledDirectory } : {}),
      terminal: configuration.terminal ?? true,
    },
  }
}
