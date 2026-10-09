/** Editor language admission keeps unsaved documents bound to their selected project directory. */
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { pathToFileURL } from 'node:url'
import { afterEach, beforeEach, describe, expect, it } from 'vitest'
import { brandString } from '../../src/shared/brand.ts'
import type { WorkspaceId } from '../../src/shared/ide-files-protocol.ts'
import { prepareIdeLanguageMessage, type IdeLanguageSpec } from '../../src/host/ide/language.ts'

let directory: string
let spec: IdeLanguageSpec
beforeEach(async () => {
  directory = await mkdtemp(join(tmpdir(), 'rainy-language-'))
  spec = { workspaceId: brandString<WorkspaceId>('language-test'), root: directory, language: 'python', argv: ['fixture-language-server'] }
})
afterEach(async () => {
  await rm(directory, { recursive: true, force: true })
})

describe('editor language message admission', () => {
  it('binds initialization to the project directory rather than a client process or directory', async () => {
    const result = await prepareIdeLanguageMessage({ jsonrpc: '2.0', id: 1, method: 'initialize', params: {
      processId: 1234, rootUri: 'file:///wrong', workspaceFolders: [{ uri: 'file:///wrong', name: 'wrong' }], capabilities: {},
    } }, spec)
    expect(result.params).toMatchObject({ processId: null, rootPath: directory, rootUri: pathToFileURL(directory).href,
      workspaceFolders: [{ uri: pathToFileURL(directory).href }] })
    expect(result.params).not.toHaveProperty('initializationOptions')
  })

  it('offers the bundled tsserver to the TypeScript server without replacing an editor choice', async () => {
    const typescript = { ...spec, language: 'typescript' as const, tsserver: '/bundled/typescript/lib/tsserver.js' }
    const initialize = (initializationOptions?: unknown) => ({ jsonrpc: '2.0', id: 1, method: 'initialize',
      params: { capabilities: {}, ...initializationOptions === undefined ? {} : { initializationOptions } } })
    expect((await prepareIdeLanguageMessage(initialize(), typescript)).params)
      .toMatchObject({ initializationOptions: { tsserver: { fallbackPath: '/bundled/typescript/lib/tsserver.js' } } })
    expect((await prepareIdeLanguageMessage(initialize({ preferences: { quoteStyle: 'single' }, tsserver: { fallbackPath: '/editor/tsserver.js' } }), typescript)).params)
      .toMatchObject({ initializationOptions: { preferences: { quoteStyle: 'single' }, tsserver: { fallbackPath: '/editor/tsserver.js' } } })
  })

  it('retains unsaved text and document versions without reading disk content', async () => {
    const path = join(directory, 'sample.py')
    await writeFile(path, 'saved = 1\n')
    const message = { jsonrpc: '2.0', method: 'textDocument/didChange', params: {
      textDocument: { uri: pathToFileURL(path).href, version: 3 }, contentChanges: [{ text: 'unsaved = 2\n' }],
    } }
    await expect(prepareIdeLanguageMessage(message, spec)).resolves.toEqual(message)
  })

  it('admits a new document under a real project directory', async () => {
    await mkdir(join(directory, 'src'))
    const message = { jsonrpc: '2.0', method: 'textDocument/didOpen', params: {
      textDocument: { uri: pathToFileURL(join(directory, 'src', 'new.py')).href, languageId: 'python', version: 1, text: '' },
    } }
    await expect(prepareIdeLanguageMessage(message, spec)).resolves.toEqual(message)
  })

  it('rejects an outside document before forwarding it to the language process', async () => {
    await expect(prepareIdeLanguageMessage({ jsonrpc: '2.0', id: 2, method: 'textDocument/hover', params: {
      textDocument: { uri: pathToFileURL(join(directory, '..', 'other.py')).href }, position: { line: 0, character: 0 },
    } }, spec)).rejects.toThrow('outside its workspace')
    await expect(prepareIdeLanguageMessage({ jsonrpc: '2.0', id: 3, method: 'textDocument/hover', params: {
      textDocument: { uri: 'https://example.invalid/a.py' },
    } }, spec)).rejects.toThrow('file URIs')
  })

  it('requires a separate connection for another project and rejects malformed messages', async () => {
    await expect(prepareIdeLanguageMessage({ jsonrpc: '2.0', method: 'workspace/didChangeWorkspaceFolders', params: {} }, spec))
      .rejects.toThrow('another workspace language connection')
    await expect(prepareIdeLanguageMessage({ jsonrpc: '1.0', id: 1 }, spec)).rejects.toThrow('Invalid editor language message')
  })

  it('accepts responses to server requests without inventing document fields', async () => {
    const response = { jsonrpc: '2.0', id: 'configuration-1', result: [{ pythonPath: '/usr/bin/python3' }] }
    await expect(prepareIdeLanguageMessage(response, spec)).resolves.toEqual(response)
  })
})
