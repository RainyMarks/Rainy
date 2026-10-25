/** How the rich Markdown editor writes tables back. */
import { describe, expect, it } from 'vitest'
import { conventionalTables } from '../../src/renderer/ide/viewers/markdown-merge.ts'
import { preserveSource } from '../../src/renderer/ide/viewers/markdown-merge.ts'

describe('conventionalTables', () => {
  it('writes delimiter rows as --- and keeps alignment colons', () => {
    expect(conventionalTables('| a | b | c |\n| - | :- | -: |\n| 1 | 2 | 3 |\n')).toBe('| a | b | c |\n| --- | :--- | ---: |\n| 1 | 2 | 3 |\n')
    expect(conventionalTables('| x |\n|:-:|\n')).toBe('| x |\n| :---: |\n')
  })

  it('leaves code, horizontal rules and ordinary text alone', () => {
    const text = '```\n| - | - |\n```\n\n---\n\n- item\n\n~~~md\n| - |\n~~~\n'
    expect(conventionalTables(text)).toBe(text)
  })
})

describe('preserveSource', () => {
  const source = '# Title\n\n[![b](https://x.io/a?b=1&c=2)](https://x.io)\n\n|a|b|\n|---|---|\n|1|2|\n\nPath ~/x and text.\n\nLast line.\n'
  // What a Markdown serializer makes of `source`: escapes, spaced tables, an extra blank line.
  const loaded = '# Title\n\n[![b](https://x.io/a?b=1\&c=2)](https://x.io)\n\n| a | b |\n| --- | --- |\n| 1 | 2 |\n\nPath \~/x and text.\n\n\nLast line.\n'

  it('returns the file unchanged when nothing was edited', () => {
    expect(preserveSource(source, loaded, loaded)).toBe(source)
  })

  it('takes only the edited lines from the editor', () => {
    const edited = loaded.replace('Last line.', 'Last line, edited.')
    expect(preserveSource(source, loaded, edited)).toBe(source.replace('Last line.', 'Last line, edited.'))
  })

  it('adds and removes whole paragraphs without touching the rest', () => {
    const added = loaded.replace('# Title\n', '# Title\n\nNew paragraph.\n')
    expect(preserveSource(source, loaded, added)).toBe(source.replace('# Title\n', '# Title\n\nNew paragraph.\n'))
    const removed = loaded.replace('Path \~/x and text.\n\n', '')
    expect(preserveSource(source, loaded, removed)).toBe('# Title\n\n[![b](https://x.io/a?b=1&c=2)](https://x.io)\n\n|a|b|\n|---|---|\n|1|2|\n\n\nLast line.\n')
  })

  it('writes an edited table in the editor form and keeps the lines around it', () => {
    const edited = loaded.replace('| 1 | 2 |', '| 1 | 3 |')
    const merged = preserveSource(source, loaded, edited)
    expect(merged).toContain('[![b](https://x.io/a?b=1&c=2)](https://x.io)')
    expect(merged).toContain('| 1 | 3 |')
    expect(merged).toContain('Path ~/x and text.')
  })
})
