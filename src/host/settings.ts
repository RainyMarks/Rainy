/** `settings.json` and `.credentials.json` under the Host home, plus the one-time import of RainyAgent 1.x settings. */
import { readFile } from 'node:fs/promises'
import { join } from 'node:path'
import yaml from 'js-yaml'
import { z } from 'zod'
import type { McpServerConfig, ModelSelection, ModelSetup, UiPreferences } from '../shared/rpc.ts'
import { readJson, SerialQueue, writeJson } from './files.ts'

/** Maximum characters of the user's global prompt. */
export const GLOBAL_PROMPT_MAX_CHARS = 4000

const thinking = z.enum(['off', 'low', 'high', 'max'])
const modelSchema = z.object({
  provider: z.string().regex(/^[a-z][a-z0-9-]{0,47}$/),
  baseURL: z.string().url(),
  model: z.string().min(1),
  contextWindow: z.number().int().min(4096),
  maxTokens: z.number().int().positive().optional(),
  local: z.boolean(),
  api: z.enum(['openai-completions', 'openai-responses', 'anthropic-messages']).optional(),
  thinking: thinking.optional(),
  thinkingFormat: z.enum(['openai', 'deepseek', 'qwen']).optional(),
  maxTokensField: z.enum(['max_tokens', 'max_completion_tokens']).optional(),
})
const serverSchema = z.object({
  name: z.string().regex(/^[A-Za-z0-9_-]{1,32}$/),
  enabled: z.boolean(),
  transport: z.enum(['stdio', 'streamable-http']),
  command: z.string().min(1).optional(),
  args: z.array(z.string()).optional(),
  env: z.record(z.string(), z.string()).optional(),
  url: z.string().url().optional(),
  tools: z.array(z.string()),
}).refine(server => server.transport === 'stdio' ? server.command !== undefined : server.url !== undefined, 'stdio servers need a command; HTTP servers need a URL')

/** Interface defaults for a new installation. */
export const DEFAULT_PREFS: Readonly<UiPreferences> = {
  locale: 'zh', theme: 'system', uiFontSize: 14, codeFontSize: 13, busyEnter: 'queue', stepDetail: 'standard', showUsage: true,
}
const prefsSchema = z.object({
  locale: z.enum(['zh', 'en']),
  theme: z.enum(['system', 'light', 'dark']),
  uiFontSize: z.number().int().min(12).max(17),
  codeFontSize: z.number().int().min(12).max(17),
  busyEnter: z.enum(['queue', 'steer']),
  stepDetail: z.enum(['compact', 'standard', 'detailed']),
  showUsage: z.boolean(),
})

const fileSchema = z.object({
  version: z.literal(1),
  models: z.array(modelSchema),
  selected: z.object({ provider: z.string(), model: z.string(), thinking: thinking.optional() }).nullable(),
  globalPrompt: z.string().max(GLOBAL_PROMPT_MAX_CHARS),
  mcpServers: z.array(serverSchema),
  ui: prefsSchema.partial(),
  workbench: z.record(z.string(), z.unknown()).optional(),
})

/** Persisted settings. */
export interface SettingsData {
  version: 1
  models: ModelSetup[]
  selected: ModelSelection | null
  globalPrompt: string
  mcpServers: McpServerConfig[]
  ui: Partial<UiPreferences>
  workbench?: Record<string, unknown> | undefined
}

const credentialsSchema = z.object({ version: z.literal(1), keys: z.record(z.string(), z.string()) })

/**
 * Credential name for a provider. The DeepSeek preset uses `DEEPSEEK_API_KEY`, both Claude presets share
 * `ANTHROPIC_API_KEY`, and every other provider has its own name. An environment variable with the same
 * name overrides the stored value.
 * @param provider Provider id.
 * @returns The credential name.
 */
export function credentialName(provider: string): string {
  if (provider === 'rainy-deepseek') return 'DEEPSEEK_API_KEY'
  if (provider === 'rainy-claude' || provider === 'rainy-claude-haiku') return 'ANTHROPIC_API_KEY'
  return `RAINY_${provider.replaceAll('-', '_').toUpperCase()}_KEY`
}

/** Placeholder stored for local models that accept any key. */
export const LOCAL_NO_KEY = 'rainy-local-no-key'

/** Settings and credentials of one Host. */
export class Settings {
  private data: SettingsData = { version: 1, models: [], selected: null, globalPrompt: '', mcpServers: [], ui: {} }
  private keys: Record<string, string> = {}
  private readonly queue = new SerialQueue()
  private readonly settingsFile: string
  private readonly credentialsFile: string

  /**
   * @param home Host home directory.
   * @param env Process environment; variables named like a credential override stored keys.
   */
  constructor(private readonly home: string, private readonly env: NodeJS.ProcessEnv) {
    this.settingsFile = join(home, 'settings.json')
    this.credentialsFile = join(home, '.credentials.json')
  }

  /** Load both files. When `settings.json` does not exist yet, import RainyAgent 1.x settings first. */
  async load(): Promise<void> {
    const raw = await readJson(this.settingsFile)
    if (raw === undefined) {
      await this.importLegacy()
      await writeJson(this.settingsFile, this.data)
      await writeJson(this.credentialsFile, { version: 1, keys: this.keys })
      return
    }
    this.data = fileSchema.parse(raw) as SettingsData
    const credentials = await readJson(this.credentialsFile)
    this.keys = credentials === undefined ? {} : credentialsSchema.parse(credentials).keys
  }

  /** @returns A copy of the persisted settings. */
  get(): SettingsData {
    return structuredClone(this.data)
  }

  /** @returns Interface preferences with defaults filled in. */
  prefs(): UiPreferences {
    return { ...DEFAULT_PREFS, ...this.data.ui }
  }

  /**
   * Apply a change and persist it.
   * @param change Mutates the settings copy; may be async.
   * @returns The committed settings.
   */
  update(change: (data: SettingsData) => void | Promise<void>): Promise<SettingsData> {
    return this.queue.run(async () => {
      const next = structuredClone(this.data)
      await change(next)
      const parsed = fileSchema.parse(next) as SettingsData
      await writeJson(this.settingsFile, parsed)
      this.data = parsed
      return this.get()
    })
  }

  /**
   * Update interface preferences.
   * @param change Fields to change.
   * @returns The resulting preferences.
   */
  async setPrefs(change: Partial<UiPreferences>): Promise<UiPreferences> {
    prefsSchema.partial().parse(change)
    await this.update((data) => { data.ui = { ...data.ui, ...change } })
    return this.prefs()
  }

  /**
   * Read a provider's API key. An environment variable with the credential's name takes precedence.
   * @param provider Provider id.
   * @returns The key, or `undefined` when none is stored.
   */
  apiKey(provider: string): string | undefined {
    const name = credentialName(provider)
    return this.env[name] || this.keys[name]
  }

  /** @returns Providers whose key is available. */
  providersWithKeys(): string[] {
    return this.data.models.map(model => model.provider).filter(provider => this.apiKey(provider) !== undefined)
  }

  /**
   * Store or remove a provider's API key.
   * @param provider Provider id.
   * @param key Key text; `undefined` removes the stored key.
   */
  setApiKey(provider: string, key: string | undefined): Promise<void> {
    return this.queue.run(async () => {
      const name = credentialName(provider)
      const next = { ...this.keys }
      if (key === undefined) delete next[name]
      else next[name] = key
      await writeJson(this.credentialsFile, { version: 1, keys: next })
      this.keys = next
    })
  }

  private async importLegacy(): Promise<void> {
    const credentials = await readText(join(this.home, '.credentials.yaml'))
    if (credentials !== undefined) {
      const document: unknown = yaml.load(credentials)
      if (isRecord(document)) {
        for (const [name, value] of Object.entries(document)) {
          if (typeof value === 'string' && value !== '') this.keys[name] = value
          else if (isRecord(value) && value.kind === 'api-key' && typeof value.key === 'string' && value.key !== '') this.keys[name] = value.key
        }
      }
    }
    const patch = await readText(join(this.home, 'profiles', 'rainy', 'cordis.patch.yml'))
    if (patch === undefined) return
    const entries: Record<string, unknown>[] = []
    collectEntries(yaml.load(patch), entries)
    const piAi = entries.find(entry => entry.id === 'llm-pi-ai' || entry.name === '@deepseek-ai/dsh-llm-pi-ai')
    const providers = isRecord(piAi?.config) && isRecord(piAi.config.providers) ? piAi.config.providers : {}
    for (const [provider, value] of Object.entries(providers)) {
      if (!isRecord(value) || !Array.isArray(value.models)) continue
      const compat = isRecord(value.compat) ? value.compat : {}
      for (const model of value.models) {
        if (!isRecord(model)) continue
        const parsed = modelSchema.safeParse({
          provider, baseURL: value.baseURL, model: model.id,
          contextWindow: model.contextWindow ?? value.defaultContextWindow,
          maxTokens: model.maxTokens ?? value.defaultMaxTokens,
          local: value.displayName === '本地模型', api: value.api,
          thinking: value.reasoning, thinkingFormat: compat.thinkingFormat, maxTokensField: compat.maxTokensField,
        })
        if (parsed.success) this.data.models.push(parsed.data)
      }
    }
    const selection = entries.find(entry => entry.id === 'agent-default-model')
    if (isRecord(selection?.config) && typeof selection.config.provider === 'string' && typeof selection.config.model === 'string') {
      const effort = selection.config.reasoningEffort
      const known = this.data.models.some(model => model.provider === (selection.config as Record<string, unknown>).provider && model.model === (selection.config as Record<string, unknown>).model)
      if (known) {
        this.data.selected = {
          provider: selection.config.provider, model: selection.config.model,
          ...(typeof effort === 'string' && thinking.safeParse(effort).success ? { thinking: effort as ModelSelection['thinking'] } : {}),
        }
      }
    }
    const policy = entries.find(entry => entry.id === 'rainy-policy')
    if (isRecord(policy?.config) && typeof policy.config.globalPrompt === 'string') {
      this.data.globalPrompt = policy.config.globalPrompt.slice(0, GLOBAL_PROMPT_MAX_CHARS)
    }
  }
}

async function readText(path: string): Promise<string | undefined> {
  try { return await readFile(path, 'utf8') } catch (error) {
    if ((error as NodeJS.ErrnoException).code === 'ENOENT') return undefined
    throw error
  }
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === 'object' && !Array.isArray(value)
}

/** Collect every object that names a plugin entry (`id` or `name`) anywhere in a 1.x patch document. */
function collectEntries(node: unknown, out: Record<string, unknown>[]): void {
  if (Array.isArray(node)) { for (const item of node) collectEntries(item, out); return }
  if (!isRecord(node)) return
  if (typeof node.id === 'string' || typeof node.name === 'string') out.push(node)
  for (const value of Object.values(node)) collectEntries(value, out)
}
