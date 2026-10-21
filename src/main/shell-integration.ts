/** Explorer's "Open with Rainy" entries and the file types Rainy offers to open, kept in the current user's registry.
 *  Nothing here needs administrator rights, and Windows never lets an application make itself the default app: the
 *  user picks Rainy in "Open with" or in the Default apps settings. */
import { execFile } from 'node:child_process'
import { mkdtemp, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import type { Locale } from '../shared/locale.ts'
import { windowsPowerShellPath } from './powershell.ts'

/** What the user turned on in Settings; both are on until the user turns them off. */
export interface ShellIntegration {
  /** "Open with Rainy" on files, folders and the background of a folder in Explorer. */
  readonly contextMenu: boolean
  /** Rainy listed in "Open with" and in the Default apps settings for the file types below. */
  readonly fileTypes: boolean
}

/** One change under HKEY_CURRENT_USER; `name` '' is the key's default value. */
export type RegistryOperation =
  | { readonly op: 'set'; readonly key: string; readonly name: string; readonly value: string }
  | { readonly op: 'deleteKey'; readonly key: string }
  | { readonly op: 'deleteValue'; readonly key: string; readonly name: string }
  /** Delete the key when nothing is left in it, so the vendor key goes once Rainy's own key is removed. */
  | { readonly op: 'deleteEmptyKey'; readonly key: string }

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
/** Keys above the verbs that writing them may have created, children first; each is removed only once nothing else uses it. */
const CONTEXT_PARENTS = [`${CLASSES}\\*\\shell`, `${CLASSES}\\*`, `${CLASSES}\\Directory\\Background\\shell`, `${CLASSES}\\Directory\\Background`,
  `${CLASSES}\\Directory\\shell`, `${CLASSES}\\Directory`]
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
 * @returns The operations, in order.
 */
export function integrationOperations(executable: string, locale: Locale, wanted: ShellIntegration, root = ''): RegistryOperation[] {
  const key = (path: string): string => root === '' ? path : `${root}\\${path}`
  const operations: RegistryOperation[] = []
  const icon = `"${executable}",0`
  if (wanted.contextMenu) {
    const label = contextMenuLabel(locale)
    for (const [index, verb] of CONTEXT_KEYS.entries()) {
      operations.push({ op: 'set', key: key(verb), name: '', value: label }, { op: 'set', key: key(verb), name: 'Icon', value: icon },
        { op: 'set', key: key(`${verb}\\command`), name: '', value: launchCommand(executable, index === 0 ? '%1' : '%V') })
    }
  } else {
    operations.push(...CONTEXT_KEYS.map(verb => ({ op: 'deleteKey', key: key(verb) }) as const),
      ...CONTEXT_PARENTS.map(parent => ({ op: 'deleteEmptyKey', key: key(parent) }) as const))
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
  } else {
    operations.push({ op: 'deleteKey', key: key(application) }, { op: 'deleteEmptyKey', key: key(`${CLASSES}\\Applications`) },
      { op: 'deleteValue', key: key('Software\\RegisteredApplications'), name: REGISTERED_APPLICATION },
      { op: 'deleteEmptyKey', key: key('Software\\RegisteredApplications') },
      { op: 'deleteKey', key: key(`${VENDOR}\\Rainy`) }, { op: 'deleteEmptyKey', key: key(VENDOR) })
    for (const extension of SHELL_EXTENSIONS) {
      const type = `${CLASSES}\\.${extension}`
      operations.push({ op: 'deleteKey', key: key(`${CLASSES}\\${progId(extension)}`) },
        { op: 'deleteValue', key: key(`${type}\\OpenWithProgids`), name: progId(extension) },
        { op: 'deleteEmptyKey', key: key(`${type}\\OpenWithProgids`) }, { op: 'deleteEmptyKey', key: key(type) })
    }
  }
  return operations
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
foreach ($operation in $operations) {
  $name = [string]$operation.name
  if ($operation.op -ne 'read') { $changed = $true }
  switch ($operation.op) {
    'set' {
      $key = $root.CreateSubKey([string]$operation.key)
      try { $key.SetValue($name, [string]$operation.value, [Microsoft.Win32.RegistryValueKind]::String) } finally { $key.Close() }
    }
    'deleteKey' { $root.DeleteSubKeyTree([string]$operation.key, $false) }
    'deleteValue' {
      $key = $root.OpenSubKey([string]$operation.key, $true)
      if ($null -ne $key) { try { $key.DeleteValue($name, $false) } finally { $key.Close() } }
    }
    'deleteEmptyKey' {
      $key = $root.OpenSubKey([string]$operation.key)
      if ($null -ne $key) {
        $empty = $key.SubKeyCount -eq 0 -and $key.ValueCount -eq 0
        $key.Close()
        if ($empty) { $root.DeleteSubKey([string]$operation.key, $false) }
      }
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

type ReadOperation = { readonly op: 'read'; readonly key: string; readonly name: string }

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
 */
export async function applyShellIntegration(executable: string, locale: Locale, wanted: ShellIntegration, root = ''): Promise<void> {
  await runRegistry(integrationOperations(executable, locale, wanted, root))
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
 * Read the user's choice from the desktop preferences.
 * @param preferences Contents of desktop.json.
 * @returns The choice; both parts are on until turned off.
 */
export function shellIntegrationPreference(preferences: Record<string, unknown>): ShellIntegration {
  const saved = preferences.shellIntegration
  const flag = (name: keyof ShellIntegration): boolean =>
    saved === null || typeof saved !== 'object' || !(name in saved) || (saved as Record<string, unknown>)[name] !== false
  return { contextMenu: flag('contextMenu'), fileTypes: flag('fileTypes') }
}
