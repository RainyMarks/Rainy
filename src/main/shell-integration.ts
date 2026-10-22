/** Explorer's "Open with Rainy" entries and the file types Rainy offers to open, kept in the current user's registry.
 *  Nothing here needs administrator rights, and Windows never lets an application make itself the default app: the
 *  user picks Rainy in "Open with" or in the Default apps settings. */
import { execFile } from 'node:child_process'
import { mkdtemp, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import type { Locale } from '../shared/locale.ts'
import { windowsPowerShellPath } from './powershell.ts'

/** Both parts. */
const BOTH: ShellIntegration = { contextMenu: true, fileTypes: true }

/** What the user turned on in Settings; both stay off, and nothing is written, until the user turns them on. */
export interface ShellIntegration {
  /** "Open with Rainy" on files, folders and the background of a folder in Explorer. */
  readonly contextMenu: boolean
  /** Rainy listed in "Open with" and in the Default apps settings for the file types below. */
  readonly fileTypes: boolean
}

/** One change under HKEY_CURRENT_USER; `name` '' is the key's default value. */
export type RegistryOperation =
  | { readonly op: 'set'; readonly key: string; readonly name: string; readonly value: string }
  /** Delete the key when it exists; then each parent, in order, while removing it left the parent empty. */
  | { readonly op: 'removeKey'; readonly key: string; readonly parents: readonly string[] }
  /** Delete Rainy's value when it is there; then the key and its parents, in order, while that left them empty. A key
   *  the user already had, empty or not, is never deleted for lacking Rainy's value. */
  | { readonly op: 'removeValue'; readonly key: string; readonly name: string; readonly parents: readonly string[]; readonly keepKey?: true }

/** A file type Rainy registers, with its name in both interface languages. */
interface FileType { readonly extensions: readonly string[]; readonly zh: string; readonly en: string }

const type = (extensions: string, zh: string, en: string): FileType => ({ extensions: extensions.split(' '), zh, en })

/** Text formats Rainy edits: documents, data and source code. Office files, PDFs and media are previews only and are left
 *  to the applications made for them. */
export const FILE_TYPES: readonly FileType[] = [
  type('md markdown mdown mkd', 'Markdown 文档', 'Markdown Document'),
  type('mdx', 'MDX 文档', 'MDX Document'),
  type('txt text', '文本文档', 'Text Document'),
  type('log', '日志文件', 'Log File'),
  type('json jsonc json5', 'JSON 文件', 'JSON File'),
  type('yaml yml', 'YAML 文件', 'YAML File'),
  type('toml', 'TOML 文件', 'TOML File'),
  type('ini cfg conf properties', '配置文件', 'Configuration File'),
  type('env', '环境变量文件', 'Environment File'),
  type('editorconfig gitignore gitattributes', '配置文件', 'Configuration File'),
  type('csv', 'CSV 文件', 'CSV File'),
  type('tsv', 'TSV 文件', 'TSV File'),
  type('xml', 'XML 文件', 'XML File'),
  type('svg', 'SVG 图像', 'SVG Image'),
  type('html htm', 'HTML 文件', 'HTML File'),
  type('css', 'CSS 文件', 'CSS File'),
  type('scss sass less', '样式表文件', 'Stylesheet'),
  type('js mjs cjs jsx', 'JavaScript 文件', 'JavaScript File'),
  type('ts mts cts tsx', 'TypeScript 文件', 'TypeScript File'),
  type('vue', 'Vue 组件', 'Vue Component'),
  type('svelte', 'Svelte 组件', 'Svelte Component'),
  type('py pyi', 'Python 源文件', 'Python Source File'),
  type('c', 'C 源文件', 'C Source File'),
  type('h', 'C 头文件', 'C Header File'),
  type('cpp cc cxx', 'C++ 源文件', 'C++ Source File'),
  type('hpp hh hxx', 'C++ 头文件', 'C++ Header File'),
  type('cs', 'C# 源文件', 'C# Source File'),
  type('java', 'Java 源文件', 'Java Source File'),
  type('kt kts', 'Kotlin 源文件', 'Kotlin Source File'),
  type('go', 'Go 源文件', 'Go Source File'),
  type('rs', 'Rust 源文件', 'Rust Source File'),
  type('swift', 'Swift 源文件', 'Swift Source File'),
  type('php', 'PHP 源文件', 'PHP Source File'),
  type('rb', 'Ruby 源文件', 'Ruby Source File'),
  type('lua', 'Lua 源文件', 'Lua Source File'),
  type('r', 'R 源文件', 'R Source File'),
  type('dart', 'Dart 源文件', 'Dart Source File'),
  type('scala', 'Scala 源文件', 'Scala Source File'),
  type('sql', 'SQL 文件', 'SQL File'),
  type('sh bash zsh', 'Shell 脚本', 'Shell Script'),
  type('ps1 psm1', 'PowerShell 脚本', 'PowerShell Script'),
  type('bat cmd', '批处理文件', 'Batch File'),
  type('cmake', 'CMake 文件', 'CMake File'),
  type('tex', 'LaTeX 文档', 'LaTeX Document'),
  type('bib', 'BibTeX 文献库', 'BibTeX Library'),
  type('rst', 'reStructuredText 文档', 'reStructuredText Document'),
  type('adoc', 'AsciiDoc 文档', 'AsciiDoc Document'),
  type('diff patch', '补丁文件', 'Patch File'),
]

/** Every registered extension, without its dot. */
export const SHELL_EXTENSIONS: readonly string[] = FILE_TYPES.flatMap(entry => entry.extensions)

const CLASSES = 'Software\\Classes'
/** The verb's key name under each shell key. */
const VERB = 'Rainy'
const CONTEXT_KEYS = [`${CLASSES}\\*\\shell\\${VERB}`, `${CLASSES}\\Directory\\shell\\${VERB}`, `${CLASSES}\\Directory\\Background\\shell\\${VERB}`]
/** Keys above each verb that writing it may have created, children first; each goes only once nothing else uses it. */
const CONTEXT_PARENTS: readonly (readonly string[])[] = [
  [`${CLASSES}\\*\\shell`, `${CLASSES}\\*`],
  [`${CLASSES}\\Directory\\shell`, `${CLASSES}\\Directory`],
  [`${CLASSES}\\Directory\\Background\\shell`, `${CLASSES}\\Directory\\Background`, `${CLASSES}\\Directory`],
]
const VENDOR = 'Software\\RainyMarks'
const CAPABILITIES = `${VENDOR}\\Rainy\\Capabilities`
/** The name Rainy is listed under in Default apps; `ms-settings:defaultapps?registeredAppUser=` takes the same name. */
export const REGISTERED_APPLICATION = 'Rainy'

/** @param extension Extension without its dot. @returns Rainy's ProgID for that type. */
export const progId = (extension: string): string => `Rainy.${extension}`

/** @param executable Absolute path of RainyAgent.exe. @returns the command Explorer runs; `%1` is a file, `%V` a folder. */
export function launchCommand(executable: string, placeholder: '%1' | '%V' = '%1'): string {
  return `"${executable}" "${placeholder}"`
}

/** @param locale Interface language. @returns The context menu label. */
export function contextMenuLabel(locale: Locale): string {
  return locale === 'zh' ? '通过 Rainy 打开' : 'Open with Rainy'
}

/**
 * The registry changes that bring the current user's registration to `wanted`: parts turned off are removed, parts turned
 * on are (re)written for this executable and language.
 * @param executable Absolute path of RainyAgent.exe.
 * @param locale Language of the menu label and the file type names.
 * @param wanted What the user turned on.
 * @param root Key every path is placed under; tests pass a scratch key.
 * @param keep Keys the user had before Rainy wrote to them ({@link removableKeys}); they stay even when left empty.
 * @param recorded Parts whose existing keys were looked up when they were turned on. For a part without that record (written
 *   by 3.9.0, or preferences that cannot be read) only Rainy's own keys and values go; the keys above them stay.
 * @param present Parts Rainy's registration has now; a part that is not there is not removed, so keys Windows or the user
 *   created under the same names (Applications\RainyAgent.exe from "Choose another app") are left alone.
 * @returns The operations, in order.
 */
export function integrationOperations(executable: string, locale: Locale, wanted: ShellIntegration, root = '',
  keep: ReadonlySet<string> = new Set(), recorded: ShellIntegration = BOTH, present: ShellIntegration = BOTH): RegistryOperation[] {
  const key = (path: string): string => root === '' ? path : `${root}\\${path}`
  const kept = (path: string): boolean => keep.has(path.toLowerCase())
  // The walk up stops below the first key the user had: nothing above it can be left empty by Rainy.
  const above = (part: keyof ShellIntegration, parents: readonly string[]): string[] => {
    if (!recorded[part]) return []
    const at = parents.findIndex(kept)
    return (at < 0 ? parents : parents.slice(0, at)).map(key)
  }
  const removeValue = (part: keyof ShellIntegration, path: string, name: string, parents: readonly string[]): RegistryOperation =>
    kept(path) || !recorded[part] ? { op: 'removeValue', key: key(path), name, parents: [], keepKey: true }
      : { op: 'removeValue', key: key(path), name, parents: above(part, parents) }
  const operations: RegistryOperation[] = []
  const icon = `"${executable}",0`
  if (wanted.contextMenu) {
    const label = contextMenuLabel(locale)
    for (const [index, verb] of CONTEXT_KEYS.entries()) {
      operations.push({ op: 'set', key: key(verb), name: '', value: label }, { op: 'set', key: key(verb), name: 'Icon', value: icon },
        { op: 'set', key: key(`${verb}\\command`), name: '', value: launchCommand(executable, index === 0 ? '%1' : '%V') })
    }
  } else if (present.contextMenu) {
    operations.push(...CONTEXT_KEYS.map((verb, index) => ({ op: 'removeKey', key: key(verb), parents: above('contextMenu', CONTEXT_PARENTS[index]!) }) as const))
  }
  const application = `${CLASSES}\\Applications\\RainyAgent.exe`
  if (wanted.fileTypes) {
    const description = locale === 'zh' ? 'AI 编程、Markdown 写作与科研办公工作台' : 'Workbench for AI coding, Markdown writing and research'
    operations.push({ op: 'set', key: key(application), name: 'FriendlyAppName', value: 'Rainy' },
      { op: 'set', key: key(`${application}\\shell\\open\\command`), name: '', value: launchCommand(executable) },
      { op: 'set', key: key(CAPABILITIES), name: 'ApplicationName', value: 'Rainy' },
      { op: 'set', key: key(CAPABILITIES), name: 'ApplicationDescription', value: description },
      { op: 'set', key: key(CAPABILITIES), name: 'ApplicationIcon', value: icon },
      { op: 'set', key: key('Software\\RegisteredApplications'), name: REGISTERED_APPLICATION, value: key(CAPABILITIES) })
    for (const entry of FILE_TYPES) {
      for (const extension of entry.extensions) {
        const id = progId(extension)
        operations.push({ op: 'set', key: key(`${CLASSES}\\${id}`), name: '', value: entry[locale] },
          { op: 'set', key: key(`${CLASSES}\\${id}\\DefaultIcon`), name: '', value: icon },
          { op: 'set', key: key(`${CLASSES}\\${id}\\shell\\open\\command`), name: '', value: launchCommand(executable) },
          { op: 'set', key: key(`${CLASSES}\\.${extension}\\OpenWithProgids`), name: id, value: '' },
          { op: 'set', key: key(`${application}\\SupportedTypes`), name: `.${extension}`, value: '' },
          { op: 'set', key: key(`${CAPABILITIES}\\FileAssociations`), name: `.${extension}`, value: id })
      }
    }
  } else if (present.fileTypes) {
    // Windows writes the same application key when the user picks RainyAgent.exe under "Choose another app"; when it was
    // there before, only what Rainy added to it goes.
    operations.push(...kept(application)
      ? [removeValue('fileTypes', application, 'FriendlyAppName', []), { op: 'removeKey', key: key(`${application}\\SupportedTypes`), parents: [] } as const]
      : [{ op: 'removeKey', key: key(application), parents: above('fileTypes', [`${CLASSES}\\Applications`]) } as const],
    removeValue('fileTypes', 'Software\\RegisteredApplications', REGISTERED_APPLICATION, []),
    { op: 'removeKey', key: key(`${VENDOR}\\Rainy`), parents: above('fileTypes', [VENDOR]) })
    for (const extension of SHELL_EXTENSIONS) {
      const type = `${CLASSES}\\.${extension}`
      operations.push({ op: 'removeKey', key: key(`${CLASSES}\\${progId(extension)}`), parents: [] },
        removeValue('fileTypes', `${type}\\OpenWithProgids`, progId(extension), [type]))
    }
  }
  return operations
}

/**
 * Keys above Rainy's own that turning a part off may delete once they are empty: the ones writing it may have created.
 * @param parts The parts about to be turned on.
 * @returns Paths relative to the scratch root, as the operations name them.
 */
export function removableKeys(parts: ShellIntegration): string[] {
  const keys: string[] = []
  if (parts.contextMenu) keys.push(...new Set(CONTEXT_PARENTS.flat()))
  if (parts.fileTypes) {
    keys.push(`${CLASSES}\\Applications`, `${CLASSES}\\Applications\\RainyAgent.exe`, 'Software\\RegisteredApplications', VENDOR)
    for (const extension of SHELL_EXTENSIONS) keys.push(`${CLASSES}\\.${extension}`, `${CLASSES}\\.${extension}\\OpenWithProgids`)
  }
  return keys
}

/**
 * @param keys Paths under the current user (and the scratch root).
 * @param root Scratch key for tests; empty in the application.
 * @returns The keys that exist now, lower-cased, so they can be kept when Rainy's entries are removed later.
 */
export async function existingKeys(keys: readonly string[], root = ''): Promise<string[]> {
  if (keys.length === 0) return []
  const key = (path: string): string => root === '' ? path : `${root}\\${path}`
  const found = await runRegistry(keys.map(path => ({ op: 'exists', key: key(path) }) as const))
  return keys.filter((_path, index) => found[index] === true).map(path => path.toLowerCase())
}

/** What the registry holds now: the context menu's label and command, and the command of Rainy's Markdown type. */
export interface ShellIntegrationStatus {
  readonly contextMenuLabel: string | null
  readonly contextMenuCommand: string | null
  readonly fileTypeCommand: string | null
}

/** Applies a JSON list of operations, or reads values, under HKEY_CURRENT_USER with the .NET registry API: one process, and
 *  Unicode names and paths stay intact. */
const SCRIPT = String.raw`
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [Text.Encoding]::UTF8
$root = [Microsoft.Win32.Registry]::CurrentUser
# Windows PowerShell 5.1 passes a parsed JSON array down the pipeline as one object; the variable keeps it an array.
$operations = ConvertFrom-Json -InputObject (Get-Content -LiteralPath $env:RAINY_REGISTRY_OPERATIONS -Raw -Encoding UTF8)
$read = New-Object System.Collections.ArrayList
$changed = $false
function Test-Empty([string]$path) {
  $key = $root.OpenSubKey($path)
  if ($null -eq $key) { return $false }
  try { return $key.SubKeyCount -eq 0 -and $key.ValueCount -eq 0 } finally { $key.Close() }
}
# Parents go one by one while each is left empty; the first one still in use stops the walk.
function Remove-EmptyParents($parents) {
  foreach ($parent in @($parents)) {
    if (-not (Test-Empty ([string]$parent))) { break }
    $root.DeleteSubKey([string]$parent, $false)
  }
}
foreach ($operation in $operations) {
  $name = [string]$operation.name
  if ($operation.op -ne 'read' -and $operation.op -ne 'exists') { $changed = $true }
  switch ($operation.op) {
    'set' {
      $key = $root.CreateSubKey([string]$operation.key)
      try { $key.SetValue($name, [string]$operation.value, [Microsoft.Win32.RegistryValueKind]::String) } finally { $key.Close() }
    }
    'removeKey' {
      $key = $root.OpenSubKey([string]$operation.key)
      if ($null -ne $key) {
        $key.Close()
        $root.DeleteSubKeyTree([string]$operation.key, $false)
        Remove-EmptyParents $operation.parents
      }
    }
    'removeValue' {
      $key = $root.OpenSubKey([string]$operation.key, $true)
      if ($null -ne $key) {
        $had = $false
        try {
          $had = @($key.GetValueNames()) -contains $name
          if ($had) { $key.DeleteValue($name, $false) }
        } finally { $key.Close() }
        if ($had -and -not $operation.keepKey -and (Test-Empty ([string]$operation.key))) {
          $root.DeleteSubKey([string]$operation.key, $false)
          Remove-EmptyParents $operation.parents
        }
      }
    }
    'exists' {
      $key = $root.OpenSubKey([string]$operation.key)
      [void]$read.Add($null -ne $key)
      if ($null -ne $key) { $key.Close() }
    }
    'read' {
      $key = $root.OpenSubKey([string]$operation.key)
      $value = $null
      if ($null -ne $key) { try { $value = $key.GetValue($name, $null) } finally { $key.Close() } }
      [void]$read.Add($value)
    }
  }
}
# Explorer caches menus, icons and Open with lists per type until it is told the associations changed.
if ($changed) {
  Add-Type -Namespace Rainy -Name Shell -MemberDefinition '[DllImport("shell32.dll")] public static extern void SHChangeNotify(int eventId, uint flags, IntPtr item1, IntPtr item2);'
  [Rainy.Shell]::SHChangeNotify(0x08000000, 0, [IntPtr]::Zero, [IntPtr]::Zero)
}
Write-Output ('RAINY_REGISTRY ' + (ConvertTo-Json -InputObject @($read.ToArray()) -Compress))
`

type ReadOperation = { readonly op: 'read'; readonly key: string; readonly name: string } | { readonly op: 'exists'; readonly key: string }

/** Run the registry script with `operations`; a temporary file carries them, since the list is longer than a command line. */
async function runRegistry(operations: readonly (RegistryOperation | ReadOperation)[], timeoutMs = 60_000): Promise<unknown[]> {
  const folder = await mkdtemp(join(tmpdir(), 'rainy-shell-'))
  try {
    const list = join(folder, 'operations.json')
    await writeFile(list, JSON.stringify(operations), 'utf8')
    const output = await new Promise<string>((resolve, reject) => {
      execFile(windowsPowerShellPath(), ['-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-EncodedCommand', Buffer.from(SCRIPT, 'utf16le').toString('base64')],
        { env: { ...process.env, RAINY_REGISTRY_OPERATIONS: list }, timeout: timeoutMs, windowsHide: true, encoding: 'utf8', maxBuffer: 1024 * 1024 },
        (error, stdout, stderr) => {
          if (error !== null) reject(new Error(`Updating the registry failed: ${stderr.trim() || error.message}`))
          else resolve(stdout)
        })
    })
    const line = output.split(/\r?\n/u).find(value => value.startsWith('RAINY_REGISTRY '))
    if (line === undefined) throw new Error('The registry script returned no result.')
    const values: unknown = JSON.parse(line.slice('RAINY_REGISTRY '.length))
    return Array.isArray(values) ? values : [values]
  } finally {
    await rm(folder, { recursive: true, force: true }).catch(() => undefined)
  }
}

/**
 * Write or remove the registration so it matches `wanted`.
 * @param executable Absolute path of RainyAgent.exe.
 * @param locale Language of the labels.
 * @param wanted What the user turned on.
 * @param root Scratch key for tests; empty in the application.
 * @param memory Keys the user had before Rainy wrote to them ({@link existingKeys}) and the parts that looked them up.
 * @param present Parts Rainy's registration has now; the uninstaller passes both, so everything of Rainy's goes.
 */
export async function applyShellIntegration(executable: string, locale: Locale, wanted: ShellIntegration, root = '',
  memory: ShellMemory = NO_MEMORY, present: ShellIntegration = BOTH): Promise<void> {
  await runRegistry(integrationOperations(executable, locale, wanted, root, new Set(memory.keep.map(path => path.toLowerCase())), memory.recorded, present))
}

/** What Rainy remembers about the registry it wrote to: the keys that were there before, and the parts that looked. */
export interface ShellMemory {
  readonly keep: readonly string[]
  readonly recorded: ShellIntegration
}

/** Nothing remembered: removal deletes only Rainy's own keys and values. */
export const NO_MEMORY: ShellMemory = { keep: [], recorded: { contextMenu: false, fileTypes: false } }

/**
 * Bring the registry to `wanted`, remembering the keys the user already had for parts being turned on now, so turning them
 * off later removes only what Rainy added.
 * @param executable Absolute path of RainyAgent.exe.
 * @param locale Language of the labels.
 * @param wanted What the user turned on.
 * @param memory What was remembered when the parts were turned on earlier.
 * @param root Scratch key for tests; empty in the application.
 * @param remember Saves the memory of a part being turned on before anything is written, so a write cut short cannot lose it.
 * @returns Whether the registry was changed, and the memory to keep from now on.
 */
export async function syncShellIntegration(executable: string, locale: Locale, wanted: ShellIntegration, memory: ShellMemory,
  root = '', remember: (memory: ShellMemory) => Promise<void> = async () => undefined): Promise<{ changed: boolean; memory: ShellMemory }> {
  const status = await readShellIntegration(root)
  if (shellIntegrationCurrent(status, executable, locale, wanted)) return { changed: false, memory }
  const present = { contextMenu: status.contextMenuCommand !== null, fileTypes: status.fileTypeCommand !== null }
  const turningOn = { contextMenu: wanted.contextMenu && !present.contextMenu, fileTypes: wanted.fileTypes && !present.fileTypes }
  let before = memory
  if (turningOn.contextMenu || turningOn.fileTypes) {
    const found = await existingKeys(removableKeys(turningOn), root)
    before = { keep: [...new Set([...memory.keep.map(path => path.toLowerCase()), ...found])],
      recorded: { contextMenu: memory.recorded.contextMenu || turningOn.contextMenu, fileTypes: memory.recorded.fileTypes || turningOn.fileTypes } }
    await remember(before)
  }
  await applyShellIntegration(executable, locale, wanted, root, before, present)
  // A part that is off now has nothing of Rainy's left; the next time it is turned on, it looks again.
  const off = removableKeys({ contextMenu: !wanted.contextMenu, fileTypes: !wanted.fileTypes }).map(path => path.toLowerCase())
  const kept = before.keep.filter(path => !off.includes(path))
  return { changed: true, memory: { keep: kept,
    recorded: { contextMenu: wanted.contextMenu && before.recorded.contextMenu, fileTypes: wanted.fileTypes && before.recorded.fileTypes } } }
}


/**
 * @param preferences Contents of desktop.json.
 * @returns The keys remembered as the user's own when a part was turned on, and which parts looked them up.
 */
export function shellMemory(preferences: Record<string, unknown>): ShellMemory {
  const saved = preferences.explorerIntegration
  const record: Record<string, unknown> = saved !== null && typeof saved === 'object' ? saved as Record<string, unknown> : {}
  const recorded: Record<string, unknown> = record.recorded !== null && typeof record.recorded === 'object' ? record.recorded as Record<string, unknown> : {}
  return { keep: Array.isArray(record.keep) ? record.keep.filter((path): path is string => typeof path === 'string') : [],
    recorded: { contextMenu: recorded.contextMenu === true, fileTypes: recorded.fileTypes === true } }
}

/**
 * @param root Scratch key for tests; empty in the application.
 * @returns The values that tell whether the registration is present and current.
 */
export async function readShellIntegration(root = ''): Promise<ShellIntegrationStatus> {
  const key = (path: string): string => root === '' ? path : `${root}\\${path}`
  const [label, command, fileType] = await runRegistry([
    { op: 'read', key: key(CONTEXT_KEYS[0]!), name: '' },
    { op: 'read', key: key(`${CONTEXT_KEYS[0]!}\\command`), name: '' },
    { op: 'read', key: key(`${CLASSES}\\${progId('md')}\\shell\\open\\command`), name: '' },
  ])
  const text = (value: unknown): string | null => typeof value === 'string' ? value : null
  return { contextMenuLabel: text(label), contextMenuCommand: text(command), fileTypeCommand: text(fileType) }
}

/**
 * @param status What the registry holds.
 * @param executable This installation's executable.
 * @param locale Current interface language.
 * @param wanted What the user turned on.
 * @returns Whether the registry already matches, so nothing needs to be written.
 */
export function shellIntegrationCurrent(status: ShellIntegrationStatus, executable: string, locale: Locale, wanted: ShellIntegration): boolean {
  const menu = wanted.contextMenu
    ? status.contextMenuCommand === launchCommand(executable) && status.contextMenuLabel === contextMenuLabel(locale)
    : status.contextMenuCommand === null
  const types = wanted.fileTypes ? status.fileTypeCommand === launchCommand(executable) : status.fileTypeCommand === null
  return menu && types
}

/**
 * Read the user's choice from the desktop preferences. 3.9.0 kept both parts on unless turned off, under
 * `shellIntegration`; that key is no longer read, so an upgrade removes what 3.9.0 wrote until the user opts in again.
 * @param preferences Contents of desktop.json.
 * @returns The choice; each part is on only when the user turned it on.
 */
export function shellIntegrationPreference(preferences: Record<string, unknown>): ShellIntegration {
  const saved = preferences.explorerIntegration
  const flag = (name: keyof ShellIntegration): boolean =>
    saved !== null && typeof saved === 'object' && (saved as Record<string, unknown>)[name] === true
  return { contextMenu: flag('contextMenu'), fileTypes: flag('fileTypes') }
}
