/** Workbench timing, output retention and initial terminal dimensions. */

/** Host-resolved workbench limits, injected into the page as `window.__RAINY_WORKBENCH_CONFIG__`. */
export interface Config {
  /** Maximum wait for the workbench's application-ready acknowledgement. */
  readyTimeoutMs: number
  /** Maximum wait for a durable draft-save acknowledgement. */
  flushTimeoutMs: number
  /** Interval between observations of open editor files, idle execution, and visible model settings. */
  editorPollMs: number
  /** Quiet interval before recovery buffers are durably saved. */
  editorStateDebounceMs: number
  /** Poll interval while a user-owned run or debug operation is active. */
  executionPollMs: number
  /** Maximum retained output characters per workspace execution view. */
  editorMaxOutputCharacters: number
  /** Maximum workspace execution views retained in the renderer. */
  editorMaxRetainedWorkspaces: number
  /** Initial terminal columns before its panel reports actual geometry. */
  editorTerminalCols: number
  /** Initial terminal rows before its panel reports actual geometry. */
  editorTerminalRows: number
  /** Initial context window for a new local model configuration. */
  localModelContextWindow: number
  /** Initial context window for a new API model configuration. */
  apiModelContextWindow: number
}

/** Values used when `settings.json` does not override them. */
export const DEFAULT_CONFIG: Readonly<Config> = {
  readyTimeoutMs: 15000,
  flushTimeoutMs: 15000,
  editorPollMs: 2000,
  editorStateDebounceMs: 500,
  executionPollMs: 200,
  editorMaxOutputCharacters: 1048576,
  editorMaxRetainedWorkspaces: 8,
  editorTerminalCols: 80,
  editorTerminalRows: 24,
  localModelContextWindow: 100000,
  apiModelContextWindow: 1000000,
}

const LIMITS: Readonly<Record<keyof Config, readonly [number, number]>> = {
  readyTimeoutMs: [1000, 60000],
  flushTimeoutMs: [1000, 60000],
  editorPollMs: [500, 60000],
  editorStateDebounceMs: [100, 10000],
  executionPollMs: [50, 5000],
  editorMaxOutputCharacters: [1024, 16 * 1024 * 1024],
  editorMaxRetainedWorkspaces: [1, 64],
  editorTerminalCols: [2, 500],
  editorTerminalRows: [2, 200],
  localModelContextWindow: [4096, Number.MAX_SAFE_INTEGER],
  apiModelContextWindow: [4096, Number.MAX_SAFE_INTEGER],
}

/**
 * Merge user overrides into the defaults.
 * @param overrides Partial values from `settings.json`.
 * @returns A complete configuration.
 * @throws Error when an override is not an integer inside its allowed range.
 */
export function resolveConfig(overrides: Partial<Record<string, unknown>> = {}): Config {
  const config = { ...DEFAULT_CONFIG }
  for (const key of Object.keys(LIMITS) as (keyof Config)[]) {
    const value = overrides[key]
    if (value === undefined) continue
    const [min, max] = LIMITS[key]
    if (typeof value !== 'number' || !Number.isInteger(value) || value < min || value > max) {
      throw new Error(`settings.json workbench.${key} must be an integer between ${min} and ${max}`)
    }
    config[key] = value
  }
  return config
}
