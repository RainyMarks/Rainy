/** Projects (workspaces) registered in this Host and the directories mounted into each. */
import { randomUUID } from 'node:crypto'
import { realpath, stat } from 'node:fs/promises'
import { basename, isAbsolute, normalize } from 'node:path'
import { z } from 'zod'
import { brandString } from '../shared/brand.ts'
import type { IdeRootId, IdeWorkspace, IdeWorkspaceRoot, WorkspaceId } from '../shared/ide-files-protocol.ts'
import { readJson, SerialQueue, writeJson } from './files.ts'
import { ideContains, IdeOperationError } from './ide/files-core.ts'

/** Stable identity of each project's primary directory. */
export const primaryRootId = brandString<IdeRootId>('primary')
/** Directories per project, including the primary directory. */
export const MAX_ROOTS = 16

/** One registered project. */
export interface Project {
  id: WorkspaceId
  /** Canonical primary directory; chats started in this project use it as cwd. */
  path: string
  title: string
  /** Secondary directories in attachment order. */
  attached: { rootId: IdeRootId; path: string; title: string }[]
  createdAt: number
  openedAt: number
}

const projectSchema = z.object({
  id: z.string().min(1).transform(value => brandString<WorkspaceId>(value)),
  path: z.string().min(1),
  title: z.string(),
  attached: z.array(z.object({ rootId: z.string().min(1).transform(value => brandString<IdeRootId>(value)), path: z.string().min(1), title: z.string() })),
  createdAt: z.number(),
  openedAt: z.number(),
})
const fileSchema = z.object({ version: z.literal(1), projects: z.array(projectSchema) })

/**
 * Compare two directory paths the way the platform's filesystem does.
 * @param left First path.
 * @param right Second path.
 * @returns Whether they name the same directory spelling.
 */
export function samePath(left: string, right: string): boolean {
  return process.platform === 'win32' ? normalize(left).toLowerCase() === normalize(right).toLowerCase() : left === right
}

async function canonicalDirectory(path: string): Promise<string> {
  if (!isAbsolute(path) || path.includes('\0')) throw new IdeOperationError('invalid-path', 'Choose an absolute directory path.')
  try {
    const canonical = await realpath(path)
    if (!(await stat(canonical)).isDirectory()) throw new IdeOperationError('not-directory', 'Choose an existing directory.')
    return canonical
  } catch (error) {
    if (error instanceof IdeOperationError) throw error
    throw new IdeOperationError('not-directory', 'Choose an existing directory.')
  }
}

/** The project catalog stored in `<home>/projects.json`. */
export class Projects {
  private projects: Project[] = []
  private readonly queue = new SerialQueue()
  private readonly listeners = new Set<(project: Project) => void>()

  /** @param file Path of `projects.json`. */
  constructor(private readonly file: string) {}

  /** Load the catalog; a missing file starts empty. */
  async load(): Promise<void> {
    const raw = await readJson(this.file)
    this.projects = raw === undefined ? [] : fileSchema.parse(raw).projects
  }

  /**
   * Observe committed changes.
   * @param listener Receives each project after it changes.
   * @returns A function that removes the listener.
   */
  onChange(listener: (project: Project) => void): () => void {
    this.listeners.add(listener)
    return () => { this.listeners.delete(listener) }
  }

  /** @returns Projects, most recently opened first. */
  list(): Project[] {
    return [...this.projects].sort((left, right) => right.openedAt - left.openedAt)
  }

  /** @param id Project id. @returns The project, if registered. */
  get(id: WorkspaceId): Project | undefined {
    return this.projects.find(project => project.id === id)
  }

  /**
   * Find the project whose primary directory is `cwd`.
   * @param cwd Directory to look up.
   * @returns The project, if any.
   */
  byPath(cwd: string): Project | undefined {
    return this.projects.find(project => samePath(project.path, cwd))
  }

  /**
   * Find the project that contains `path` in any of its roots.
   * @param path Absolute path.
   * @returns The innermost matching project, if any.
   */
  containing(path: string): Project | undefined {
    return this.projects
      .filter(project => roots(project).some(root => ideContains(root.path, path)))
      .sort((left, right) => right.path.length - left.path.length)[0]
  }

  /**
   * Register a directory as a project, or return the project that already has it as primary directory.
   * @param path Absolute directory.
   * @param projectId Identity to use for a new project (target switches keep the carrier's id).
   * @returns The project, marked as most recently opened.
   */
  open(path: string, projectId?: WorkspaceId): Promise<Project> {
    return this.queue.run(async () => {
      const canonical = await canonicalDirectory(path)
      const now = Date.now()
      let project = this.byPath(canonical)
      if (project === undefined) {
        project = { id: projectId ?? brandString<WorkspaceId>(randomUUID()), path: canonical, title: basename(canonical) || canonical, attached: [], createdAt: now, openedAt: now }
        this.projects.push(project)
      } else project.openedAt = now
      await this.save(project)
      return project
    })
  }

  /**
   * Mount another directory into a project. A directory already mounted keeps its root.
   * @param id Project id.
   * @param path Absolute directory.
   * @param imported Root identity and title to keep when the carrier moves a project between targets.
   * @returns The updated project.
   */
  attach(id: WorkspaceId, path: string, imported?: { rootId: IdeRootId; title: string }): Promise<Project> {
    return this.queue.run(async () => {
      const project = this.require(id)
      const canonical = await canonicalDirectory(path)
      const all = roots(project)
      if (imported?.rootId === primaryRootId) throw new IdeOperationError('invalid-request', 'The primary directory cannot be imported as an attachment.')
      const existing = all.find(root => samePath(root.path, canonical))
      if (existing !== undefined) {
        if (imported !== undefined && imported.rootId !== existing.rootId) throw new IdeOperationError('invalid-request', 'The imported directory already has another root identity.')
        return project
      }
      if (all.some(root => ideContains(root.path, canonical) || ideContains(canonical, root.path))) {
        throw new IdeOperationError('root-overlap', 'The directory overlaps an existing project folder.')
      }
      if (all.length >= MAX_ROOTS) throw new IdeOperationError('too-large', 'The project has reached its directory limit.')
      project.attached.push({ rootId: imported?.rootId ?? brandString<IdeRootId>(randomUUID()), path: canonical, title: imported?.title ?? (basename(canonical) || canonical) })
      await this.save(project)
      return project
    })
  }

  /**
   * Unmount a secondary directory. Files and chats are untouched.
   * @param id Project id.
   * @param rootId Secondary root.
   * @returns The updated project.
   */
  detach(id: WorkspaceId, rootId: IdeRootId): Promise<Project> {
    return this.queue.run(async () => {
      const project = this.require(id)
      if (rootId === primaryRootId) throw new IdeOperationError('invalid-request', 'The primary project directory cannot be removed.')
      const next = project.attached.filter(root => root.rootId !== rootId)
      if (next.length !== project.attached.length) {
        project.attached = next
        await this.save(project)
      }
      return project
    })
  }

  /**
   * Rename a project's display title.
   * @param id Project id.
   * @param title New title.
   * @returns The updated project.
   */
  rename(id: WorkspaceId, title: string): Promise<Project> {
    return this.queue.run(async () => {
      const project = this.require(id)
      const trimmed = title.trim()
      if (trimmed === '' || trimmed.length > 200) throw new IdeOperationError('invalid-request', 'Project titles must be 1–200 characters.')
      project.title = trimmed
      await this.save(project)
      return project
    })
  }

  /**
   * Remove a project from the catalog. Its directories and chats stay on disk.
   * @param id Project id.
   */
  remove(id: WorkspaceId): Promise<void> {
    return this.queue.run(async () => {
      this.projects = this.projects.filter(project => project.id !== id)
      await writeJson(this.file, { version: 1, projects: this.projects })
    })
  }

  /**
   * Resolve one mounted directory immediately before a file or process operation.
   * @param id Project id.
   * @param rootId Root id; omitted means the primary directory.
   * @returns The root, after checking it still resolves to the same directory.
   */
  async resolveRoot(id: WorkspaceId, rootId: IdeRootId = primaryRootId): Promise<IdeWorkspaceRoot> {
    const root = roots(this.require(id)).find(item => item.rootId === rootId)
    if (root === undefined) throw new IdeOperationError('workspace-not-found', 'This directory is no longer attached to the project.')
    try {
      const canonical = await realpath(root.path)
      if (!samePath(canonical, root.path) || !(await stat(canonical)).isDirectory()) {
        throw new IdeOperationError('workspace-unavailable', 'The project directory is unavailable or now resolves to another location.')
      }
      return root
    } catch (error) {
      if (error instanceof IdeOperationError) throw error
      throw new IdeOperationError('workspace-unavailable', 'The project directory is unavailable.')
    }
  }

  private require(id: WorkspaceId): Project {
    const project = this.get(id)
    if (project === undefined) throw new IdeOperationError('workspace-not-found', 'The selected project is no longer registered.')
    return project
  }

  private async save(changed: Project): Promise<void> {
    await writeJson(this.file, { version: 1, projects: this.projects })
    for (const listener of this.listeners) listener(changed)
  }
}

/**
 * All mounted directories of a project.
 * @param project Project record.
 * @returns The primary root followed by attached roots.
 */
export function roots(project: Project): IdeWorkspaceRoot[] {
  return [
    { rootId: primaryRootId, path: project.path, title: project.title, primary: true },
    ...project.attached.map(root => ({ ...root, primary: false })),
  ]
}

/**
 * The IDE's view of a project.
 * @param project Project record.
 * @returns The wire shape used by `workspaces.*` operations.
 */
export function workspaceView(project: Project): IdeWorkspace {
  return { workspaceId: project.id, path: project.path, title: project.title, roots: roots(project) }
}
