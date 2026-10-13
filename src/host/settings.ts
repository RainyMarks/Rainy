/** `settings.json` and the API keys under the Host home (`credentials.ts`), plus the one-time import of RainyAgent 1.x settings. */
import { readFile } from 'node:fs/promises'
import { validateHeaderName, validateHeaderValue } from 'node:http'
import { join } from 'node:path'
import yaml from 'js-yaml'
import { z } from 'zod'
import { ACCENTS, COLOR_SCHEMES, FONT_SETTING_MAX, fontFamilies } from '../shared/appearance.ts'
import { localized } from '../shared/locale.ts'
import type { McpServerConfig, ModelSelection, ModelSetup, PermissionMode, PermissionRules, ProxySettings, UiPreferences } from '../shared/rpc.ts'
import { CredentialFiles, type CredentialSeal } from './credentials.ts'
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
  headers: z.record(z.string(), z.string()).optional().refine(validHeaders, 'MCP request headers need valid, distinct names and values'),
  tools: z.array(z.string()),
}).refine(server => server.transport === 'stdio' ? server.command !== undefined : server.url !== undefined, 'stdio servers need a command; HTTP servers need a URL')

/**
 * @param headers MCP request headers.
 * @returns Whether every header is valid HTTP and no two names differ only in case.
 */
function validHeaders(headers: Record<string, string> | undefined): boolean {
  if (headers === undefined) return true
  const names = new Set<string>()
  for (const [name, value] of Object.entries(headers)) {
    try {
      validateHeaderName(name)
      validateHeaderValue(name, value)
    } catch (_invalid) {
      return false
    }
    if (names.has(name.toLowerCase())) return false
    names.add(name.toLowerCase())
  }
  return true
}

/** Interface defaults for a new installation. */
export const DEFAULT_PREFS: Readonly<UiPreferences> = {
  locale: 'zh', theme: 'system', uiFontSize: 14, codeFontSize: 13, busyEnter: 'queue', stepDetail: 'standard', showUsage: true,
  colorScheme: 'graphite', accent: 'auto', uiFont: '', codeFont: '',
}
const fontSetting = z.string().max(FONT_SETTING_MAX).refine(value => fontFamilies(value) !== undefined, 'Invalid font family list')
const prefsSchema = z.object({
  locale: z.enum(['zh', 'en']),
  theme: z.enum(['system', 'light', 'dark']),
  uiFontSize: z.number().int().min(12).max(17),
  codeFontSize: z.number().int().min(12).max(17),
  busyEnter: z.enum(['queue', 'steer']),
  stepDetail: z.enum(['compact', 'standard', 'detailed']),
  showUsage: z.boolean(),
  colorScheme: z.enum(COLOR_SCHEMES),
  accent: z.enum(ACCENTS),
  uiFont: fontSetting,
  codeFont: fontSetting,
})
/** Stored preferences. A value this version does not accept, such as a color scheme a newer version added, is dropped
 *  instead of failing the whole file. */
const storedPrefsSchema = z.record(z.string(), z.unknown()).transform((stored) => {
  const kept: Record<string, unknown> = {}
  for (const [key, field] of Object.entries(prefsSchema.shape)) {
    const parsed = field.safeParse(stored[key])
    if (parsed.success) kept[key] = parsed.data
  }
  return kept as Partial<UiPreferences>
})

/** Remembered approvals per project; lists are capped so a runaway model cannot grow the file without bound. */
const MAX_RULES = 200
const rulesSchema = z.object({ commands: z.array(z.string().min(1).max(200)).max(MAX_RULES), tools: z.array(z.string().min(1).max(128)).max(MAX_RULES) })
const securitySchema = z.object({
  defaultMode: z.enum(['read-only', 'ask', 'auto']).optional(),
  rules: z.record(z.string(), rulesSchema).optional(),
})

/** Mode of new chats until the user picks another in Settings → Security. */
export const DEFAULT_PERMISSION_MODE: PermissionMode = 'ask'

const fileSchema = z.object({
  version: z.literal(1),
  models: z.array(modelSchema),
  selected: z.object({ provider: z.string(), model: z.string(), thinking: thinking.optional() }).nullable(),
  globalPrompt: z.string().max(GLOBAL_PROMPT_MAX_CHARS),
  mcpServers: z.array(serverSchema),
  ui: storedPrefsSchema,
  workbench: z.record(z.string(), z.unknown()).optional(),
  security: securitySchema.optional(),
  proxy: z.object({ mode: z.enum(['system', 'off', 'manual']), url: z.string().max(2048).optional() }).optional(),
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
  /** Default permission mode and remembered approvals by workspace id; absent in settings written before 2.1. */
  security?: { defaultMode?: PermissionMode | undefined; rules?: Record<string, PermissionRules> | undefined } | undefined
  /** Proxy for model requests; absent means the system proxy. */
  proxy?: ProxySettings | undefined
}

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

/**
 * @param name Credential name from {@link credentialName}.
 * @returns The service the key belongs to, as Settings names it.
 */
function credentialLabel(name: string): string {
  if (name === 'DEEPSEEK_API_KEY') return 'DeepSeek'
  if (name === 'ANTHROPIC_API_KEY') return 'Claude'
  const provider = /^RAINY_(.+)_KEY$/.exec(name)?.[1]
  return provider === undefined ? name : provider.toLowerCase().replaceAll('_', '-')
}

/** Placeholder stored for local models that accept any key. */
export const LOCAL_NO_KEY = 'rainy-local-no-key'

/** Settings and credentials of one Host. */
export class Settings {
  private data: SettingsData = { version: 1, models: [], selected: null, globalPrompt: '', mcpServers: [], ui: {} }
  private keys: Record<string, string> = {}
  private readonly queue = new SerialQueue()
  private readonly settingsFile: string
  private readonly credentials: CredentialFiles

  /**
   * @param home Host home directory.
   * @param env Process environment; variables named like a credential override stored keys.
   * @param seal Encrypts API keys at rest with the carrier's key; without it they are stored as plain text.
   */
  constructor(private readonly home: string, private readonly env: NodeJS.ProcessEnv, seal?: CredentialSeal) {
    this.settingsFile = join(home, 'settings.json')
    this.credentials = new CredentialFiles(home, seal)
  }

  /** @returns Whether API keys are encrypted at rest. */
  get keysSealed(): boolean {
    return this.credentials.sealed
  }

  /** Load both files. When `settings.json` does not exist yet, import RainyAgent 1.x settings first. */
  async load(): Promise<void> {
    const raw = await readJson(this.settingsFile)
    if (raw === undefined) {
      // Keys stored before settings.json went missing stay, unreadable ones included; a 1.x import only adds keys.
      this.keys = await this.credentials.read()
      await this.importLegacy()
      await writeJson(this.settingsFile, this.data)
      await this.credentials.write(this.keys)
      return
    }
    this.data = fileSchema.parse(raw) as SettingsData
    this.keys = await this.credentials.read()
  }

  /** @returns A copy of the persisted settings. */
  get(): SettingsData {
    return structuredClone(this.data)
  }

  /** @returns The mode new chats start in. */
  defaultPermission(): PermissionMode {
    return this.data.security?.defaultMode ?? DEFAULT_PERMISSION_MODE
  }

  /**
   * @param mode Mode new chats start in.
   * @returns The stored mode.
   */
  async setDefaultPermission(mode: PermissionMode): Promise<PermissionMode> {
    await this.update((data) => { data.security = { ...data.security, defaultMode: mode } })
    return this.defaultPermission()
  }

  /**
   * @param workspaceId Project id.
   * @returns Commands and MCP tools the user allowed without asking in that project.
   */
  permissionRules(workspaceId: string): PermissionRules {
    const rules = this.data.security?.rules?.[workspaceId]
    return { commands: [...rules?.commands ?? []], tools: [...rules?.tools ?? []] }
  }

  /**
   * Remember or forget an approval for a project.
   * @param workspaceId Project id.
   * @param kind Command prefix or MCP tool name.
   * @param rule The prefix or tool name.
   * @param present Add when true, remove when false.
   * @returns The project's rules afterwards.
   */
  async setPermissionRule(workspaceId: string, kind: 'command' | 'tool', rule: string, present: boolean): Promise<PermissionRules> {
    await this.update((data) => {
      const security = data.security ?? {}
      const all = { ...security.rules ?? {} }
      const current = all[workspaceId] ?? { commands: [], tools: [] }
      const field = kind === 'command' ? 'commands' : 'tools'
      const list = current[field].filter(item => item !== rule)
      if (present) list.push(rule)
      const next = { ...current, [field]: list.slice(-MAX_RULES) }
      if (next.commands.length === 0 && next.tools.length === 0) delete all[workspaceId]
      else all[workspaceId] = next
      data.security = { ...security, rules: all }
    })
    return this.permissionRules(workspaceId)
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

  /** @returns Credential names of stored keys this Host cannot open and no environment variable replaces. */
  unreadableKeys(): string[] {
    return this.credentials.unreadableNames.filter(name => !this.env[name] && this.keys[name] === undefined)
  }

  /**
   * Explain stored keys that could not be opened, for Settings → Models. Called when shown, so it follows the language.
   * @returns The message naming the affected services, or `undefined` when every stored key is readable.
   */
  unreadableKeysMessage(): string | undefined {
    const names = this.unreadableKeys()
    if (names.length === 0) return undefined
    const services = { zh: names.map(credentialLabel).join('、'), en: names.map(credentialLabel).join(', ') }
    return this.credentials.sealed
      ? localized({
        zh: `已保存的 ${services.zh} API 密钥无法用当前的 Windows 数据保护密钥解开（例如重置过 Windows 账户密码后），请重新填写。原来的加密内容仍保留在 .credentials.sealed.json 中。`,
        en: `The saved API keys for ${services.en} cannot be opened with the current Windows data protection key, for example after the Windows account password was reset. Enter them again; the encrypted values stay in .credentials.sealed.json.`,
      })
      : localized({
        zh: `Windows 数据保护当前不可用，无法读取加密保存的 ${services.zh} API 密钥。可以稍后重启 Rainy 再试，或重新填写密钥（将不加密保存）。`,
        en: `Windows data protection is unavailable, so the encrypted API keys for ${services.en} cannot be read. Restart Rainy later, or enter the keys again; they will then be stored unencrypted.`,
      })
  }

  /** @returns Providers whose real key is available; the local-model placeholder does not count. */
  providersWithKeys(): string[] {
    return this.data.models.map(model => model.provider).filter((provider) => {
      const key = this.apiKey(provider)
      return key !== undefined && key !== LOCAL_NO_KEY
    })
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
      await this.credentials.write(next, key === undefined ? [name] : [])
      this.keys = next
    })
  }

  private async importLegacy(): Promise<void> {
    const credentials = await readText(join(this.home, '.credentials.yaml'))
    /** 1.x credentials by reference name, such as `DEEPSEEK_API_KEY`. */
    const refs: Record<string, string> = {}
    if (credentials !== undefined) {
      const document: unknown = yaml.load(credentials)
      if (isRecord(document)) {
        // Released 1.x builds write version 1 with the keys under `refs`; pre-release builds kept them at the top level.
        const section = document.version === 1 && isRecord(document.refs) ? document.refs : document
        for (const [name, value] of Object.entries(section)) {
          if (typeof value === 'string' && value !== '') refs[name] = value
          else if (isRecord(value) && value.kind === 'api-key' && typeof value.key === 'string' && value.key !== '') refs[name] = value.key
        }
      }
    }
    this.keys = { ...refs, ...this.keys }
    const patch = await readText(join(this.home, 'profiles', 'rainy', 'cordis.patch.yml'))
    if (patch === undefined) return
    const entries: Record<string, unknown>[] = []
    collectEntries(yaml.load(patch), entries)
    const piAi = entries.find(entry => entry.id === 'llm-pi-ai' || entry.name === '@deepseek-ai/dsh-llm-pi-ai')
    const providers = isRecord(piAi?.config) && isRecord(piAi.config.providers) ? piAi.config.providers : {}
    for (const [provider, value] of Object.entries(providers)) {
      if (!isRecord(value) || !Array.isArray(value.models)) continue
      // A 1.x provider names its key reference in `apiKeyEnv`; 2.0 looks the key up by credentialName.
      const key = typeof value.apiKeyEnv === 'string' ? refs[value.apiKeyEnv] : undefined
      if (key !== undefined) this.keys[credentialName(provider)] ??= key
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
