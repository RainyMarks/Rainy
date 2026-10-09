/** Windows PowerShell location and literal strings for scripts the carrier builds itself. */
import { win32 } from 'node:path'

/**
 * Locate the system Windows PowerShell instead of searching the working directory and PATH.
 * @param environment - environment supplying SystemRoot.
 * @returns the absolute System32 executable path.
 */
export function windowsPowerShellPath(environment: NodeJS.ProcessEnv = process.env): string {
  return win32.join(environment.SystemRoot ?? environment.SYSTEMROOT ?? 'C:\Windows', 'System32', 'WindowsPowerShell', 'v1.0', 'powershell.exe')
}

/**
 * Quote a value as a single-quoted PowerShell literal.
 * PowerShell also ends such strings at ‘ ’ ‚ ‛, which may appear in Windows profile paths; every quote is doubled.
 * @param value - literal text, typically a path.
 * @returns the quoted literal.
 */
export function quotePowerShell(value: string): string {
  return `'${value.replace(/['‘-‛]/gu, '$&$&')}'`
}
