/** Project map colors: code by language, the other kinds by kind, and the agent's marks told apart. */
import { readFileSync } from 'node:fs'
import { describe, expect, it } from 'vitest'
import type { MapFileKind } from '../../src/shared/map-protocol.ts'
import { fileColor } from '../../src/renderer/map/colors.ts'

const styles = readFileSync(new URL('../../src/renderer/map/ProjectMap.module.css', import.meta.url), 'utf8')
const variable = (name: string): string | undefined => new RegExp(`${name}: ([^;]+);`, 'u').exec(styles)?.[1]

describe('fileColor', () => {
  it('colors code by language and tests as tests, whatever their language', () => {
    expect(fileColor('python', 'code')).toBe('var(--map-py)')
    expect(fileColor('python', 'test')).toBe('var(--map-test)')
    expect(fileColor('typescript', 'test')).toBe('var(--map-test)')
    expect(fileColor('cobol', 'code')).toBe('var(--map-code)')
  })

  it('names only colors the stylesheet defines', () => {
    const kinds: MapFileKind[] = ['code', 'test', 'doc', 'config', 'asset', 'other']
    const languages = ['typescript', 'javascript', 'python', 'cpp', 'go', 'rust', 'java', 'ruby', 'shell', 'html', 'cobol']
    for (const kind of kinds) {
      for (const language of languages) {
        const name = /^var\((--map-[a-z]+)\)$/u.exec(fileColor(language, kind))?.[1]
        expect(name).toBeDefined()
        expect(variable(name!)).toBeDefined()
      }
    }
  })
})

it('draws a running write and a change waiting for review in different colors', () => {
  expect(variable('--map-writing')).toBeDefined()
  expect(variable('--map-changed')).not.toBe(variable('--map-writing'))
})
