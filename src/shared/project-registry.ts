/** Carrier-owned project identity shared by native Windows and WSL Hosts. */
import { mkdir, open, readFile, rename, unlink } from 'node:fs/promises'
import { join, posix, win32, resolve } from 'node:path'
import { randomUUID } from 'node:crypto'
import { brandString, type Branded } from './brand.ts'
import { z } from 'zod'
import type { WorkspaceId } from './ide-files-protocol.ts'

/** Stable public project identity; migration retains the original WorkspaceId bytes. */
export type ProjectId = Branded<'RainyProjectId'>
/** One native Windows installation or registered WSL distribution. */
export type ExecutionTargetId = Branded<'RainyExecutionTargetId'>
/** A host-local workspace associated with the public project. */
export interface ProjectBinding { targetId: ExecutionTargetId; workspaceId: WorkspaceId; path: string }
/** Shared metadata; sessions and editor recovery remain in their owning Host. */
export interface ProjectRecord { projectId: ProjectId; title: string; activeTargetId: ExecutionTargetId; bindings: ProjectBinding[] }
/** Versioned carrier catalog; one active Host owns writes during a window lifetime. */
export interface ProjectCatalog { version: 1; projects: ProjectRecord[] }

const text = z.string().min(1).max(32768).refine(value => !value.includes('\0'))
const projectId = z.string().regex(/^[A-Za-z0-9_-][A-Za-z0-9_.-]{0,159}$/u).transform(value => brandString<ProjectId>(value))
const targetId = text.transform(value => brandString<ExecutionTargetId>(value))
/** @param value - persisted public project identity. @returns its validated branded identity. */
export function ProjectId(value: string): ProjectId { return projectId.parse(value) }
/** @param value - configured execution target. @returns its validated branded identity. */
export function ExecutionTargetId(value: string): ExecutionTargetId { return targetId.parse(value) }
const catalogSchema: z.ZodType<ProjectCatalog> = z.object({ version: z.literal(1), projects: z.array(z.object({
  projectId, title: text, activeTargetId: targetId,
  bindings: z.array(z.object({ targetId, workspaceId: text.transform(value => brandString<WorkspaceId>(value)), path: text }).strict()),
}).strict()) }).strict().superRefine((value, context) => {
  const projects = new Set<string>()
  const bindings = new Set<string>()
  for (const project of value.projects) {
    if (projects.has(project.projectId)) context.addIssue({ code: 'custom', message: 'Duplicate project identity.' })
    projects.add(project.projectId)
    for (const binding of project.bindings) {
      const key = JSON.stringify([binding.targetId, binding.workspaceId])
      if (bindings.has(key)) context.addIssue({ code: 'custom', message: 'A workspace belongs to more than one project.' })
      bindings.add(key)
      if (!posix.isAbsolute(binding.path) && !win32.isAbsolute(binding.path)) context.addIssue({ code: 'custom', message: 'Project paths must be absolute.' })
    }
  }
})

/** Project registration input; the Host supplies its canonical workspace path. */
export interface ProjectRegistration { workspaceId: WorkspaceId; path: string; title: string; projectId?: ProjectId }
/** Serialized catalog operations shared by every consumer in one Host. */
export interface ProjectRegistry {
  /** @returns the current detached catalog. */
  list(): Promise<ProjectRecord[]>
  /** @param workspaceId - identity in the selected Host. @returns its public project identity, if registered. */
  projectForWorkspace(workspaceId: WorkspaceId): Promise<ProjectId | undefined>
  /**
   * @param registration - canonical workspace and optional existing public identity.
   * @returns its retained or newly registered project identity.
   */
  getOrRegister(registration: ProjectRegistration): Promise<ProjectId>
  /**
   * @param id - existing public identity.
   * @param target - target selected for future conversations.
   * @returns completion after persistence.
   */
  selectTarget(id: ProjectId, target: ExecutionTargetId): Promise<void>
}

const instances = new Map<string, ProjectRegistry>()

/**
 * Create or reuse the sole in-process writer for a carrier catalog.
 * The carrier writes only while its Host is stopped; different execution targets must never run writers concurrently.
 * @param options - local path to shared carrier storage and the current execution target.
 * @returns serialized, lazily opened catalog operations.
 */
export function createProjectRegistry(options: { root: string; targetId: string }): ProjectRegistry {
  const root = resolve(options.root)
  const target = targetId.parse(options.targetId)
  const key = JSON.stringify([root, target])
  const existing = instances.get(key)
  if (existing) return existing
  const path = join(root, 'projects.json')
  let pending: Promise<unknown> = Promise.resolve()
  const serialized = <T>(action: () => Promise<T>): Promise<T> => {
    const result = pending.then(action, action)
    pending = result.then(() => undefined, () => undefined)
    return result
  }
  const load = async (): Promise<ProjectCatalog> => {
    try { return catalogSchema.parse(JSON.parse(await readFile(path, 'utf8'))) }
    catch (error) {
      if (error instanceof Error && 'code' in error && error.code === 'ENOENT') return { version: 1, projects: [] }
      throw error
    }
  }
  const save = async (catalog: ProjectCatalog): Promise<void> => {
    await mkdir(root, { recursive: true, mode: 0o700 })
    const temporary = join(root, `projects.${randomUUID()}.pending`)
    const file = await open(temporary, 'wx', 0o600)
    try {
      try { await file.writeFile(JSON.stringify(catalogSchema.parse(catalog), null, 2) + '\n'); await file.sync() }
      finally { await file.close() }
      await rename(temporary, path)
    } catch (error) {
      await unlink(temporary).catch((_cleanupError: unknown) => { /* Preserve the original publication failure. */ })
      throw error
    }
  }
  const registry: ProjectRegistry = {
    list: () => serialized(async () => structuredClone((await load()).projects)),
    projectForWorkspace: workspaceId => serialized(async () => (await load()).projects.find(project =>
      project.bindings.some(binding => binding.targetId === target && binding.workspaceId === workspaceId))?.projectId),
    getOrRegister: registration => serialized(async () => {
      const catalog = await load()
      const old = catalog.projects.find(project => project.bindings.some(binding =>
        binding.targetId === target && binding.workspaceId === registration.workspaceId))
      if (old) {
        if (registration.projectId && registration.projectId !== old.projectId) throw new Error('The workspace is already bound to another project.')
        return old.projectId
      }
      let project = registration.projectId ? catalog.projects.find(item => item.projectId === registration.projectId) : undefined
      if (registration.projectId && !project) throw new Error('The selected public project does not exist.')
      if (!project) {
        const id = projectId.parse(registration.workspaceId)
        if (catalog.projects.some(item => item.projectId === id)) throw new Error('A project with this identity already exists on another target; select it explicitly.')
        project = { projectId: id, title: registration.title, activeTargetId: target, bindings: [] }
        catalog.projects.push(project)
      }
      project.bindings.push({ targetId: target, workspaceId: registration.workspaceId, path: registration.path })
      project.activeTargetId = target
      await save(catalog)
      return project.projectId
    }),
    selectTarget: (id, selected) => serialized(async () => {
      const catalog = await load()
      const project = catalog.projects.find(item => item.projectId === id)
      if (!project) throw new Error('The selected public project does not exist.')
      project.activeTargetId = targetId.parse(selected)
      await save(catalog)
    }),
  }
  instances.set(key, registry)
  return registry
}
