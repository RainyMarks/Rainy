// @vitest-environment happy-dom
/** The in-app directory browser: two-column landing, typed paths, hidden folders and folder creation. */
import { afterEach, describe, expect, it, vi } from 'vitest'
import type { IdeDirectoryListing } from '../../src/shared/ide-files-protocol.ts'
import { DirectoryBrowser, pathCrumbs } from '../../src/renderer/app/DirectoryBrowser.tsx'
import { allByRole, byRole, change, cleanup, click, keyDown, nameOf, render, waitFor } from './ide-dom.tsx'

vi.mock('../../src/renderer/rpc.ts', () => import('./ide-host-mock.ts'))

afterEach(cleanup)

const entry = (path: string, hidden = false) => ({ name: path.split('/').at(-1) ?? path, path, hidden })
const initialTree: Readonly<Record<string, IdeDirectoryListing>> = {
  '/home': { path: '/home', parent: '/', roots: ['/', '/home/user'], entries: [entry('/home/user')] },
  '/home/user': { path: '/home/user', parent: '/home', roots: ['/', '/home/user'],
    entries: [entry('/home/user/.config', true), entry('/home/user/ctf'), entry('/home/user/notes')] },
  '/home/user/ctf': { path: '/home/user/ctf', parent: '/home/user', roots: ['/', '/home/user'], entries: [entry('/home/user/ctf/pwn')] },
  '/home/user/ctf/pwn': { path: '/home/user/ctf/pwn', parent: '/home/user/ctf', roots: ['/', '/home/user'], entries: [] },
  '/': { path: '/', parent: null, roots: ['/', '/home/user'], entries: [entry('/home'), entry('/tmp')] },
}

function fixture() {
  const tree: Record<string, IdeDirectoryListing> = { ...initialTree }
  const listDirectory = vi.fn(async (path: string | undefined): Promise<IdeDirectoryListing> => {
    const listing = tree[path ?? '/home/user']
    if (listing === undefined) throw new Error(`The directory does not exist: ${path ?? ''}`)
    return listing
  })
  const createDirectory = vi.fn(async (path: string): Promise<IdeDirectoryListing> => {
    const parent = tree['/home/user']
    if (parent !== undefined) tree['/home/user'] = { ...parent, entries: [...parent.entries, entry(path)] }
    return { path, parent: '/home/user', roots: [], entries: [] }
  })
  const onOpen = vi.fn()
  const onClose = vi.fn()
  render(<DirectoryBrowser open busy={false} onOpen={onOpen} onClose={onClose} listDirectory={listDirectory} createDirectory={createDirectory} />)
  return { listDirectory, createDirectory, onOpen, onClose }
}

const rows = (): string[] => allByRole('button').filter(button => button.closest('[role="listitem"]') !== null).map(nameOf)
const open = (): Promise<HTMLElement> => waitFor(() => {
  const button = byRole('button', '打开')
  expect(button.matches(':disabled')).toBe(false)
  return button
})

describe('directory browser', () => {
  it('lands on the home directory beside its parent and opens it', async () => {
    const h = fixture()
    click(await open())
    expect(h.listDirectory.mock.calls.map(([path]) => path)).toEqual([undefined, '/home'])
    expect(rows()).toEqual(['user', 'ctf', 'notes'])
    expect(allByRole('button').filter(button => button.parentElement?.className.includes('crumbSeat') ?? false).map(nameOf))
      .toEqual(['/', 'home', 'user'])
    expect(h.onOpen).toHaveBeenCalledExactlyOnceWith('/home/user')
  })

  it('selects a folder, steps into its children and opens the selection', async () => {
    const h = fixture()
    await open()
    click(byRole('button', 'ctf'))
    await waitFor(() => { expect(rows()).toEqual(['ctf', 'notes', 'pwn']) })
    click(byRole('button', 'pwn'))
    await waitFor(() => { expect(rows()).toEqual(['pwn']) })
    click(await open())
    expect(h.onOpen).toHaveBeenCalledExactlyOnceWith('/home/user/ctf/pwn')
  })

  it('reveals hidden folders only on request', async () => {
    fixture()
    await open()
    expect(rows()).toEqual(['user', 'ctf', 'notes'])
    click(byRole('button', '显示隐藏文件'))
    expect(rows()).toEqual(['user', '.config', 'ctf', 'notes'])
  })

  it('navigates to a typed path, filters by the typed name and reports unreadable paths', async () => {
    const h = fixture()
    await open()
    click(byRole('button', '编辑路径'))
    const input = byRole('textbox', '编辑路径') as HTMLInputElement
    expect(input.value).toBe('/home/user/')
    change(input, '/home/user/no')
    expect(rows()).toEqual(['user', 'notes'])
    change(input, '/missing')
    keyDown(input, { key: 'Enter' })
    await waitFor(() => { expect(byRole('alert').textContent).toBe('The directory does not exist: /missing') })
    change(input, '/')
    keyDown(input, { key: 'Enter' })
    await waitFor(() => { expect(rows()).toEqual(['home', 'tmp']) })
    click(await open())
    expect(h.onOpen).toHaveBeenCalledExactlyOnceWith('/')
  })

  it('creates a folder in the selected folder and selects it', async () => {
    const h = fixture()
    await open()
    click(byRole('button', '新建文件夹'))
    expect(byRole('dialog', '新建文件夹').textContent).toContain('在“user”中新建文件夹')
    change(byRole('textbox', '文件夹名称'), 'new-task')
    click(byRole('button', '创建'))
    await waitFor(() => { expect(h.createDirectory).toHaveBeenCalledExactlyOnceWith('/home/user/new-task') })
    await waitFor(() => {
      expect(allByRole('button').filter(button => button.getAttribute('aria-current') === 'true').map(nameOf)).toEqual(['new-task'])
    })
    click(await open())
    expect(h.onOpen).toHaveBeenCalledExactlyOnceWith('/home/user/new-task')
  })

  it('cancels without a choice', async () => {
    const h = fixture()
    await open()
    click(byRole('button', '取消'))
    expect(h.onClose).toHaveBeenCalledOnce()
    expect(h.onOpen).not.toHaveBeenCalled()
  })
})

describe('breadcrumbs', () => {
  it('splits POSIX, drive and UNC paths into openable ancestors', () => {
    expect(pathCrumbs('/home/user')).toEqual([{ name: '/', path: '/' }, { name: 'home', path: '/home' }, { name: 'user', path: '/home/user' }])
    expect(pathCrumbs('C:\\Users\\me')).toEqual([{ name: 'C:\\', path: 'C:\\' }, { name: 'Users', path: 'C:\\Users' }, { name: 'me', path: 'C:\\Users\\me' }])
    expect(pathCrumbs('\\\\server\\share\\ctf')).toEqual([{ name: '\\\\server\\share\\', path: '\\\\server\\share\\' },
      { name: 'ctf', path: '\\\\server\\share\\ctf' }])
  })
})
