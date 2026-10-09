// @vitest-environment happy-dom
/** Run target selection: extension default, remembered per-file language and pinned profiles. */
import { describe, expect, it } from 'vitest'
import { chooseFileLanguage, pinRunProfile, runTarget, selectedPythonExecutable } from '../../src/renderer/ide/run-target.ts'
import type { IdeExecutionConfiguration } from '../../src/shared/ide-execution-protocol.ts'

const script = { path: 'tools/solve.py', label: 'solve.py', program: 'tools/solve.py' }
const native = { path: 'src/main.c', label: 'main.c', program: 'src/main.c' }

describe('run target', () => {
  it('runs the open file by its extension and recognizes nothing for unknown files', () => {
    expect(runTarget(undefined, script)).toEqual({ mode: 'auto',
      configuration: { name: 'solve.py', language: 'python', program: 'tools/solve.py', terminal: true } })
    expect(runTarget(undefined, native)?.configuration.language).toBe('c')
    expect(runTarget(undefined, { path: 'notes.txt', label: 'notes.txt', program: 'notes.txt' })).toBeUndefined()
  })

  it('remembers a chosen language for one file only and returns to the extension without leaving a record', () => {
    const chosen = chooseFileLanguage(undefined, script, 'cpp')
    expect(runTarget(chosen, script)).toMatchObject({ mode: 'file', configuration: { language: 'cpp' } })
    expect(runTarget(chosen, native)).toMatchObject({ mode: 'auto', configuration: { language: 'c' } })
    const restored = chooseFileLanguage(chosen, script, 'auto')
    expect(restored.profiles).toEqual([])
    expect(runTarget(restored, script)).toMatchObject({ mode: 'auto', configuration: { language: 'python' } })
  })

  it('keeps arguments when switching languages and drops settings that belong to another language', () => {
    const saved: IdeExecutionConfiguration = { profiles: [{ name: 'solve.py', language: 'python', program: 'tools/solve.py',
      pythonModule: 'tools.solve', arguments: ['--flag'], terminal: true }], activeProfile: null, breakpoints: [], watches: [] }
    const php = chooseFileLanguage(saved, script, 'php')
    expect(php.profiles).toEqual([{ name: 'solve.py', language: 'php', program: 'tools/solve.py', arguments: ['--flag'], terminal: true }])
    expect(chooseFileLanguage(php, script, 'auto').profiles).toEqual([{ name: 'solve.py', language: 'python', program: 'tools/solve.py',
      arguments: ['--flag'], terminal: true }])
  })

  it('runs a pinned profile from any file until a file language is chosen', () => {
    const entry = chooseFileLanguage(undefined, native, 'c')
    const pinned = pinRunProfile(entry, 'main.c')
    expect(runTarget(pinned, script)).toMatchObject({ mode: 'pinned', configuration: { program: 'src/main.c' } })
    const unpinned = chooseFileLanguage(pinned, script, 'auto')
    expect(unpinned.activeProfile).toBeNull()
    expect(runTarget(unpinned, script)).toMatchObject({ mode: 'auto', configuration: { program: 'tools/solve.py' } })
  })

  it('gives a new per-file record a unique name', () => {
    const taken: IdeExecutionConfiguration = { profiles: [{ name: 'solve.py', language: 'python', program: 'other/solve.py', terminal: true }],
      activeProfile: null, breakpoints: [], watches: [] }
    expect(chooseFileLanguage(taken, script, 'python').profiles.map(profile => profile.name)).toEqual(['solve.py', 'solve.py (2)'])
  })
})

describe('language server interpreter', () => {
  it('uses the pinned Python profile interpreter, else the first Python profile naming one', () => {
    const configuration: IdeExecutionConfiguration = { profiles: [
      { name: 'default', language: 'python', program: 'main.py' },
      { name: 'venv', language: 'python', program: 'main.py', pythonModule: 'demo.main', executable: '/venv/bin/python' },
    ], activeProfile: 'venv', breakpoints: [], watches: [] }
    expect(selectedPythonExecutable(configuration)).toBe('/venv/bin/python')
    expect(selectedPythonExecutable({ ...configuration, activeProfile: 'default' })).toBeUndefined()
    expect(selectedPythonExecutable({ ...configuration, activeProfile: null })).toBe('/venv/bin/python')
    expect(selectedPythonExecutable(undefined)).toBeUndefined()
  })
})
