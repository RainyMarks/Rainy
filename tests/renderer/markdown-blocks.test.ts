import { expect, it } from 'vitest'
import { markdownBlocks } from '../../src/renderer/chat/Markdown.tsx'

it('splits a reply at blank lines between blocks, giving back the text when joined', () => {
  const text = '# Title\n\nFirst paragraph\nstill first.\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n> quote\n\nLast'
  const blocks = markdownBlocks(text)
  expect(blocks).toEqual(['# Title\n', 'First paragraph\nstill first.\n', '| a | b |\n|---|---|\n| 1 | 2 |\n', '> quote\n', 'Last'])
  expect(blocks.join('\n')).toBe(text)
})

it('keeps code fences, math blocks and lists whole', () => {
  const fence = '```js\nconst a = 1\n\nconst b = 2\n```'
  const math = '$$\na = b\n\nc = d\n$$'
  const list = '1. one\n\n2. two\n\n   indented\n- next'
  expect(markdownBlocks(`${fence}\n\nafter`)).toEqual([`${fence}\n`, 'after'])
  expect(markdownBlocks(`${math}\n\nafter`)).toEqual([`${math}\n`, 'after'])
  expect(markdownBlocks(`${list}\n\nafter`)).toEqual([`${list}\n`, 'after'])
  // A fence still open while the reply streams holds everything after it.
  expect(markdownBlocks('intro\n\n```py\nx = 1\n\ny = 2')).toEqual(['intro\n', '```py\nx = 1\n\ny = 2'])
  // A longer fence is closed only by a fence at least as long.
  expect(markdownBlocks('````\n```\n\ninner\n````\n\nafter')).toEqual(['````\n```\n\ninner\n````\n', 'after'])
})

it('does not split text whose definitions apply to the whole document', () => {
  const text = 'See [the docs][1].\n\nMore text[^n].\n\n[1]: https://example.com\n[^n]: A note.'
  expect(markdownBlocks(text)).toEqual([text])
})
