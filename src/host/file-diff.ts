/** Bounded, read-only Git changes for an explicitly opened project file. */
import { execFile } from 'node:child_process'
import { promisify } from 'node:util'
import { resolve, relative, isAbsolute, sep } from 'node:path'
import { realpath } from 'node:fs/promises'

/** Compare a project file without invoking configured external diff drivers or text converters. */
export async function fileDiff(cwd: string, path: string): Promise<string> {
  const root = await realpath(cwd)
  const target = await realpath(resolve(root, path))
  const scoped = relative(root, target)
  if (isAbsolute(scoped) || scoped === '..' || scoped.startsWith('..' + sep)) throw new Error('只显示当前项目内文件的差异。')
  const run = (args: string[]) => promisify(execFile)('git', ['-C', root, ...args], { encoding: 'utf8', timeout: 10000, maxBuffer: 512 * 1024, windowsHide: true })
  await run(['rev-parse', '--show-toplevel'])
  let tracked = true
  try { await run(['ls-files', '--error-unmatch', '--', scoped]) } catch (error) {
    if (error === null || typeof error !== 'object' || !('code' in error) || error.code !== 1) throw error
    tracked = false
  }
  const args = tracked ? ['diff', '--no-ext-diff', '--no-textconv', '--', scoped]
    : ['diff', '--no-index', '--no-ext-diff', '--no-textconv', '--', '/dev/null', target]
  try { return (await run(args)).stdout } catch (error) {
    if (error !== null && typeof error === 'object' && 'code' in error && error.code === 1 && 'stdout' in error && typeof error.stdout === 'string') return error.stdout
    throw new Error('无法读取差异：请检查 Git 仓库，或在终端查看超出 512 KiB 的 diff。')
  }
}
