/** Existing interpreter discovery, functional probes and workspace-owned environment selection. */
import { execFile } from 'node:child_process'
import { createHash, randomUUID } from 'node:crypto'
import { access, mkdir, open, readFile, readdir, rename, rm, stat } from 'node:fs/promises'
import { constants, existsSync } from 'node:fs'
import { delimiter, dirname, isAbsolute, join, relative, resolve, sep } from 'node:path'
import { homedir } from 'node:os'
import { promisify } from 'node:util'
import { z } from 'zod'
import { brandString } from '../../shared/brand.ts'
import type { WorkspaceId } from '../../shared/ide-files-protocol.ts'
import { ExecutionTargetId } from '../../shared/project-registry.ts'
import type { RuntimeCandidate, RuntimeEnvironmentId, RuntimeLanguage, RuntimePlatform, RuntimeSnapshot } from '../../shared/runtime-protocol.ts'
import { scrubbedEnv } from '../process.ts'

const execute = promisify(execFile)
const languages = ['python', 'node', 'php', 'c', 'cpp'] as const
const languageSchema = z.enum(languages)
const pathSchema = z.string().min(1).max(32768).refine(value => isAbsolute(value) && !value.includes('\0'))
const selectionSchema = z.object({ version: z.literal(1), workspaces: z.record(z.string(), z.object({
  root: pathSchema, selected: z.partialRecord(languageSchema, pathSchema),
  pythonPrefix: pathSchema.optional(),
}).strict()) }).strict()
type SelectionState = z.infer<typeof selectionSchema>

/** One bounded process used for interpreter inspection. */
export interface RuntimeProbeCommand {
  executable: string
  arguments: readonly string[]
  cwd?: string
  environment?: Record<string, string>
  timeoutMs: number
}
/** @param command - direct argv and bounded output. @returns complete standard output from a successful probe. */
export type RuntimeProbeRunner = (command: RuntimeProbeCommand) => Promise<string>
/** Interpreter discovery limits and owned persistence location. */
export interface RuntimeEnvironmentOptions {
  root: string
  targetId: string
  platform?: RuntimePlatform
  bundledRoot?: string
  /** Interpreters shipped with the carrier; installed components and workspace selections take precedence. */
  builtinExecutables?: Partial<Record<RuntimeLanguage, string>>
  probeTimeoutMs: number
  maxCandidates: number
  /** Probes run at the same time; each candidate is an independent process. */
  probeConcurrency?: number
  run?: RuntimeProbeRunner
  resolveWorkspace: (workspaceId: WorkspaceId) => { path: string } | undefined
}
/** Explicit execution values shared by tools, IDE and language services. */
export interface ResolvedWorkspaceEnvironment {
  environment: Record<string, string>
  executables: Partial<Record<RuntimeLanguage, string>>
}

const pythonProbe = `import sys,json,platform,importlib
checks=[]
def test(name,fn):
 try: fn(); checks.append({'name':name,'ready':True})
 except Exception as error: checks.append({'name':name,'ready':False,'detail':str(error)[:300]})
test('python',lambda:compile('x=1+1','<runtime-probe>','exec'))
test('pip',lambda:importlib.import_module('pip'))
test('numpy',lambda:importlib.import_module('numpy').dot([1,2],[3,4]))
test('pandas',lambda:importlib.import_module('pandas').DataFrame({'x':[1,2]}).sum())
test('scipy',lambda:importlib.import_module('scipy.linalg').det([[1,0],[0,1]]))
test('scikit-learn',lambda:importlib.import_module('sklearn.tree').DecisionTreeClassifier().fit([[0],[1]],[0,1]))
test('pillow',lambda:importlib.import_module('PIL.Image').new('RGB',(2,2)).getpixel((0,0)))
def cpu():
 torch=importlib.import_module('torch'); x=torch.ones(2,requires_grad=True); (x*x).sum().backward()
def cuda():
 torch=importlib.import_module('torch')
 if not torch.cuda.is_available(): raise RuntimeError('CUDA is not available in this environment')
 x=torch.ones((2,2),device='cuda',requires_grad=True); result=(x@x).sum(); result.backward(); value=result.item()
 if value!=8: raise RuntimeError('CUDA computation returned an unexpected result')
 if x.grad.sum().item()!=16: raise RuntimeError('CUDA backward computation returned an unexpected result')
def vision():
 torch=importlib.import_module('torch'); module=importlib.import_module('torchvision')
 module.ops.nms(torch.tensor([[0.,0.,2.,2.],[0.,0.,1.,1.]]),torch.tensor([0.9,0.8]),0.5)
def audio():
 torch=importlib.import_module('torch'); module=importlib.import_module('torchaudio')
 module.functional.resample(torch.ones(1,160),16000,8000)
test('torch-cpu',cpu)
test('torch-cuda',cuda)
test('torchvision',vision)
test('torchaudio',audio)
for name in ['sympy','matplotlib','cv2','transformers','datasets','accelerate','safetensors','tokenizers','sentencepiece']:
 test(name,lambda name=name:importlib.import_module(name))
print(json.dumps({'version':platform.python_version(),'platform':'windows' if sys.platform=='win32' else 'linux' if sys.platform.startswith('linux') else sys.platform,'executable':sys.executable,'prefix':sys.prefix,'capabilities':checks}))`
const nodeProbe = "console.log(JSON.stringify({version:process.versions.node,platform:process.platform==='win32'?'windows':process.platform,executable:process.execPath,capabilities:[{name:'node',ready:2+2===4}]}))"
const phpProbe = "echo json_encode(['version'=>PHP_VERSION,'platform'=>PHP_OS_FAMILY==='Windows'?'windows':strtolower(PHP_OS_FAMILY),'executable'=>PHP_BINARY,'capabilities'=>array_map(fn($name)=>['name'=>$name,'ready'=>extension_loaded($name)],['json','openssl','mbstring','pdo','pdo_sqlite','curl','zip'])]);"
const probeResultSchema = z.object({
  version: z.string().min(1), platform: z.string(), executable: z.string().min(1), prefix: z.string().optional(),
  capabilities: z.array(z.object({ name: z.string(), ready: z.boolean(), detail: z.string().optional() }).strict()) }).strict()

function hostPlatform(): RuntimePlatform { return process.platform === 'win32' ? 'windows' : 'linux' }
function inside(root: string, path: string): boolean {
  const value = relative(root, path)
  return value === '' || (!isAbsolute(value) && value !== '..' && !value.startsWith(`..${sep}`))
}
function commandName(language: RuntimeLanguage, platform: RuntimePlatform): string {
  const name = { python: platform === 'windows' ? 'python' : 'python3', node: 'node', php: 'php', c: 'gcc', cpp: 'g++' }[language]
  return platform === 'windows' ? `${name}.exe` : name
}
function identity(target: string, language: RuntimeLanguage, path: string): RuntimeEnvironmentId {
  return brandString<RuntimeEnvironmentId>(createHash('sha256').update(JSON.stringify([target, language, path])).digest('hex'))
}

/**
 * Probe an interpreter by executing bounded local computations, without installing or modifying its packages.
 * @param options - execution world, candidate and bounded command runner.
 * @returns separate executable and library observations; absence of optional libraries does not disable the interpreter.
 */
export async function probeRuntimeCandidate(options: {
  targetId: string
  platform: RuntimePlatform
  language: RuntimeLanguage
  path: string
  source: RuntimeCandidate['source']
  timeoutMs: number
  run: RuntimeProbeRunner
}): Promise<RuntimeCandidate> {
  const candidate: RuntimeCandidate = { id: identity(options.targetId, options.language, options.path), language: options.language,
    path: options.path, source: options.source, platform: options.platform, version: null, ready: false, capabilities: [] }
  try {
    pathSchema.parse(options.path)
    const arguments_ = options.language === 'python' ? ['-I', '-B', '-c', pythonProbe]
      : options.language === 'node' ? ['-e', nodeProbe] : options.language === 'php' ? ['-r', phpProbe] : ['--version']
    const output = await options.run({ executable: options.path, arguments: arguments_, timeoutMs: options.timeoutMs,
      ...(options.language === 'php' && options.platform === 'windows' ? { environment: { RAINY_PHP_EXTENSION_DIR: join(dirname(options.path), 'ext') } } : {}) })
    if (options.language === 'c' || options.language === 'cpp') {
      const version = output.split(/\r?\n/u).find(line => line.trim())
      if (!version || !/gcc|g\+\+|clang|LLVM/i.test(version)) throw new Error('The executable did not identify a supported C/C++ compiler.')
      const triple = (await options.run({ executable: options.path, arguments: ['-dumpmachine'], timeoutMs: options.timeoutMs })).trim()
      const compilerPlatform = /windows|mingw|cygwin|msvc/iu.test(triple) ? 'windows' : /linux/iu.test(triple) ? 'linux' : undefined
      if (compilerPlatform !== options.platform) throw new Error('The compiler produces programs for another execution platform.')
      return { ...candidate, version, ready: true, capabilities: [{ name: 'compiler-start', ready: true }] }
    }
    const result = probeResultSchema.parse(JSON.parse(output.trim()))
    if (result.platform !== options.platform) throw new Error(`The interpreter runs on ${result.platform}, but this workspace runs on ${options.platform}.`)
    return { ...candidate, path: result.executable, version: result.version, ready: true, capabilities: result.capabilities,
      ...(result.prefix === undefined ? {} : { prefix: result.prefix }) }
  } catch (error) {
    return { ...candidate, error: error instanceof Error ? error.message.slice(0, 1000) : 'Interpreter inspection failed.' }
  }
}

/** Workspace environment owner; all mutations are limited to Rainy's own selection record. */
export class RuntimeEnvironments {
  private state: SelectionState = { version: 1, workspaces: {} }
  private loaded = false
  private pending: Promise<unknown> = Promise.resolve()
  private readonly observations = new Map<string, RuntimeCandidate[]>()
  private readonly platform: RuntimePlatform
  private readonly run: RuntimeProbeRunner
  private readonly target: ReturnType<typeof ExecutionTargetId>
  private componentRoots: { id: string; path: string }[] = []
  private bundledExecutables: Partial<Record<RuntimeLanguage, string>> = {}

  /** @param options - fixed storage, platform, limits and workspace authority. */
  constructor(private readonly options: RuntimeEnvironmentOptions) {
    this.platform = options.platform ?? hostPlatform()
    this.target = ExecutionTargetId(options.targetId)
    this.run = options.run ?? (async command => (await execute(command.executable, [...command.arguments], {
      cwd: command.cwd, env: { ...scrubbedEnv(), PYTHONDONTWRITEBYTECODE: '1', HF_HUB_OFFLINE: '1', TRANSFORMERS_OFFLINE: '1', ...command.environment },
      timeout: command.timeoutMs, maxBuffer: 256 * 1024, windowsHide: true, encoding: 'utf8',
    })).stdout)
  }

  /** @returns completion after the selection record is validated; malformed state is never discarded. */
  async initialize(): Promise<void> {
    try { this.state = selectionSchema.parse(JSON.parse(await readFile(join(this.options.root, 'runtime-environments.json'), 'utf8'))) }
    catch (error) { if (!(error instanceof Error && 'code' in error && error.code === 'ENOENT')) throw error }
    this.loaded = true
    await this.refreshComponents()
  }

  private async refreshComponents(): Promise<void> {
    const root = this.options.bundledRoot
    if (!root) return
    let active: Record<string, string>
    try { active = z.record(z.string(), z.string()).parse(JSON.parse(await readFile(join(root, 'active.json'), 'utf8'))) }
    catch (error) { if (error instanceof Error && 'code' in error && error.code === 'ENOENT') { this.componentRoots = []; this.bundledExecutables = {}; return }; throw error }
    const entries: { id: string; path: string }[] = []
    const defaults: Partial<Record<RuntimeLanguage, string>> = {}
    for (const [id, spelling] of Object.entries(active)) {
      if (!/^(?:windows|linux)-(?:basic|science-cpu|science-cuda|cpp|development)$/u.test(id)
        || !id.startsWith(this.platform + '-') || !new RegExp(`^${id}/(?:[a-f0-9]{32}|[a-f0-9]{64})$`, 'u').test(spelling)) throw new Error('An installed component has an invalid execution target or generation.')
      const path = resolve(root, spelling)
      if (!inside(root, path) || path === root) throw new Error('An installed component escapes its owned directory.')
      entries.push({ id, path })
      if (id === `${this.platform}-basic`) {
        defaults.python = join(path, 'python', ...(this.platform === 'windows' ? ['python.exe'] : ['bin', 'python3']))
        defaults.node = join(path, 'node', ...(this.platform === 'windows' ? ['node.exe'] : ['bin', 'node']))
        if (this.platform === 'windows') defaults.php = join(path, 'php', 'php.exe')
      }
      if (id === 'windows-cpp') { defaults.c = join(path, 'cpp/bin/clang.exe'); defaults.cpp = join(path, 'cpp/bin/clang++.exe') }
    }
    this.componentRoots = entries
    this.bundledExecutables = defaults
  }

  private workspace(workspaceId: WorkspaceId): { path: string } {
    const workspace = this.options.resolveWorkspace(workspaceId)
    if (!workspace) throw new Error('The selected workspace does not exist in this execution target.')
    return workspace
  }

  private savedWorkspace(workspaceId: WorkspaceId): SelectionState['workspaces'][string] | undefined {
    return this.state.workspaces[workspaceId]
  }

  private async persist(): Promise<void> {
    await mkdir(this.options.root, { recursive: true, mode: 0o700 })
    const temporary = join(this.options.root, `runtime-environments.${randomUUID()}.pending`)
    const file = await open(temporary, 'wx', 0o600)
    try {
      try { await file.writeFile(JSON.stringify(this.state, null, 2) + '\n'); await file.sync() }
      finally { await file.close() }
      await rename(temporary, join(this.options.root, 'runtime-environments.json'))
    } catch (error) {
      await rm(temporary, { force: true }).catch((_cleanupError: unknown) => { /* Preserve the original write failure. */ })
      throw error
    }
  }

  private async candidates(workspaceId: WorkspaceId): Promise<{ language: RuntimeLanguage; path: string; source: RuntimeCandidate['source'] }[]> {
    const root = this.workspace(workspaceId).path
    const found = new Map<string, { language: RuntimeLanguage; path: string; source: RuntimeCandidate['source'] }>()
    const add = async (language: RuntimeLanguage, path: string, source: RuntimeCandidate['source']): Promise<void> => {
      if (found.size >= this.options.maxCandidates || !isAbsolute(path)) return
      try { if (!(await stat(path)).isFile()) return; await access(path, this.platform === 'windows' ? constants.F_OK : constants.X_OK) }
      catch (_missingCandidate) { return }
      const key = `${language}:${this.platform === 'windows' ? path.toLowerCase() : path}`
      if (!found.has(key)) found.set(key, { language, path, source })
    }
    for (const [language, path] of Object.entries(this.savedWorkspace(workspaceId)?.selected ?? {})) {
      await add(languageSchema.parse(language), path, 'manual')
    }
    for (const [language, path] of Object.entries(this.options.builtinExecutables ?? {})) await add(languageSchema.parse(language), path, 'bundled')
    for (const name of ['.venv', 'venv']) await add('python', join(root, name, this.platform === 'windows' ? 'Scripts' : 'bin', this.platform === 'windows' ? 'python.exe' : 'python'), 'project')
    for (const component of this.componentRoots) {
      for (const language of languages) {
        const directory = language === 'c' || language === 'cpp' ? 'cpp' : language
        for (const path of [join(component.path, directory, 'bin', commandName(language, this.platform)), join(component.path, directory, commandName(language, this.platform))]) await add(language, path, 'bundled')
        if (this.platform === 'windows' && (language === 'c' || language === 'cpp')) await add(language, join(component.path, directory, 'bin', language === 'c' ? 'clang.exe' : 'clang++.exe'), 'bundled')
      }
    }
    let conda = ''
    try { conda = await readFile(join(homedir(), '.conda', 'environments.txt'), 'utf8') }
    catch (error) { if (!(error instanceof Error && 'code' in error && error.code === 'ENOENT')) throw error }
    for (const prefix of conda.split(/\r?\n/u).filter(Boolean).slice(0, this.options.maxCandidates)) {
      await add('python', join(prefix, ...(this.platform === 'windows' ? ['python.exe'] : ['bin', 'python'])), 'conda')
    }
    for (const prefix of ['miniforge3', 'miniconda3', 'anaconda3']) await add('python', join(homedir(), prefix, ...(this.platform === 'windows' ? ['python.exe'] : ['bin', 'python'])), 'conda')
    if (this.platform === 'windows') {
      const pythonRoot = join(process.env.LOCALAPPDATA ?? join(homedir(), 'AppData', 'Local'), 'Programs', 'Python')
      try { for (const entry of (await readdir(pythonRoot, { withFileTypes: true })).slice(0, this.options.maxCandidates)) if (entry.isDirectory()) await add('python', join(pythonRoot, entry.name, 'python.exe'), 'system') }
      catch (error) { if (!(error instanceof Error && 'code' in error && error.code === 'ENOENT')) throw error }
    }
    for (const directory of (process.env.PATH ?? '').split(delimiter).filter(Boolean)) {
      for (const language of languages) await add(language, join(directory.replace(/^"|"$/g, ''), commandName(language, this.platform)), 'system')
      if (this.platform === 'linux') await add('python', join(directory, 'python'), 'system')
    }
    return [...found.values()]
  }

  /** @param workspaceId - current Host workspace. @returns freshly verified candidate runtimes without installing anything. */
  async discover(workspaceId: WorkspaceId): Promise<RuntimeSnapshot> {
    await this.refreshComponents()
    const candidates = await this.candidates(workspaceId)
    const results = new Array<RuntimeCandidate>(candidates.length)
    let next = 0
    // Results keep candidate order; probes never throw, they record failures on the candidate.
    await Promise.all(Array.from({ length: Math.min(this.options.probeConcurrency ?? 1, candidates.length) }, async () => {
      for (let index = next++; index < candidates.length; index = next++) {
        results[index] = await probeRuntimeCandidate({ ...candidates[index], targetId: this.target, platform: this.platform,
          timeoutMs: this.options.probeTimeoutMs, run: this.run })
      }
    }))
    this.observations.set(workspaceId, results)
    return this.status(workspaceId)
  }

  /**
   * @param workspaceId - current Host workspace.
   * @param language - runtime family.
   * @param path - explicit existing executable.
   * @returns the probe merged into the latest observations.
   */
  async probe(workspaceId: WorkspaceId, language: RuntimeLanguage, path: string): Promise<RuntimeSnapshot> {
    this.workspace(workspaceId)
    const candidate = await probeRuntimeCandidate({ targetId: this.target, platform: this.platform, language, path, source: 'manual', timeoutMs: this.options.probeTimeoutMs, run: this.run })
    this.observations.set(workspaceId, [
      ...(this.observations.get(workspaceId) ?? []).filter(value => value.id !== candidate.id), candidate,
    ])
    return this.status(workspaceId)
  }

  /** @param workspaceId - current Host workspace. @returns saved choices and cached functional observations. */
  status(workspaceId: WorkspaceId): Promise<RuntimeSnapshot> {
    return Promise.resolve().then(() => this.snapshot(workspaceId))
  }

  private snapshot(workspaceId: WorkspaceId): RuntimeSnapshot {
    this.workspace(workspaceId)
    const candidates = this.observations.get(workspaceId) ?? []
    const selected: RuntimeSnapshot['selected'] = {}
    for (const [language, path] of Object.entries(this.savedWorkspace(workspaceId)?.selected ?? {})) {
      const family = languageSchema.parse(language)
      selected[family] = candidates.find(candidate => candidate.language === family && candidate.path === path)
        ?? { id: identity(this.target, family, path), language: family, path, source: 'manual', platform: this.platform,
          version: null, ready: false, capabilities: [], error: 'Recheck this environment to confirm its current capabilities.' }
    }
    return { targetId: this.target, platform: this.platform, workspaceId, selected, candidates }
  }

  /**
   * @param workspaceId - current Host workspace.
   * @param language - family whose explicit selection changes.
   * @param path - existing executable, or null to clear.
   * @returns state after functional validation and atomic persistence.
   */
  select(workspaceId: WorkspaceId, language: RuntimeLanguage, path: string | null): Promise<RuntimeSnapshot> {
    const action = async (): Promise<RuntimeSnapshot> => {
      const root = this.workspace(workspaceId).path
      let candidate: RuntimeCandidate | undefined
      if (path !== null) {
        candidate = await probeRuntimeCandidate({ targetId: this.target, platform: this.platform, language, path, source: 'manual', timeoutMs: this.options.probeTimeoutMs, run: this.run })
        if (!candidate.ready) throw new Error(candidate.error ?? 'This interpreter is not usable in the current execution target.')
      }
      const before = structuredClone(this.state)
      let selected = { ...(this.savedWorkspace(workspaceId)?.selected ?? {}) }
      if (candidate) selected[language] = candidate.path
      else {
        const { [language]: _removed, ...remaining } = selected
        selected = remaining
      }
      const previousPrefix = this.savedWorkspace(workspaceId)?.pythonPrefix
      const pythonPrefix = language === 'python' ? candidate?.prefix : previousPrefix
      this.state.workspaces[workspaceId] = { root, selected, ...(pythonPrefix ? { pythonPrefix } : {}) }
      try { await this.persist() } catch (error) { this.state = before; throw error }
      if (candidate) this.observations.set(workspaceId, [
        ...(this.observations.get(workspaceId) ?? []).filter(value => value.id !== candidate.id), candidate,
      ])
      return this.status(workspaceId)
    }
    const result = this.pending.then(action, action)
    this.pending = result.then(() => undefined, () => undefined)
    return result
  }

  /**
   * @param cwd - process directory in the current execution world.
   * @returns an explicit per-process overlay, without changing process.env.
   */
  resolve(cwd: string): ResolvedWorkspaceEnvironment {
    if (!this.loaded) throw new Error('Runtime selections have not finished loading.')
    const match = Object.values(this.state.workspaces).filter(entry => inside(entry.root, cwd))
      .sort((left, right) => right.root.length - left.root.length).at(0)
    // A built-in interpreter counts only once its optional component has been downloaded.
    const builtin = Object.fromEntries(Object.entries(this.options.builtinExecutables ?? {}).filter(([, path]) => existsSync(path)))
    const executables = { ...builtin, ...this.bundledExecutables, ...(match?.selected ?? {}) }
    const directories = [...new Set(Object.values(executables).map(path => dirname(path)))]
    if (match?.pythonPrefix && this.platform === 'windows') {
      directories.push(join(match.pythonPrefix, 'Scripts'), join(match.pythonPrefix, 'Library', 'bin'))
    }
    for (const component of this.componentRoots.filter(value => value.id.endsWith('-basic') || value.id.endsWith('-cpp') || value.id.endsWith('-development'))) {
      for (const directory of ['python', 'node', 'php', 'cpp', 'cmake', 'ninja', 'llvm', 'git/cmd', 'codelldb/extension/adapter']) {
        directories.push(join(component.path, directory, 'bin'), join(component.path, directory))
        if (this.platform === 'windows' && directory === 'python') directories.push(join(component.path, directory, 'Scripts'))
      }
    }
    const environment: Record<string, string> = { PATH: [...directories, process.env.PATH ?? ''].join(delimiter), PYTHONDONTWRITEBYTECODE: '1' }
    if (this.componentRoots.some(component => component.id === 'windows-cpp')) environment.CMAKE_GENERATOR = 'Ninja'
    if (executables.php && this.platform === 'windows') {
      environment.RAINY_PHP_EXTENSION_DIR = join(dirname(executables.php), 'ext')
    }
    return { environment, executables }
  }
}
