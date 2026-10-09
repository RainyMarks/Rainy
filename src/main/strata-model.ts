/** Resolve external Qwen3.8 Flash Next files without changing weights or importing executable configuration. */
import { createHash } from 'node:crypto'
import type { Dirent } from 'node:fs'
import { open, readFile, readdir, stat } from 'node:fs/promises'
import { basename, dirname, extname, isAbsolute, join, resolve } from 'node:path'
import { z } from 'zod'
import { strataSettingsSchema } from '../shared/strata-protocol.ts'
import type { StrataModel, StrataSettings } from '../shared/strata-protocol.ts'

/** Explicit initial allocation; changing model-request budgets remains the model settings page's responsibility. */
export const DEFAULT_STRATA_SETTINGS: StrataSettings = {
  modelPath: '', mtpPath: '', contextWindow: 32768, port: 8081, kvCache: 'int8',
  vramReserveMiB: 700, residentBudgetGiB: null,
}

const profileSchema = z.object({
  args: z.array(z.string()).max(256), cwd: z.string().optional(), tokenizer: z.string().optional(), model_name: z.string().optional(),
  port: z.number().int().min(1024).max(65535).optional(),
})

/** One offline converter writes only inside the caller's private model cache. */
export interface StrataPreparation {
  readonly tool: 'iq_pack.py' | 'mtp_rt.py'
  readonly args: readonly string[]
  readonly directory: string
  readonly stage: 'model' | 'mtp'
}

/** A resolved model keeps executable and network configuration out of imported profiles. */
export interface ResolvedStrataModel {
  readonly model: StrataModel
  readonly settings: StrataSettings
  readonly preparations: readonly StrataPreparation[]
  readonly missing: readonly string[]
}

async function isFile(path: string): Promise<boolean> {
  try { const info = await stat(path); return info.isFile() && info.size > 0 }
  catch (error) { if (error instanceof Error && 'code' in error && (error.code === 'ENOENT' || error.code === 'ENOTDIR')) return false; throw error }
}

async function hasFiles(root: string, names: readonly string[]): Promise<boolean> {
  return (await Promise.all(names.map(name => isFile(join(root, name))))).every(Boolean)
}

async function readJson(path: string): Promise<unknown> {
  if ((await stat(path)).size > 65536) throw new Error('Strata 配置文件超过 64 KiB。')
  return JSON.parse((await readFile(path, 'utf8')).replace(/^\uFEFF/u, ''))
}

function absoluteFrom(base: string, value: string): string { return isAbsolute(value) ? resolve(value) : resolve(base, value) }

function argument(args: readonly string[], flag: string): string | undefined {
  const index = args.indexOf(flag)
  return index < 0 ? undefined : args[index + 1]
}

async function firstGguf(path: string): Promise<{ path: string; shards: string[] }> {
  let file = path
  const split = /^(.*)-(\d{5})-of-(\d{5})\.gguf$/iu.exec(basename(path))
  const shards: string[] = []
  if (split) {
    const total = Number(split[3])
    if (total < 1 || total > 128) throw new Error('GGUF 分片数量无效。')
    for (let index = 1; index <= total; index++) shards.push(join(dirname(path), `${split[1]}-${String(index).padStart(5, '0')}-of-${split[3]}.gguf`))
    file = join(dirname(path), `${split[1]}-00001-of-${split[3]}.gguf`)
  } else shards.push(file)
  for (const shard of shards) if (!await isFile(shard)) throw new Error(`缺少模型分片：${basename(shard)}`)
  const handle = await open(file, 'r')
  try {
    const magic = Buffer.alloc(4)
    if ((await handle.read(magic, 0, 4, 0)).bytesRead !== 4 || magic.toString('ascii') !== 'GGUF') throw new Error('所选文件不是 GGUF 模型。')
  } finally { await handle.close() }
  return { path: file, shards }
}

async function filesIn(root: string): Promise<string[]> {
  try { return (await readdir(root, { withFileTypes: true })).filter(entry => entry.isFile()).map(entry => join(root, entry.name)) }
  catch (error) { if (error instanceof Error && 'code' in error && error.code === 'ENOENT') return []; throw error }
}

async function modelInDirectory(root: string): Promise<string> {
  const direct = await filesIn(root)
  const profiles = direct.filter(path => /^strata-.+\.json$/iu.test(basename(path)) && !/\.shared-settings\.json$/iu.test(path))
  if (profiles.length === 1) return profiles[0]
  const candidates = direct.filter(path => /\.gguf$/iu.test(path) && !/mtp/iu.test(basename(path)))
  if (!candidates.length) {
    const models = join(root, 'models')
    let entries: Dirent[]
    try { entries = await readdir(models, { withFileTypes: true }) }
    catch (error) { if (!(error instanceof Error && 'code' in error && error.code === 'ENOENT')) throw error; entries = [] }
    if (entries.length > 128) throw new Error('模型目录包含过多项目，请直接选择主模型的 GGUF 文件。')
    for (const entry of entries) {
      if (entry.isDirectory()) candidates.push(...(await filesIn(join(models, entry.name))).filter(path => /\.gguf$/iu.test(path)))
      else if (entry.isFile() && /\.gguf$/iu.test(entry.name)) candidates.push(join(models, entry.name))
    }
  }
  const first = candidates.filter(path => !/mtp/iu.test(basename(path))
    && (!/-\d{5}-of-\d{5}\.gguf$/iu.test(path) || /-00001-of-\d{5}\.gguf$/iu.test(path)))
  if (first.length !== 1) throw new Error(first.length ? '目录包含多个模型，请直接选择需要的首个 GGUF 分片。' : '目录中没有找到主模型 GGUF，请选择完整模型目录或首个分片。')
  return first[0]
}

async function fingerprint(paths: readonly string[]): Promise<string> {
  const facts = await Promise.all(paths.map(async (path) => {
    const info = await stat(path)
    return { path: resolve(path), size: info.size, mtimeMs: info.mtimeMs }
  }))
  return createHash('sha256').update(JSON.stringify(facts)).digest('hex').slice(0, 24)
}

async function packReady(pack: string, tokenizer = join(pack, 'tokenizer')): Promise<boolean> {
  return await hasFiles(pack, ['index.txt', 'dense.bin'])
    && (await isFile(join(pack, 'native_experts.txt')) || await isFile(join(pack, 'experts.bin')))
    && await hasFiles(tokenizer, ['vocab.json', 'merges.txt', 'token_type.json'])
}

const mtpFiles = ['dense.bin', 'dense.txt', 'experts.bin']

async function resolveMtp(source: string, candidates: readonly string[], cacheRoot: string): Promise<{
  path: string | null
  preparations: StrataPreparation[]
}> {
  let selected = source ? resolve(source) : ''
  if (!selected) {
    for (const candidate of candidates) {
      if (await hasFiles(candidate, mtpFiles) || await isFile(candidate) && /\.gguf$/iu.test(candidate)) { selected = candidate; break }
    }
  }
  if (!selected) return { path: null, preparations: [] }
  if (await hasFiles(selected, mtpFiles)) return { path: selected, preparations: [] }
  if ((await stat(selected)).isDirectory()) {
    if (await hasFiles(join(selected, 'rt'), mtpFiles)) return { path: join(selected, 'rt'), preparations: [] }
    const packed = join(selected, 'mtp-q2_0.gguf')
    if (await isFile(packed)) selected = packed
    else throw new Error('MTP 目录需要 dense.bin、dense.txt、experts.bin，或配套的 mtp-q2_0.gguf。')
  }
  const gguf = await firstGguf(selected)
  const output = join(cacheRoot, 'mtp', await fingerprint(gguf.shards))
  if (await hasFiles(output, [...mtpFiles, '.rainy-complete.json'])) return { path: output, preparations: [] }
  return { path: output, preparations: [{ tool: 'mtp_rt.py', args: ['--gguf', gguf.path, '--out', output], directory: output, stage: 'mtp' }] }
}

/**
 * Import only weights and supported allocation fields, with no launch, hook, or credential copying.
 * @param input - settings selected by the user.
 * @param cacheRoot - Rainy-owned directory for derived native packs and MTP runtime files.
 * @param importDefaults - whether a newly selected profile supplies its saved allocation defaults.
 * @returns inspected files and the exact offline preparation steps still needed.
 */
export async function resolveStrataModel(
  input: StrataSettings, cacheRoot: string, importDefaults = false,
): Promise<ResolvedStrataModel | null> {
  if (!input.modelPath.trim()) return null
  const sourcePath = resolve(input.modelPath)
  const selected = (await stat(sourcePath)).isDirectory() ? await modelInDirectory(sourcePath) : sourcePath
  let native = selected
  let explicitPack: string | undefined
  let explicitTokenizer: string | undefined
  let explicitMtp: string | undefined
  let modelName: string | undefined
  let settings = { ...input, modelPath: sourcePath }
  if (extname(selected).toLowerCase() === '.json') {
    const profile = profileSchema.parse(await readJson(selected))
    const base = profile.cwd ? absoluteFrom(dirname(selected), profile.cwd) : dirname(selected)
    const path = argument(profile.args, '--native')
    if (!path) throw new Error('此 Strata 配置没有外部主模型 GGUF（--native）。')
    native = absoluteFrom(base, path)
    const pack = argument(profile.args, '--pack')
    if (pack) explicitPack = absoluteFrom(base, pack)
    if (profile.tokenizer) explicitTokenizer = absoluteFrom(base, profile.tokenizer)
    const mtp = argument(profile.args, '--mtp')
    if (mtp) explicitMtp = absoluteFrom(base, mtp)
    modelName = profile.model_name
    if (importDefaults) {
      const context = argument(profile.args, '--max-context')
      const kv = argument(profile.args, '--kv')
      const reserve = argument(profile.args, '--vram-reserve-mib')
      const budget = argument(profile.args, '--resident-budget-gib')
      settings = strataSettingsSchema.parse({ ...settings,
        ...(context ? { contextWindow: Number(context) } : {}), ...(profile.port ? { port: profile.port } : {}),
        ...(kv ? { kvCache: kv } : {}), ...(reserve ? { vramReserveMiB: Number(reserve) } : {}),
        ...(budget ? { residentBudgetGiB: Number(budget) } : {}),
        ...(explicitMtp ? { mtpPath: explicitMtp } : {}),
      })
    }
  } else if (extname(selected).toLowerCase() !== '.gguf') throw new Error('请选择 Qwen3.8 Flash Next GGUF、模型目录或已有 Strata JSON 配置。')
  const gguf = await firstGguf(native)
  const modelDirectory = dirname(gguf.path)
  const preparations: StrataPreparation[] = []
  let packPath: string | null = null
  let tokenizerPath: string | null = null
  const packCandidates = [...new Set([...(explicitPack ? [explicitPack] : []), join(modelDirectory, 'pack'),
    resolve(modelDirectory, '../../packs', basename(modelDirectory))])]
  for (const candidate of packCandidates) {
    const tokenizer = candidate === explicitPack && explicitTokenizer ? explicitTokenizer : join(candidate, 'tokenizer')
    if (await packReady(candidate, tokenizer)) { packPath = candidate; tokenizerPath = tokenizer; break }
  }
  if (!packPath) {
    packPath = join(cacheRoot, 'packs', await fingerprint(gguf.shards))
    tokenizerPath = join(packPath, 'tokenizer')
    if (!await packReady(packPath) || !await isFile(join(packPath, '.rainy-complete.json'))) {
      preparations.push({ tool: 'iq_pack.py', args: ['--gguf', gguf.path, '--out', packPath, '--compat-bf16'],
        directory: packPath, stage: 'model' })
    }
  }
  const mtpCandidates = [explicitMtp, ...[modelDirectory, dirname(modelDirectory), dirname(dirname(modelDirectory))]
    .flatMap(base => [join(base, 'mtp/rt'), join(base, 'mtp'), join(base, 'mtp/mtp-q2_0.gguf'), join(base, 'mtp-q2_0.gguf')])]
    .filter((value): value is string => value !== undefined)
  const mtp = await resolveMtp(settings.mtpPath, mtpCandidates, cacheRoot)
  preparations.push(...mtp.preparations)
  const model = modelName?.trim() || basename(gguf.path, '.gguf').replace(/-\d{5}-of-\d{5}$/u, '').toLowerCase()
  return { settings, preparations, missing: mtp.path ? [] : ['缺少配套 MTP 权重。请选择 MTP GGUF 或包含 dense.bin、dense.txt、experts.bin 的目录。'],
    model: { sourcePath, model, ggufPath: gguf.path, packPath, tokenizerPath, mtpPath: mtp.path,
      needsPreparation: preparations.length > 0 } }
}

/**
 * Discover explicit installer records without running setup or scanning arbitrary user directories.
 * @param settingsPath - Strata's per-user settings file, when installed.
 * @returns profile paths that the user may choose to import.
 */
export async function discoverStrataProfiles(settingsPath?: string): Promise<{ path: string; label: string }[]> {
  if (!settingsPath) return []
  let value: unknown
  try { value = await readJson(settingsPath) }
  catch (error) { if (error instanceof Error && 'code' in error && error.code === 'ENOENT') return []; throw error }
  const { installs } = z.object({ installs: z.array(z.string()).max(20).default([]) }).parse(value)
  const profiles: { path: string; label: string }[] = []
  for (const root of installs) {
    if (!isAbsolute(root)) continue
    for (const path of await filesIn(root)) {
      if (/^strata-.+\.json$/iu.test(basename(path)) && !/\.shared-settings\.json$/iu.test(path)) {
        profiles.push({ path, label: basename(path, '.json').replace(/^strata-/u, '') })
      }
    }
  }
  return profiles.sort((a, b) => a.path.localeCompare(b.path))
}
