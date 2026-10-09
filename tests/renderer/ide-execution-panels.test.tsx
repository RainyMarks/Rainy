// @vitest-environment happy-dom
/** Debug console contexts and saved CMake preset choices in the run configuration dialog. */
import { afterEach, describe, expect, it, vi } from 'vitest'
import { brandString } from '../../src/shared/brand.ts'
import type { IdeDebugId, IdeExecutionLanguage, IdeRunConfiguration } from '../../src/shared/ide-execution-protocol.ts'
import type { WorkspaceId } from '../../src/shared/ide-files-protocol.ts'
import type { IdeExecutionApi } from '../../src/renderer/ide/ide-execution-api.ts'
import type { IdeFilesApi } from '../../src/renderer/ide/ide-api.ts'
import { IdeBottom } from '../../src/renderer/ide/IdeBottom.tsx'
import { RunConfigurationDialog } from '../../src/renderer/ide/RunConfigurationDialog.tsx'
import { IdeExecutionModel } from '../../src/renderer/ide/ide-execution-model.ts'
import { IdeModel } from '../../src/renderer/ide/ide-model.ts'
import { allByText, byRole, change, cleanup, click, nameOf, render, waitFor } from './ide-dom.tsx'

vi.mock('../../src/renderer/rpc.ts', () => import('./ide-host-mock.ts'))
vi.mock('../../src/renderer/ide/editor-loader.ts', () => ({
  loadEditorAssets: async () => ({ terminal: () => ({ write: () => {}, reset: () => {}, fit: () => {},
    focus: () => {}, setAppearance: () => {}, dispose: () => {} }) }),
}))

const owners: { model: IdeModel; execution: IdeExecutionModel }[] = []
afterEach(async () => {
  cleanup()
  for (const owner of owners.splice(0)) { owner.model.dispose(); await owner.execution.dispose() }
  vi.restoreAllMocks()
})

function fixture(profiles: readonly IdeRunConfiguration[] = []) {
  const model = new IdeModel({ request: vi.fn<IdeFilesApi['request']>() as IdeFilesApi['request'] }, {
    debounceMs: 60_000, pollMs: 60_000, restoreSession: vi.fn(async () => {}),
  })
  const workspaceId = brandString<WorkspaceId>('execution-controls')
  const current = model.state.getSnapshot()
  model.state.set({ ...current, phase: 'ready', workspace: { workspaceId, path: '/project', title: 'Project' },
    data: { ...current.data, activePath: 'main.cpp', layout: { ...current.data.layout, bottomTab: 'debug' },
      execution: { profiles, activeProfile: profiles[0]?.name ?? null, breakpoints: [], watches: [] } } })
  const execution = new IdeExecutionModel({ request: vi.fn<IdeExecutionApi['request']>() as IdeExecutionApi['request'] }, {
    pollMs: 60_000, activePollMs: 60_000, maxOutputCharacters: 100_000, maxRetainedWorkspaces: 8,
    terminalCols: 80, terminalRows: 24,
    getConfiguration: () => model.state.getSnapshot().data.execution ?? { profiles: [], activeProfile: null, breakpoints: [], watches: [] },
    setConfiguration: (configuration) => { model.execution(configuration) }, onError: (error) => { model.fail(error) }, onReveal: vi.fn(),
  })
  owners.push({ model, execution })
  return { model, execution, workspaceId }
}

describe('debug console adapter contexts', () => {
  it.each<readonly [IdeExecutionLanguage, 'watch' | 'repl']>([
    ['c', 'watch'], ['cpp', 'watch'], ['python', 'repl'], ['javascript', 'repl'], ['typescript', 'repl'],
  ])('evaluates %s console expressions using %s', async (language, context) => {
    const { model, execution, workspaceId } = fixture()
    const id = brandString<IdeDebugId>('94a11954-e943-4c11-8c1f-589872cb89ef')
    const snapshot = { ...execution.state.getSnapshot(), workspaceId, selected: id, debugId: id,
      status: { runs: [], terminals: [], debugSessions: [{ id, workspaceId, language, name: 'debug', phase: 'paused' as const, breakpoints: [] }] } }
    const evaluate = vi.spyOn(execution, 'evaluate').mockResolvedValue({ result: '42', variablesReference: 0 })
    render(<IdeBottom state={model.state.getSnapshot()} executionState={snapshot} execution={execution} model={model}
      reveal={vi.fn()} appearance={{ dark: false, fontSize: 13 }} />)
    change(byRole('textbox', '调试控制台'), 'value + 1')
    click(byRole('button', '求值'))
    await waitFor(() => { expect(evaluate).toHaveBeenCalledExactlyOnceWith('value + 1', context) })
    await waitFor(() => { expect(allByText('42')).toHaveLength(1) })
  })
})

const cmakeProfile: IdeRunConfiguration = { name: 'CMake debug', language: 'cpp', program: 'main.cpp', build: {
  kind: 'cmake', buildDirectory: 'build/debug', target: 'main', executable: 'main', configurePreset: 'debug', buildPreset: 'build-debug',
} }

describe('CMake run configuration fields', () => {
  it('clears the Python module when the user changes the profile to a compiled language', () => {
    const { model } = fixture([{ name: 'module', language: 'python', program: 'main.py', pythonModule: 'package.main' }])
    render(<RunConfigurationDialog open close={vi.fn()} state={model.state.getSnapshot()} model={model} />)
    click(byRole('button', '语言'))
    click(byRole('menuitem', 'C++'))
    change(byRole('textbox', '程序路径'), 'main.cpp')
    click(byRole('button', '保存'))
    const saved = model.state.getSnapshot().data.execution?.profiles[0]
    expect(saved?.language).toBe('cpp')
    expect(saved?.program).toBe('main.cpp')
    expect(saved?.pythonModule).toBeUndefined()
  })

  it('displays and preserves saved configure and build presets when saving the dialog', () => {
    const { model } = fixture([cmakeProfile])
    const close = vi.fn()
    render(<RunConfigurationDialog open close={close} state={model.state.getSnapshot()} model={model} />)
    const configure = byRole('textbox', 'CMake 配置预设（可选）') as HTMLInputElement
    const build = byRole('textbox', 'CMake 构建预设（可选）') as HTMLInputElement
    expect(`${nameOf(configure)}: ${configure.value}\n${nameOf(build)}: ${build.value}`)
      .toBe('CMake 配置预设（可选）: debug\nCMake 构建预设（可选）: build-debug')
    click(byRole('button', '保存'))
    expect(model.state.getSnapshot().data.execution?.profiles[0]?.build).toEqual(cmakeProfile.build)
    expect(close).toHaveBeenCalledOnce()
  })

  it('loads another profile without carrying over its presets and saves newly entered names', () => {
    const other: IdeRunConfiguration = { ...cmakeProfile, name: 'CMake other', build: { kind: 'cmake', buildDirectory: 'build/other', target: 'other', executable: 'other' } }
    const { model } = fixture([cmakeProfile, other])
    render(<RunConfigurationDialog open close={vi.fn()} state={model.state.getSnapshot()} model={model} />)
    click(byRole('button', '运行配置'))
    click(byRole('menuitem', other.name))
    const configure = byRole('textbox', 'CMake 配置预设（可选）') as HTMLInputElement
    const build = byRole('textbox', 'CMake 构建预设（可选）') as HTMLInputElement
    expect(configure.value).toBe('')
    expect(build.value).toBe('')
    change(configure, 'release')
    change(build, 'build-release')
    click(byRole('button', '保存'))
    expect(model.state.getSnapshot().data.execution?.profiles.find(profile => profile.name === other.name)?.build).toEqual({
      kind: 'cmake', buildDirectory: 'build/other', target: 'other', executable: 'other', configurePreset: 'release', buildPreset: 'build-release',
    })
  })

  it('omits optional presets after the user clears both fields', () => {
    const { model } = fixture([cmakeProfile])
    render(<RunConfigurationDialog open close={vi.fn()} state={model.state.getSnapshot()} model={model} />)
    change(byRole('textbox', 'CMake 配置预设（可选）'), '')
    change(byRole('textbox', 'CMake 构建预设（可选）'), '')
    click(byRole('button', '保存'))
    expect(model.state.getSnapshot().data.execution?.profiles[0]?.build).toEqual({ kind: 'cmake', buildDirectory: 'build/debug', target: 'main', executable: 'main' })
  })

  it('refuses malformed argument JSON and keeps the dialog open', () => {
    const { model } = fixture([cmakeProfile])
    const close = vi.fn()
    render(<RunConfigurationDialog open close={close} state={model.state.getSnapshot()} model={model} />)
    change(byRole('textbox', '参数（JSON 数组）'), '{"not": "an array"}')
    click(byRole('button', '保存'))
    expect(byRole('alert').textContent).toBe('请检查配置中的 JSON 数组、环境变量和必填路径。')
    expect(close).not.toHaveBeenCalled()
    expect(model.state.getSnapshot().data.execution?.profiles).toEqual([cmakeProfile])
  })
})
