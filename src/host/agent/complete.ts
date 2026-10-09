/** `@` completion: files and folders under a working directory, and other chats. */
import { readdir } from 'node:fs/promises'
import { join, relative, sep } from 'node:path'
import type { CompletionItem } from '../../shared/rpc.ts'
import type { StoredChat } from './store.ts'

const MAX_RESULTS = 20
const MAX_ENTRIES = 50_000
/** Index entries are reused for this long. */
const INDEX_TTL_MS = 10_000
const EXCLUDED = new Set(['.git', 'node_modules', 'dist', 'build', 'out', 'coverage', 'target', '.next', '.nuxt', '.turbo', '.venv', '__pycache__', '.pytest_cache', '.mypy_cache', '.gradle'])

interface Indexed { path: string; directory: boolean }
const indexes = new Map<string, { at: number; entries: Promise<Indexed[]> }>()

async function index(root: string): Promise<Indexed[]> {
  const cached = indexes.get(root)
  if (cached !== undefined && Date.now() - cached.at < INDEX_TTL_MS) return cached.entries
  const entries = (async () => {
    const found: Indexed[] = []
    const queue = [root]
    while (queue.length > 0 && found.length < MAX_ENTRIES) {
      const directory = queue.shift()!
      let children
      try { children = await readdir(directory, { withFileTypes: true }) } catch (_error) { continue }
      for (const child of children) {
        if (found.length >= MAX_ENTRIES) break
        const path = join(directory, child.name)
        const rel = relative(root, path).split(sep).join('/')
        if (child.isDirectory()) {
          if (EXCLUDED.has(child.name)) continue
          found.push({ path: rel, directory: true })
          queue.push(path)
        } else if (child.isFile()) found.push({ path: rel, directory: false })
      }
    }
    return found
  })()
  indexes.set(root, { at: Date.now(), entries })
  return entries
}

function score(path: string, query: string): number {
  const lower = path.toLowerCase()
  const q = query.toLowerCase()
  const name = lower.slice(lower.lastIndexOf('/') + 1)
  if (name === q) return 0
  if (name.startsWith(q)) return 1
  if (name.includes(q)) return 2
  if (lower.includes(q)) return 3
  let at = 0
  for (const char of q) {
    at = lower.indexOf(char, at)
    if (at < 0) return -1
    at++
  }
  return 4
}

function quote(path: string): string {
  return /\s/.test(path) ? `"${path}"` : path
}

/**
 * Files and folders matching a query. A query containing `/` lists that folder; otherwise the whole tree is ranked.
 * @param root Working directory.
 * @param query Text after `@`.
 * @returns At most 20 completions.
 */
export async function completeFiles(root: string, query: string): Promise<CompletionItem[]> {
  const normalized = query.replaceAll('\\', '/').replace(/^"/, '')
  const all = await index(root)
  const slash = normalized.lastIndexOf('/')
  let matches: Indexed[]
  if (normalized === '' || slash >= 0) {
    const directory = slash < 0 ? '' : normalized.slice(0, slash + 1)
    const fragment = (slash < 0 ? normalized : normalized.slice(slash + 1)).toLowerCase()
    matches = all.filter((entry) => {
      if (!entry.path.startsWith(directory)) return false
      const rest = entry.path.slice(directory.length)
      return rest !== '' && !rest.includes('/') && rest.toLowerCase().startsWith(fragment)
    }).sort((left, right) => Number(right.directory) - Number(left.directory) || left.path.localeCompare(right.path))
  } else {
    matches = all.map(entry => ({ entry, rank: score(entry.path, normalized) }))
      .filter(item => item.rank >= 0)
      .sort((left, right) => left.rank - right.rank || left.entry.path.length - right.entry.path.length || left.entry.path.localeCompare(right.entry.path))
      .map(item => item.entry)
  }
  return matches.slice(0, MAX_RESULTS).map(entry => ({
    kind: entry.directory ? 'directory' : 'file',
    insert: quote(entry.directory ? `${entry.path}/` : entry.path),
    label: entry.path.slice(entry.path.lastIndexOf('/') + 1) + (entry.directory ? '/' : ''),
    detail: entry.path,
  }))
}

/**
 * Other chats matching a query, inserted as `@[title](rainy-session:<id>)`.
 * @param chats Chats to search.
 * @param query Text after `@`.
 * @param exclude The chat being written in.
 * @returns At most 5 completions.
 */
export function completeChats(chats: readonly StoredChat[], query: string, exclude: string | undefined): CompletionItem[] {
  const q = query.toLowerCase()
  return chats.filter(chat => chat.id !== exclude && chat.title !== '' && (q === '' || chat.title.toLowerCase().includes(q)))
    .slice(0, 5)
    .map(chat => ({ kind: 'session', insert: `[${chat.title.replace(/[\]\n]/g, ' ')}](rainy-session:${chat.id})`, label: chat.title, detail: new Date(chat.updatedAt).toLocaleString() }))
}
