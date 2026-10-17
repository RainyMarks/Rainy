/** Slides and legacy Word documents as PDF, made by the Microsoft Office or WPS Office installed on this computer. */
import { execFile } from 'node:child_process'
import { createHash } from 'node:crypto'
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { encryptedOpenXml } from '../shared/office-protocol.ts'
import { windowsPowerShellPath } from './powershell.ts'

/** Which Office application opens the file. */
export type OfficeKind = 'slides' | 'word'

const KINDS: Readonly<Record<string, OfficeKind>> = {
  pptx: 'slides', pptm: 'slides', ppsx: 'slides', ppsm: 'slides', potx: 'slides', ppt: 'slides', pps: 'slides', pot: 'slides', odp: 'slides', dps: 'slides',
  doc: 'word', dot: 'word', wps: 'word', odt: 'word', docx: 'word', docm: 'word', dotx: 'word', dotm: 'word', rtf: 'word',
}

/** Largest file handed to Office, in bytes. */
export const OFFICE_MAX_BYTES = 200 * 1024 * 1024

/** Neither Microsoft Office nor WPS Office could convert the file. `missing` means neither is installed; `encrypted` that the
 *  file needs a password to open. */
export class OfficeConversionError extends Error {
  constructor(message: string, readonly missing: boolean, readonly encrypted = false) {
    super(message)
    this.name = 'OfficeConversionError'
  }
}

/**
 * @param extension Lower-case extension without the dot.
 * @returns The application family, or `undefined` for files Office is not asked to convert.
 */
export function officeKind(extension: string): OfficeKind | undefined {
  return Object.hasOwn(KINDS, extension) ? KINDS[extension] : undefined
}

// PowerPoint and WPS keep one process per user, so an instance that was already running belongs to the user and is
// left open; Word.Application always starts its own instance. The password argument makes Word fail on a protected
// document (error 5408) instead of asking for the password in a hidden window. SaveAs2 can wait forever on a hidden
// prompt in current Word builds, so the fallback is the older SaveAs.
const SCRIPT = String.raw`
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
$in = $env:RAINY_OFFICE_IN
$out = $env:RAINY_OFFICE_OUT
$slides = $env:RAINY_OFFICE_KIND -eq 'slides'
$candidates = if ($slides) { @(@('PowerPoint.Application', 'POWERPNT', 'Microsoft PowerPoint'), @('KWPP.Application', 'wpp', 'WPS Office')) }
  else { @(@('Word.Application', 'WINWORD', 'Microsoft Word'), @('KWps.Application', 'wps', 'WPS Office')) }
$failures = @()
$found = $false
$encrypted = $false
foreach ($candidate in $candidates) {
  $progId = $candidate[0]
  if (-not (Test-Path -LiteralPath "Registry::HKEY_CLASSES_ROOT\$progId\CLSID")) { continue }
  $found = $true
  $wasRunning = $null -ne (Get-Process -Name $candidate[1] -ErrorAction SilentlyContinue)
  $app = $null
  $file = $null
  try {
    $app = New-Object -ComObject $progId
    if ($slides) {
      $file = $app.Presentations.Open($in, -1, 0, 0)
      $file.SaveAs($out, 32)
    } else {
      try { $app.Visible = $false } catch {}
      try { $app.DisplayAlerts = 0 } catch {}
      $file = $app.Documents.Open($in, $false, $true, $false, 'rainy-no-password')
      try { $file.ExportAsFixedFormat($out, 17) } catch { $file.SaveAs([ref]$out, [ref]17) }
    }
    if (Test-Path -LiteralPath $out) { Write-Output ('OK ' + $candidate[2]); exit 0 }
    $failures += $candidate[2] + ': no PDF was written'
  } catch {
    if ($_.Exception.HResult -eq -2146822880) { $encrypted = $true }
    $failures += $candidate[2] + ': ' + $_.Exception.Message
  } finally {
    if ($null -ne $file) { try { if ($slides) { $file.Close() } else { $file.Close(0) } } catch {} }
    if ($null -ne $app -and ($progId -eq 'Word.Application' -or -not $wasRunning)) { try { $app.Quit() } catch {} }
    foreach ($object in @($file, $app)) { if ($null -ne $object) { try { [void][Runtime.InteropServices.Marshal]::ReleaseComObject($object) } catch {} } }
  }
  if ($encrypted) { break }
}
if (-not $found) { Write-Output 'MISSING'; exit 3 }
if ($encrypted) { Write-Output 'PASSWORD'; exit 5 }
Write-Output ('FAILED ' + ($failures -join ' | '))
exit 4
`

function run(environment: NodeJS.ProcessEnv, timeoutMs: number): Promise<string> {
  return new Promise((resolve) => {
    execFile(windowsPowerShellPath(), ['-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-EncodedCommand', Buffer.from(SCRIPT, 'utf16le').toString('base64')],
      { env: environment, timeout: timeoutMs, windowsHide: true, encoding: 'utf8', maxBuffer: 1024 * 1024 },
      (error, stdout) => {
        const output = stdout.trim().split(/\r?\n/u).at(-1) ?? ''
        resolve(output !== '' ? output : error?.killed === true ? 'TIMEOUT' : `FAILED ${error?.message ?? 'no output'}`)
      })
  })
}

/**
 * Convert one document; the input is written to a private temporary folder that is removed afterwards.
 * @param bytes File contents.
 * @param extension Lower-case extension, which tells Office the format.
 * @param timeoutMs Time allowed for starting Office and exporting.
 * @returns The PDF and the application that made it.
 */
export async function convertOfficeToPdf(bytes: Uint8Array, extension: string, timeoutMs = 120_000): Promise<{ pdf: Buffer; application: string }> {
  const kind = officeKind(extension)
  if (kind === undefined) throw new OfficeConversionError(`.${extension} files are not converted`, false)
  if (bytes.length > OFFICE_MAX_BYTES) throw new OfficeConversionError('The file is too large to convert', false)
  // Office would ask for the password in a window nobody sees, or in the user's own PowerPoint.
  if (encryptedOpenXml(bytes, extension)) throw new OfficeConversionError('The file is protected with a password', false, true)
  const folder = await mkdtemp(join(tmpdir(), 'rainy-office-'))
  try {
    const input = join(folder, `document.${extension}`)
    const output = join(folder, 'document.pdf')
    await writeFile(input, bytes)
    const result = await run({ ...process.env, RAINY_OFFICE_IN: input, RAINY_OFFICE_OUT: output, RAINY_OFFICE_KIND: kind }, timeoutMs)
    if (result === 'MISSING') throw new OfficeConversionError('Microsoft Office or WPS Office is not installed', true)
    if (result === 'PASSWORD') throw new OfficeConversionError('The file is protected with a password', false, true)
    if (result === 'TIMEOUT') throw new OfficeConversionError('Office did not finish converting in time', false)
    if (!result.startsWith('OK ')) throw new OfficeConversionError(result.replace(/^FAILED /u, ''), false)
    return { pdf: await readFile(output), application: result.slice(3) }
  } finally {
    await rm(folder, { recursive: true, force: true, maxRetries: 5, retryDelay: 200 }).catch(() => undefined)
  }
}

/** Remembers recent conversions, so switching back to a tab does not start Office again. */
export class OfficeConverter {
  private readonly results = new Map<string, Promise<{ pdf: Buffer; application: string }>>()

  constructor(private readonly convert: typeof convertOfficeToPdf = convertOfficeToPdf, private readonly keep = 6) {}

  /**
   * @param bytes File contents.
   * @param extension Lower-case extension.
   * @returns The PDF and the application that made it; a failed conversion is not remembered.
   */
  toPdf(bytes: Uint8Array, extension: string): Promise<{ pdf: Buffer; application: string }> {
    const key = createHash('sha256').update(extension).update('\0').update(bytes).digest('hex')
    const known = this.results.get(key)
    if (known !== undefined) {
      this.results.delete(key)
      this.results.set(key, known)
      return known
    }
    const task = this.convert(bytes, extension)
    task.catch(() => { if (this.results.get(key) === task) this.results.delete(key) })
    this.results.set(key, task)
    while (this.results.size > this.keep) this.results.delete(this.results.keys().next().value!)
    return task
  }
}
