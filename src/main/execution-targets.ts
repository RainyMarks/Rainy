/** Carrier target discovery and lossless desktop preference updates. */
import { execFile } from 'node:child_process'
import { readFile, mkdir } from 'node:fs/promises'
import { dirname } from 'node:path'
import { promisify } from 'node:util'
import { z } from 'zod'
import { writeEnvironmentRecord } from './environment.ts'
import { windowsPowerShellPath } from './powershell.ts'
import { ExecutionTargetId } from './project-registry.ts'
import type { ProjectId } from './project-registry.ts'
import type { IdeRootId } from '@deepseek-ai/dsh-client-ui-rainy/ide-files-protocol'

/** One explicit native execution destination. */
export interface ExecutionTarget { id: ExecutionTargetId; kind: 'windows' | 'wsl'; label: string; distro?: string }
/** Pending cross-target project binding consumed only by the new Host. */
export interface PendingProjectTarget {
  projectId: ProjectId
  path: string
  roots?: readonly { rootId: IdeRootId; path: string; title: string }[]
}
const targetSchema = z.discriminatedUnion('kind', [
  z.object({ id: z.literal('windows-local').transform(ExecutionTargetId), kind: z.literal('windows'), label: z.string() }).strict(),
  z.object({ id: z.string().min(1).transform(ExecutionTargetId), kind: z.literal('wsl'), label: z.string(), distro: z.string().min(1).max(256) }).strict(),
])
const run = promisify(execFile)
/** Native Windows is available without any WSL installation. */
export const WINDOWS_TARGET: ExecutionTarget = { id: ExecutionTargetId('windows-local'), kind: 'windows', label: 'Windows' }

/** @param path - private desktop preferences. @returns validated JSON without changing unrelated fields. */
export async function readDesktopPreferences(path: string): Promise<Record<string, unknown>> {
  try { return z.record(z.string(), z.unknown()).parse(JSON.parse(await readFile(path, 'utf8'))) }
  catch (error) { if (error instanceof Error && 'code' in error && error.code === 'ENOENT') return {}; throw error }
}

/** @param preferences - existing desktop settings. @returns the retained WSL choice or the Windows default for a new installation. */
export function savedExecutionTarget(preferences: Record<string, unknown>): ExecutionTarget {
  if (preferences.executionTarget !== undefined) return targetSchema.parse(preferences.executionTarget)
  if (typeof preferences.distro === 'string' && preferences.distro.trim()) return {
    id: ExecutionTargetId(`wsl:${preferences.distro}`), kind: 'wsl', label: `WSL · ${preferences.distro}`, distro: preferences.distro,
  }
  return WINDOWS_TARGET
}

/** @returns Windows and registered WSL2 destinations; no distribution is started during discovery. */
export async function listExecutionTargets(): Promise<ExecutionTarget[]> {
  const script = String.raw`$ErrorActionPreference='Stop'; [Console]::OutputEncoding=[Text.UTF8Encoding]::new($false); $root='HKCU:\Software\Microsoft\Windows\CurrentVersion\Lxss'; $items=@(); if(Test-Path -LiteralPath $root){$items=@(Get-ChildItem -LiteralPath $root | ForEach-Object { $v=Get-ItemProperty -LiteralPath $_.PSPath; if($v.Version -eq 2){[pscustomobject]@{id=$_.PSChildName;name=$v.DistributionName}} })}; ConvertTo-Json -InputObject $items -Compress`
  const output = await run(windowsPowerShellPath(), ['-NoProfile', '-NonInteractive', '-EncodedCommand', Buffer.from(script, 'utf16le').toString('base64')], { windowsHide: true, timeout: 15000, maxBuffer: 256 * 1024, encoding: 'utf8' })
  const values = z.array(z.object({ id: z.string(), name: z.string().min(1) }).strict()).parse(JSON.parse(output.stdout.trim() || '[]'))
  return [WINDOWS_TARGET, ...values.filter(value => !/^docker-desktop(?:-data)?$/u.test(value.name)).map(value => ({
    id: ExecutionTargetId(`wsl:${value.id.toLowerCase()}`), kind: 'wsl' as const, label: `WSL · ${value.name}`, distro: value.name,
  }))]
}

/**
 * @param settingsPath - private desktop preferences.
 * @param target - inspected destination.
 * @param pending - optional existing project's mapped root.
 * @returns completion after an atomic preference update.
 */
export async function saveExecutionTarget(settingsPath: string, target: ExecutionTarget, pending?: PendingProjectTarget): Promise<void> {
  const preferences = await readDesktopPreferences(settingsPath)
  await mkdir(dirname(settingsPath), { recursive: true })
  const updated = { ...preferences, executionTarget: targetSchema.parse(target), ...(target.kind === 'wsl' ? { distro: target.distro } : {}),
    pendingProject: pending ?? null }
  await writeEnvironmentRecord(settingsPath, updated)
}
