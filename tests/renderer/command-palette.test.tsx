// @vitest-environment happy-dom
/** The command palette: matching typed text, running a command from the keyboard, and recently run commands first. */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { CommandPalette } from '../../src/renderer/app/CommandPalette.tsx'
import { matchScore, rankCommands, rememberCommand, type PaletteCommand } from '../../src/renderer/app/commands.ts'
import { all, cleanup, flush, render, type, waitFor } from './settings-harness.tsx'

vi.mock('../../src/renderer/rpc.ts', async () => (await import('./settings-harness.tsx')).rpcModule)

function commands(): PaletteCommand[] {
  return [
    { id: 'view.palette', category: '视图', label: '显示所有命令', keys: ['Ctrl', '+', 'Shift', '+', 'P'], run: vi.fn() },
    { id: 'edit.searchFiles', category: '编辑', label: '在文件中搜索', keys: ['Ctrl', '+', 'Shift', '+', 'F'], run: vi.fn() },
    { id: 'edit.replaceFiles', category: '编辑', label: '在文件中替换', run: vi.fn() },
    { id: 'git.commit', category: 'Git', label: 'Commit changes', run: vi.fn() },
    { id: 'run.debug', category: '运行', label: '调试当前文件', disabled: true, run: vi.fn() },
  ]
}

beforeEach(() => { localStorage.clear() })
afterEach(async () => { await cleanup() })

describe('matching', () => {
  it('needs every typed character in order, and prefers whole words and consecutive letters', () => {
    expect(matchScore('', 'anything')).toBe(0)
    expect(matchScore('xyz', 'Commit changes')).toBeUndefined()
    expect(matchScore('commit', 'git: commit changes')!).toBeGreaterThan(matchScore('cmch', 'git: commit changes')!)
    expect(matchScore('搜索', '编辑: 在文件中搜索')).toBeDefined()
    const ranked = rankCommands(commands(), 'cc', [])
    expect(ranked[0]!.id).toBe('git.commit')
  })

  it('puts recently run commands first when nothing is typed', () => {
    rememberCommand('edit.replaceFiles')
    rememberCommand('git.commit')
    expect(rankCommands(commands(), '', ['git.commit', 'edit.replaceFiles']).map(command => command.id).slice(0, 2)).toEqual(['git.commit', 'edit.replaceFiles'])
  })
})

it('filters as you type and runs the chosen command, skipping disabled ones', async () => {
  const list = commands()
  const onClose = vi.fn()
  await render(<CommandPalette query="" commands={list} onClose={onClose} />)
  const input = document.querySelector<HTMLInputElement>('[data-palette-input]')!
  await type(input, '文件中')
  expect(all('[data-command]').map(option => option.getAttribute('data-command'))).toEqual(['edit.searchFiles', 'edit.replaceFiles'])
  expect(all('[data-command="edit.searchFiles"] kbd').map(key => key.textContent)).toEqual(['Ctrl', '+', 'Shift', '+', 'F'])
  input.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowDown', bubbles: true }))
  await flush()
  input.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }))
  await flush()
  expect(onClose).toHaveBeenCalled()
  await waitFor(() => { expect(list[2]!.run).toHaveBeenCalled() })
  expect(JSON.parse(localStorage.getItem('rainy.palette.recent')!)).toEqual(['edit.replaceFiles'])

  await type(input, '调试')
  input.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }))
  await new Promise(resolve => setTimeout(resolve, 50))
  expect(list[4]!.run).not.toHaveBeenCalled()
})
