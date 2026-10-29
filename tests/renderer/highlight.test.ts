/** Grammar choice for code blocks, and grammars that load together for embedded code. */
import { describe, expect, it, vi } from 'vitest'
import { grammarFor, highlightLines, loadGrammar } from '../../src/renderer/ui/highlight.ts'

describe('grammar choice', () => {
  it('knows the aliases grammars register themselves before they load', () => {
    expect(['xsl', 'XHTML', 'ipython', 'h++', 'jsp', 'mkd', 'ps', 'plist'].map(grammarFor))
      .toEqual(['xml', 'xml', 'python', 'cpp', 'java', 'markdown', 'powershell', 'xml'])
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
