/** CMake Debug commands and preset-directory checks for human workspace launches. */
import { z } from 'zod'
import type { IdeCommandSpec, IdeRunConfiguration } from '../../shared/ide-execution-protocol.ts'
import { SENSITIVE_ENV_PATTERN } from '../process.ts'
import { containsIdePath, idePathApi, resolveIdeExecutable, resolveIdeWorkspacePath } from './execution-resolve.ts'
import type { IdeRunResolverOptions } from './execution-resolve.ts'

const presetSchema = z
  .object({
    name: z.string(),
    inherits: z.union([z.string(), z.array(z.string())]).optional(),
    binaryDir: z.string().optional(),
    configurePreset: z.string().optional(),
    environment: z.record(z.string(), z.string().nullable()).optional(),
  })
  .loose()
type Preset = z.infer<typeof presetSchema>
const documentSchema = z
  .object({
    include: z.array(z.string()).optional(),
    configurePresets: z.array(presetSchema).optional(),
    buildPresets: z.array(presetSchema).optional(),
  })
  .loose()

interface Presets {
  configure: Map<string, Preset>
  build: Map<string, Preset>
}

async function readPresets(options: IdeRunResolverOptions, root: string): Promise<Presets> {
  const result: Presets = { configure: new Map(), build: new Map() }
  const loaded = new Set<string>()
  const loading = new Set<string>()
  let bytes = 0
  const visit = async (path: string): Promise<void> => {
    const canonical = await resolveIdeWorkspacePath(options.files, root, path, 'file')
    if (loading.has(canonical)) throw new Error('CMake preset includes contain a cycle.')
    if (loaded.has(canonical)) return
    loading.add(canonical)
    const text = await options.files.readText(canonical)
    bytes += Buffer.byteLength(text)
    if (bytes > options.maxConfigurationBytes) throw new Error('CMake preset files exceed the configured size limit.')
    const data = documentSchema.parse(JSON.parse(text))
    for (const include of data.include ?? []) {
      if (include.includes('$')) throw new Error('CMake preset include paths must be explicit workspace paths.')
      await visit(idePathApi(root).resolve(idePathApi(root).dirname(canonical), include))
    }
    for (const [kind, values] of [
      ['configure', data.configurePresets],
      ['build', data.buildPresets],
    ] as const) {
      for (const preset of values ?? []) {
        if (result[kind].has(preset.name)) throw new Error(`Duplicate CMake ${kind} preset: ${preset.name}`)
        result[kind].set(preset.name, preset)
      }
    }
    loading.delete(canonical)
    loaded.add(canonical)
  }
  for (const name of ['CMakePresets.json', 'CMakeUserPresets.json']) {
    const path = idePathApi(root).join(root, name)
    if ((await options.files.kind(path)) === 'file') await visit(path)
  }
  return result
}

function inherited(name: string, presets: Map<string, Preset>, chain = new Set<string>()): Preset {
  if (chain.has(name)) throw new Error('CMake preset inheritance contains a cycle.')
  const value = presets.get(name)
  if (!value) throw new Error(`CMake preset not found: ${name}`)
  const next = new Set([...chain, name])
  const parents = typeof value.inherits === 'string' ? [value.inherits] : (value.inherits ?? [])
  let combined: Preset = { name }
  for (const parent of [...parents].reverse()) {
    const base = inherited(parent, presets, next)
    combined = { ...combined, ...base, environment: { ...combined.environment, ...base.environment } }
  }
  return { ...combined, ...value, environment: { ...combined.environment, ...value.environment } }
}

function expandDirectory(value: string, root: string, preset: Preset, explicit: Readonly<Record<string, string>>): string {
  const environment: Record<string, string> = {}
  for (const [key, contents] of Object.entries(process.env))
    if (contents !== undefined && !SENSITIVE_ENV_PATTERN.test(key) && !/^DSH_/i.test(key)) environment[key] = contents
  Object.assign(environment, explicit)
  const expanding = new Set<string>()
  const expand = (input: string): string => {
    const replaced = input.replace(
      /\$\{(sourceDir|sourceParentDir|sourceDirName|presetName)\}|\$(p?env)\{([^}]+)\}/g,
      (_match, macro: string | undefined, environmentKind: string | undefined, key: string | undefined) => {
        if (macro === 'sourceDir') return root
        if (macro === 'sourceParentDir') return idePathApi(root).dirname(root)
        if (macro === 'sourceDirName') return idePathApi(root).basename(root)
        if (macro === 'presetName') return preset.name
        if (!key) throw new Error('Invalid CMake preset macro.')
        if (environmentKind === 'penv') return environment[key] ?? ''
        if (expanding.has(key)) throw new Error('CMake preset environment contains a cycle.')
        const own = preset.environment?.[key]
        if (own === null) return ''
        if (own === undefined) return environment[key] ?? ''
        expanding.add(key)
        const result = expand(own)
        expanding.delete(key)
        return result
      },
    )
    if (replaced.includes('$')) throw new Error('The CMake build directory contains an unsupported preset macro.')
    return replaced
  }
  return idePathApi(root).resolve(root, expand(value))
}

/** Resolved CMake commands and their expected executable. */
export interface ResolvedIdeCmakeBuild {
  readonly commands: readonly IdeCommandSpec[]
  readonly directory: string
  readonly executable: string
}

/**
 * Prepare Debug configure/build commands; build presets must identify the checked build directory.
 * @param options - execution-world providers.
 * @param root - canonical workspace.
 * @param cwd - debuggee working directory.
 * @param configuration - requested C/C++ configuration.
 * @param environment - explicit child environment.
 * @returns build commands and the contained output path.
 */
export async function resolveCmakeBuild(
  options: IdeRunResolverOptions,
  root: string,
  cwd: string,
  configuration: IdeRunConfiguration,
  environment: Readonly<Record<string, string>>,
): Promise<ResolvedIdeCmakeBuild> {
  void cwd
  const build = configuration.build
  if (build?.kind !== 'cmake') throw new Error('A CMake build configuration is required.')
  await resolveIdeWorkspacePath(options.files, root, 'CMakeLists.txt', 'file')
  const directory = await resolveIdeWorkspacePath(options.files, root, build.buildDirectory)
  if (directory === root) throw new Error('Choose a separate CMake build directory inside the workspace.')
  const executable = await resolveIdeWorkspacePath(options.files, root, idePathApi(root).resolve(directory, build.executable))
  if (!containsIdePath(directory, executable) || executable === directory)
    throw new Error('The CMake executable must be inside its build directory.')
  const cmake = await resolveIdeExecutable(options.subprocess, undefined, 'cmake', environment)
  const compiler =
    configuration.executable === undefined
      ? undefined
      : await resolveIdeExecutable(
        options.subprocess,
        configuration.executable,
        configuration.language === 'c' ? 'gcc' : 'g++',
        environment,
      )
  const compilerArguments = compiler === undefined ? [] : [`-DCMAKE_${configuration.language === 'c' ? 'C' : 'CXX'}_COMPILER=${compiler}`]
  if (build.buildPreset && !build.configurePreset) throw new Error('A build preset requires its configure preset.')
  if (build.configurePreset || build.buildPreset) {
    const presets = await readPresets(options, root)
    if (build.configurePreset) inherited(build.configurePreset, presets.configure)
    if (build.buildPreset) {
      const preset = inherited(build.buildPreset, presets.build)
      if (preset.configurePreset !== build.configurePreset)
        throw new Error('The selected CMake build preset uses another configure preset.')
      const configure = inherited(build.configurePreset ?? '', presets.configure)
      if (!configure.binaryDir) throw new Error('The CMake configure preset must declare its build directory.')
      const declared = await resolveIdeWorkspacePath(
        options.files,
        root,
        expandDirectory(configure.binaryDir, root, configure, environment),
      )
      if (declared !== directory) throw new Error('The CMake preset build directory differs from the selected build directory.')
    }
  }
  const configureArgs = [
    cmake,
    ...(build.configurePreset ? ['--preset', build.configurePreset] : []),
    '-S',
    root,
    '-B',
    directory,
    ...compilerArguments,
    '-DCMAKE_BUILD_TYPE=Debug',
    '-DCMAKE_EXPORT_COMPILE_COMMANDS=ON',
  ]
  const buildArgs = [
    cmake,
    '--build',
    ...(build.buildPreset ? ['--preset', build.buildPreset] : [directory]),
    '--config',
    'Debug',
    '--target',
    build.target,
  ]
  return {
    directory,
    executable,
    commands: [
      { argv: configureArgs, cwd: root, environment },
      { argv: buildArgs, cwd: root, environment },
    ],
  }
}
