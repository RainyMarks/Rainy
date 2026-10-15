/** Host process environment chosen by the Electron carrier. */
import { existsSync, readFileSync } from 'node:fs'
import { homedir, tmpdir } from 'node:os'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

/** Resolved paths and identities for one Host process. */
export interface HostEnvironment {
  /** Application version from the package manifest next to the bundle. */
  version: string
  /** Host data root (`RAINY_HOME`). */
  home: string
  /** Directory holding `dist/` and `resources/`. */
  appRoot: string
  /** `<appRoot>/resources`. */
  resources: string
  platform: 'win32' | 'linux'
  /** `windows-local`, `wsl:<guid>` or `ssh:<destination>`. */
  executionTargetId: string
  /** Name of the execution target for the status bar, such as `WSL · Ubuntu` or `SSH · dev`. */
  executionTargetLabel?: string | undefined
  /** Directory shared by the Windows and WSL Hosts for project identity and memory. */
  carrierStateRoot: string
  /** Installed offline components (`RAINY_TOOLCHAIN_ROOT`). */
  toolchainRoot: string
  /** Bundled PHP from the optional module, when installed. */
  builtinPhp?: string | undefined
  /** PowerShell chosen explicitly with `RAINY_PWSH_PATH`; otherwise `resolvePowerShell` picks one. */
  pwshPath?: string | undefined
  /** Save and select the DeepSeek preset at startup (`RAINY_CONFIGURE_DEEPSEEK=1`), for development with `DEEPSEEK_API_KEY`. */
  configureDeepSeek?: boolean | undefined
  /** Temporary files owned by this Host (tool output spill). */
  tmp: string
  /** Port to bind; 0 picks a free port. */
  port: number
}

function findAppRoot(start: string): string {
  let directory = start
  for (;;) {
    if (existsSync(join(directory, 'package.json')) && existsSync(join(directory, 'resources'))) return directory
    const parent = dirname(directory)
    if (parent === directory) throw new Error(`Rainy application root not found above ${start}`)
    directory = parent
  }
}

/**
 * Read the carrier-provided environment.
 * @param env Process environment.
 * @param moduleUrl `import.meta.url` of the entry module.
 * @returns The Host environment.
 */
export function readHostEnvironment(env: NodeJS.ProcessEnv, moduleUrl: string): HostEnvironment {
  const appRoot = findAppRoot(dirname(fileURLToPath(moduleUrl)))
  const manifest: unknown = JSON.parse(readFileSync(join(appRoot, 'package.json'), 'utf8'))
  if (manifest === null || typeof manifest !== 'object' || !('version' in manifest) || typeof manifest.version !== 'string') {
    throw new Error('Rainy package version is unavailable')
  }
  const home = resolve(env.RAINY_HOME ?? join(homedir(), '.rainy-agent'))
  const platform = process.platform === 'win32' ? 'win32' : 'linux'
  const port = Number(env.RAINY_PORT ?? 0)
  if (!Number.isInteger(port) || port < 0 || port > 65535) throw new Error('RAINY_PORT must be a TCP port number')
  return {
    version: manifest.version,
    home,
    appRoot,
    resources: join(appRoot, 'resources'),
    platform,
    executionTargetId: env.RAINY_EXECUTION_TARGET_ID ?? (platform === 'win32' ? 'windows-local' : 'wsl:legacy'),
    executionTargetLabel: env.RAINY_EXECUTION_TARGET_LABEL || undefined,
    carrierStateRoot: env.RAINY_CARRIER_STATE_ROOT ?? join(home, 'carrier-state'),
    toolchainRoot: env.RAINY_TOOLCHAIN_ROOT ?? join(home, 'components'),
    builtinPhp: env.RAINY_BUILTIN_PHP || undefined,
    pwshPath: env.RAINY_PWSH_PATH || undefined,
    configureDeepSeek: env.RAINY_CONFIGURE_DEEPSEEK === '1',
    tmp: join(tmpdir(), `rainy-agent-${process.pid}`),
    port,
  }
}

/**
 * @param env - Host environment.
 * @returns the kind of machine the Host runs on, as the status bar and the model see it.
 */
export function hostEnvironmentKind(env: Pick<HostEnvironment, 'platform' | 'executionTargetId'>): 'Windows' | 'WSL' | 'SSH' {
  return env.executionTargetId.startsWith('ssh:') ? 'SSH' : env.platform === 'win32' ? 'Windows' : 'WSL'
}
