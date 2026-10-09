/** Skills: `SKILL.md` files the model can read when relevant; the system prompt lists only their descriptions. */
import { homedir } from 'node:os'
import { readdir, readFile, stat } from 'node:fs/promises'
import { join } from 'node:path'
import yaml from 'js-yaml'
import { estimateText } from '../../shared/budget.ts'
import type { SkillSummary } from '../../shared/rpc.ts'

/** Largest SKILL.md that is listed. */
const MAX_SKILL_BYTES = 256 * 1024
/** Bytes of a description kept in the prompt. */
const MAX_DESCRIPTION_BYTES = 512
/** Tokens all skill descriptors together may use. */
export const SKILL_PROMPT_TOKENS = 4096

function truncateBytes(text: string, max: number): string {
  const bytes = Buffer.from(text, 'utf8')
  if (bytes.length <= max) return text
  let end = max
  while (end > 0 && (bytes[end]! & 0xc0) === 0x80) end--
  return `${bytes.subarray(0, end).toString('utf8')}…`
}

/**
 * Name and description from a SKILL.md: front-matter fields, else the first heading and the first paragraph.
 * @param text File content.
 * @param fallbackName Directory name.
 * @returns Display name and description.
 */
export function describeSkill(text: string, fallbackName: string): { name: string; description: string } {
  let body = text
  let name = fallbackName
  let description = ''
  const front = /^---\r?\n([\s\S]*?)\r?\n---\r?\n?/.exec(text)
  if (front !== null) {
    body = text.slice(front[0].length)
    try {
      const meta: unknown = yaml.load(front[1]!)
      if (meta !== null && typeof meta === 'object') {
        const record = meta as Record<string, unknown>
        if (typeof record.name === 'string' && record.name.trim() !== '') name = record.name.trim()
        if (typeof record.description === 'string') description = record.description.trim()
      }
    } catch (_error) {
      // A malformed front matter leaves the body description in effect.
    }
  }
  if (description === '') {
    const paragraph = body.split(/\r?\n\s*\r?\n/).map(part => part.trim()).find(part => part !== '' && !part.startsWith('#'))
    description = paragraph?.replace(/\s+/g, ' ') ?? ''
  }
  return { name, description: truncateBytes(description, MAX_DESCRIPTION_BYTES) }
}

async function scan(root: string, scope: 'project' | 'user'): Promise<SkillSummary[]> {
  let names: string[]
  try { names = await readdir(root) } catch (error) {
    if ((error as NodeJS.ErrnoException).code === 'ENOENT' || (error as NodeJS.ErrnoException).code === 'ENOTDIR') return []
    throw error
  }
  const skills: SkillSummary[] = []
  for (const directory of names.sort()) {
    const path = join(root, directory, 'SKILL.md')
    try {
      const info = await stat(path)
      if (!info.isFile() || info.size > MAX_SKILL_BYTES) continue
      const { name, description } = describeSkill(await readFile(path, 'utf8'), directory)
      skills.push({ id: `${scope}/${directory}`, scope, name, description, path })
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code !== 'ENOENT') console.error(`[skills] ${path}: ${error instanceof Error ? error.message : String(error)}`)
    }
  }
  return skills
}

/**
 * Discover skills for a working directory.
 * @param cwd Chat working directory; project skills live in `<cwd>/.rainy/skills`.
 * @returns Project skills, then user skills from `~/.rainy-agent/skills`.
 */
export async function discoverSkills(cwd: string | undefined): Promise<SkillSummary[]> {
  return [
    ...(cwd === undefined ? [] : await scan(join(cwd, '.rainy', 'skills'), 'project')),
    ...await scan(join(homedir(), '.rainy-agent', 'skills'), 'user'),
  ]
}

/**
 * The system-prompt text listing skills, within {@link SKILL_PROMPT_TOKENS}.
 * @param skills Discovered skills.
 * @returns The section text, or an empty string.
 */
export function skillsSection(skills: readonly SkillSummary[]): string {
  const lines: string[] = []
  let tokens = 0
  for (const skill of skills) {
    const text = `Skill "${skill.id}": ${skill.description || skill.name}\nRead "${skill.path}" for the full instructions when this skill is relevant.`
    const cost = estimateText(text)
    if (tokens + cost > SKILL_PROMPT_TOKENS) break
    tokens += cost
    lines.push(text)
  }
  return lines.join('\n\n')
}
