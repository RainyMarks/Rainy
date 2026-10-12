/** Grammar choice for code blocks and files, and grammars that load together for embedded code. */
import { describe, expect, it, vi } from 'vitest'
import { grammarFor, highlightLines, languageForPath, loadGrammar } from '../../src/renderer/ui/highlight.ts'

describe('grammar choice', () => {
  it('knows the aliases grammars register themselves before they load', () => {
    expect(['xsl', 'XHTML', 'ipython', 'h++', 'jsp', 'mkd', 'ps'].map(grammarFor)).toEqual(['xml', 'xml', 'python', 'cpp', 'java', 'markdown', 'powershell'])
    expect(languageForPath('C:\\ctf\\Info.plist')).toBe('xml')
    expect(grammarFor('brainfuck')).toBeUndefined()
  })

  it('loads the script and style grammars with HTML', async () => {
    await loadGrammar('xml')
    await vi.waitFor(() => {
      const spans = highlightLines('<script>let answer = 42</script>', 'html')?.flat() ?? []
      expect(spans.find(span => span.text === 'let')?.className).toContain('hljs-keyword')
    })
  })
})
