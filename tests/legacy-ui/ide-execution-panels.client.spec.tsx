// @vitest-environment jsdom
/** Human execution controls preserve adapter context and saved CMake preset choices. */
import { afterEach, describe, expect, it, vi } from 'vitest'
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { brandString } from '@deepseek-ai/dsh-brand'
import type { TranslateNS } from '@deepseek-ai/dsh-client-locale/client'
import type { IdeDebugId, IdeExecutionLanguage, IdeRunConfiguration } from '../src/ide-execution-protocol.ts'
import type { WorkspaceId } from '../src/ide-files-protocol.ts'
import type { IdeExecutionApi } from '../src/client/ide-execution-api.ts'
import type { IdeFilesApi } from '../src/client/ide-api.ts'
import { IdeBottom } from '../src/client/IdeBottom.tsx'
import { RunConfigurationDialog } from '../src/client/RunConfigurationDialog.tsx'
import { IdeExecutionModel } from '../src/client/ide-execution-model.ts'
import { IdeModel } from '../src/client/ide-model.ts'
import { zh } from '../src/client/locales.ts'

vi.mock('../src/client/editor-loader.ts', () => ({
  loadEditorAssets: async () => ({ terminal: () => ({ write: () => {}, reset: () => {}, fit: () => {},
    setAppearance: () => {}, dispose: () => {} }) }),
}))
const owners: { model: IdeModel; execution: IdeExecutionModel }[] = []
afterEach(async () => {
  cleanup()
  for (const owner of owners.splice(0)) { owner.model.dispose(); await owner.execution.dispose() }
  vi.restoreAllMocks()
})
const t = ((key: keyof typeof zh) => zh[key]) as TranslateNS<'rainy'>

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
      status: { runs: [], terminals: [], debugSessions: [{
        id, workspaceId, language, name: 'debug', phase: 'paused' as const, breakpoints: [],
      }] } }
    const evaluate = vi.spyOn(execution, 'evaluate').mockResolvedValue({ result: '42', variablesReference: 0 })
    await act(async () => {
      render(<IdeBottom state={model.state.getSnapshot()} executionState={snapshot}
        execution={execution} model={model} t={t} reveal={vi.fn()} appearance={{ dark: false, fontSize: 13 }} />)
    })
    fireEvent.change(screen.getByRole('textbox', { name: zh.ideConsole }), { target: { value: 'value + 1' } })
    fireEvent.click(screen.getByRole('button', { name: zh.ideEvaluate }))
    await waitFor(() => { expect(evaluate).toHaveBeenCalledExactlyOnceWith('value + 1', context) })
    expect(await screen.findByText('42')).toBeTruthy()
  })
})

const cmakeProfile: IdeRunConfiguration = { name: 'CMake debug', language: 'cpp', program: 'main.cpp', build: {
  kind: 'cmake', buildDirectory: 'build/debug', target: 'main', executable: 'main', configurePreset: 'debug', buildPreset: 'build-debug',
} }

describe('CMake run configuration fields', () => {
  it('clears the Python module when the user changes the profile to a compiled language', () => {
    const { model } = fixture([{ name: 'module', language: 'python', program: 'main.py', pythonModule: 'package.main' }])
    render(<RunConfigurationDialog open close={vi.fn()} state={model.state.getSnapshot()} model={model} t={t} />)
    fireEvent.click(screen.getByRole('button', { name: zh.ideLanguage }))
    fireEvent.click(screen.getByRole('menuitem', { name: 'C++' }))
    fireEvent.change(screen.getByRole('textbox', { name: zh.ideProgram }), { target: { value: 'main.cpp' } })
    fireEvent.click(screen.getByRole('button', { name: zh.ideSave }))
    const saved = model.state.getSnapshot().data.execution?.profiles[0]
    expect(saved?.language).toBe('cpp')
    expect(saved?.program).toBe('main.cpp')
    expect(saved?.pythonModule).toBeUndefined()
  })

  it('displays and preserves saved configure and build presets when saving the dialog', async () => {
    const { model } = fixture([cmakeProfile])
    const close = vi.fn()
    render(<RunConfigurationDialog open close={close} state={model.state.getSnapshot()} model={model} t={t} />)
    fireEvent.click(screen.getByText(zh.ideAdvanced))
    const configure = screen.getByRole('textbox', { name: zh.ideConfigurePreset }) as HTMLInputElement
    const build = screen.getByRole('textbox', { name: zh.ideBuildPreset }) as HTMLInputElement
    expect(configure.value).toBe('debug')
    expect(build.value).toBe('build-debug')
    await expect(`${configure.labels?.[0]?.textContent}: ${configure.value}\n${build.labels?.[0]?.textContent}: ${build.value}\n`).toMatchFileSnapshot('./expected/cmake-presets.txt')
    fireEvent.click(screen.getByRole('button', { name: zh.ideSave }))
    expect(model.state.getSnapshot().data.execution?.profiles[0]?.build).toEqual(cmakeProfile.build)
    expect(close).toHaveBeenCalledOnce()
  })

  it('loads another profile without carrying over its presets and saves newly entered names', () => {
    const other: IdeRunConfiguration = { ...cmakeProfile, name: 'CMake other', build: { kind: 'cmake', buildDirectory: 'build/other', target: 'other', executable: 'other' } }
    const { model } = fixture([cmakeProfile, other])
    render(<RunConfigurationDialog open close={vi.fn()} state={model.state.getSnapshot()} model={model} t={t} />)
    fireEvent.click(screen.getByRole('button', { name: zh.ideRunProfiles }))
    fireEvent.click(screen.getByRole('menuitem', { name: other.name }))
    fireEvent.click(screen.getByText(zh.ideAdvanced))
    const configure = screen.getByRole('textbox', { name: zh.ideConfigurePreset }) as HTMLInputElement
    const build = screen.getByRole('textbox', { name: zh.ideBuildPreset }) as HTMLInputElement
    expect(configure.value).toBe('')
    expect(build.value).toBe('')
    fireEvent.change(configure, { target: { value: 'release' } })
    fireEvent.change(build, { target: { value: 'build-release' } })
    fireEvent.click(screen.getByRole('button', { name: zh.ideSave }))
    expect(model.state.getSnapshot().data.execution?.profiles.find(profile => profile.name === other.name)?.build).toEqual({
      kind: 'cmake', buildDirectory: 'build/other', target: 'other', executable: 'other', configurePreset: 'release', buildPreset: 'build-release',
    })
  })

  it('omits optional presets after the user clears both fields', () => {
    const { model } = fixture([cmakeProfile])
    render(<RunConfigurationDialog open close={vi.fn()} state={model.state.getSnapshot()} model={model} t={t} />)
    fireEvent.click(screen.getByText(zh.ideAdvanced))
    fireEvent.change(screen.getByRole('textbox', { name: zh.ideConfigurePreset }), { target: { value: '' } })
    fireEvent.change(screen.getByRole('textbox', { name: zh.ideBuildPreset }), { target: { value: '' } })
    fireEvent.click(screen.getByRole('button', { name: zh.ideSave }))
    expect(model.state.getSnapshot().data.execution?.profiles[0]?.build).toEqual({ kind: 'cmake', buildDirectory: 'build/debug', target: 'main', executable: 'main' })
  })
})
