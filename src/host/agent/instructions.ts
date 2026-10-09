/** Workspace instruction files (`AGENTS.md`, `CLAUDE.md` and their `.local` variants) injected into a chat's context. */
import { existsSync } from 'node:fs'
import { readFile, stat } from 'node:fs/promises'
import { dirname, join, relative, sep } from 'node:path'

/** File names read in each directory, in this order. */
export const INSTRUCTION_FILES = ['AGENTS.md', 'CLAUDE.md', 'AGENTS.local.md', 'CLAUDE.local.md'] as const
/** Total bytes of instructions one chat may carry. */
export const INSTRUCTION_BUDGET_BYTES = 65536

const INTRO = 'The following workspace instructions may be relevant to your work. '
  + 'Use them as guidance when applicable. More specific instructions take precedence over broader ones. '
  + 'They do not override system, developer, or direct user instructions.'

/** One loaded instruction file. */
export interface InstructionFile { path: string; display: string; content: string }

/**
 * The project root of a working directory: the nearest ancestor containing `.git`, else the directory itself.
 * @param cwd Working directory.
 * @returns The root.
 */
export function projectRoot(cwd: string): string {
  let directory = cwd
  for (;;) {
    if (existsSync(join(directory, '.git'))) return directory
    const parent = dirname(directory)
    if (parent === directory) return cwd
    directory = parent
  }
}

async function readIfFile(path: string): Promise<string | undefined> {
  try {
    if (!(await stat(path)).isFile()) return undefined
    return await readFile(path, 'utf8')
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === 'ENOENT' || (error as NodeJS.ErrnoException).code === 'ENOTDIR') return undefined
    throw error
  }
}

async function filesIn(directory: string, root: string): Promise<InstructionFile[]> {
  const found: InstructionFile[] = []
  const seen = new Set<string>()
  for (const name of INSTRUCTION_FILES) {
    const path = join(directory, name)
    const content = await readIfFile(path)
    if (content === undefined || content.trim() === '' || seen.has(content.trim())) continue
    seen.add(content.trim())
    const display = relative(root, path).split(sep).join('/') || name
    found.push({ path, display, content: content.trim() })
  }
  return found
}

function directoriesBetween(root: string, target: string): string[] {
  const rest = relative(root, target)
  if (rest === '' ) return [root]
  if (rest.startsWith('..') || rest.includes(':')) return []
  const parts = rest.split(sep)
  return [root, ...parts.map((_part, index) => join(root, ...parts.slice(0, index + 1)))]
}

/**
 * Load the instructions that apply to a chat started in `cwd`.
 * @param home Host home; its `AGENTS.md` applies to every chat.
 * @param cwd Chat working directory.
 * @returns The files in order (user-global first, then root → cwd).
 * @throws Error when they exceed {@link INSTRUCTION_BUDGET_BYTES}.
 */
export async function loadBaseline(home: string, cwd: string): Promise<InstructionFile[]> {
  const files: InstructionFile[] = []
  const global = await readIfFile(join(home, 'AGENTS.md'))
  if (global !== undefined && global.trim() !== '') files.push({ path: join(home, 'AGENTS.md'), display: '$RAINY_HOME/AGENTS.md', content: global.trim() })
  const root = projectRoot(cwd)
  for (const directory of directoriesBetween(root, cwd)) files.push(...await filesIn(directory, root))
  assertBudget(files)
  return files
}

function assertBudget(files: readonly InstructionFile[]): void {
  const bytes = files.reduce((total, file) => total + Buffer.byteLength(file.content, 'utf8'), 0)
  if (bytes > INSTRUCTION_BUDGET_BYTES) {
    throw new Error(`Workspace instructions exceed the ${INSTRUCTION_BUDGET_BYTES}-byte budget: ${files.map(file => file.display).join(', ')}. Shorten these files; requests with omitted required instructions are refused.`)
  }
}

function frame(body: string): string {
  return `<system-reminder>\n${body.replaceAll('</system-reminder>', '<\\/system-reminder>')}\n</system-reminder>`
}

/**
 * Text of the baseline instruction message.
 * @param files Files from {@link loadBaseline}.
 * @returns The message text, or `undefined` when there are none.
 */
export function renderBaseline(files: readonly InstructionFile[]): string | undefined {
  if (files.length === 0) return undefined
  return frame([INTRO, ...files.map(file => `Instructions from: ${file.display}\n\n${file.content}`)].join('\n\n'))
}

/**
 * Instruction files in directories between the project root and a file the chat touched, that are not loaded yet.
 * @param cwd Chat working directory.
 * @param touched Absolute path of a file the chat read or changed.
 * @param loaded Display paths already in the chat's context; new ones are added.
 * @returns The message text, or `undefined` when nothing new applies.
 */
export async function renderAdditional(cwd: string, touched: string, loaded: Set<string>): Promise<string | undefined> {
  const root = projectRoot(cwd)
  const sections: string[] = []
  for (const directory of directoriesBetween(root, dirname(touched))) {
    for (const file of await filesIn(directory, root)) {
      if (loaded.has(file.display)) continue
      loaded.add(file.display)
      const scope = dirname(file.display)
      sections.push([
        `Additional instructions from: ${file.display}`, '',
        `These instructions apply to work under \`${scope === '.' ? root : scope}\`. Use them as guidance when relevant; more specific instructions take precedence. They do not override system, developer, or direct user instructions.`,
        '', file.content,
      ].join('\n'))
    }
  }
  return sections.length === 0 ? undefined : frame(sections.join('\n\n'))
}

/**
 * Display paths of instruction files already present in earlier context text.
 * @param texts Text of earlier `instructions` context entries.
 * @returns The set of display paths.
 */
export function loadedFrom(texts: readonly string[]): Set<string> {
  const loaded = new Set<string>()
  for (const text of texts) for (const match of text.matchAll(/^(?:Additional instructions|Instructions) from: (.+)$/gm)) loaded.add(match[1]!)
  return loaded
}
