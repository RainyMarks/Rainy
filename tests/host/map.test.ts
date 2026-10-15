/** Project map: file kinds, imports across languages, the stack, the architecture boxes, and the AI explanation. */
import { mkdir, readFile, writeFile } from 'node:fs/promises'
import { dirname, join } from 'node:path'
import { afterEach, beforeEach, describe, expect, it } from 'vitest'
import { lineChangeCounts } from '../../src/shared/line-diff.ts'
import { mapView, nodeForPath, outsideId } from '../../src/shared/map-graph.ts'
import type { MapFile, ProjectMap } from '../../src/shared/map-protocol.ts'
import type { WorkspaceId } from '../../src/shared/ide-files-protocol.ts'
import { fileType } from '../../src/host/map/languages.ts'
import { declaredDependencies } from '../../src/host/map/stack.ts'
import { scanProject, vendoredPatterns } from '../../src/host/map/scan.ts'
import { FakeOpenAI, harness, tempHost } from './agent-fixtures.ts'

async function project(files: Record<string, string>): Promise<{ root: string; cleanup(): Promise<void> }> {
  const { dir, cleanup } = await tempHost()
  const root = join(dir, 'project')
  for (const [path, text] of Object.entries(files)) {
    await mkdir(dirname(join(root, path)), { recursive: true })
    await writeFile(join(root, path), text)
  }
  return { root, cleanup }
}

function importsOf(map: Pick<ProjectMap, 'files' | 'imports'>): string[] {
  const pairs: string[] = []
  for (let index = 0; index < map.imports.length; index += 2) pairs.push(`${map.files[map.imports[index]!]!.path} -> ${map.files[map.imports[index + 1]!]!.path}`)
  return pairs.sort()
}

describe('scanning', () => {
  const cleanups: (() => Promise<void>)[] = []
  afterEach(async () => { for (const cleanup of cleanups.splice(0)) await cleanup() })

  it('classifies files by name', () => {
    expect(fileType('src/app.tsx')).toMatchObject({ language: 'typescript', kind: 'code' })
    expect(fileType('tests/app.test.ts')).toMatchObject({ kind: 'test' })
    expect(fileType('pkg/util_test.go')).toMatchObject({ language: 'go', kind: 'test' })
    expect(fileType('README.md')).toMatchObject({ language: 'markdown', kind: 'doc' })
    expect(fileType('pnpm-lock.yaml')).toMatchObject({ kind: 'config', lock: true })
    expect(fileType('challenge.png')).toMatchObject({ kind: 'asset', binary: true })
    expect(fileType('Dockerfile')).toMatchObject({ language: 'docker', kind: 'config' })
    expect(fileType('requirements.txt')).toMatchObject({ kind: 'config' })
    expect(fileType('exploit.sage')).toMatchObject({ language: 'sage', kind: 'code' })
  })

  it('follows imports across languages and names external packages', async () => {
    const { root, cleanup } = await project({
      'tsconfig.json': '{ // aliases\n "compilerOptions": { "baseUrl": ".", "paths": { "@/*": ["src/*"], }, }, }',
      'package.json': JSON.stringify({ dependencies: { react: '19.0.0', zod: '4.0.0' }, devDependencies: { vite: '7.0.0' } }),
      'src/main.tsx': "import React from 'react'\nimport { App } from './app.js'\nimport '@/styles/base.css'\nimport { z } from 'zod'\nimport fs from 'node:fs'\n",
      'src/app.tsx': "export { helper } from '../lib/helper'\nconst lazy = import('./lazy')\n",
      'src/lazy.ts': 'export const x = 1\n',
      'src/styles/base.css': '@import "./tokens";\n',
      'src/styles/_tokens.scss': ':root {}\n',
      'lib/helper/index.ts': 'export const helper = 1\n',
      'solve.py': 'from pwn import *\nimport utils\nfrom crypto_tools.rsa import attack\nfrom Crypto.Util.number import long_to_bytes\nimport os, sys\n',
      'utils.py': 'from . import nothing\n',
      'crypto_tools/__init__.py': '',
      'crypto_tools/rsa.py': 'from .common import gcd\n',
      'crypto_tools/common.py': 'def gcd(a, b): return a\n',
      'requirements.txt': 'pwntools==4.12\npycryptodome>=3\n',
      'native/main.c': '#include <stdio.h>\n#include "util.h"\n',
      'native/util.h': 'int util(void);\n',
      'go.mod': 'module example.com/tool\n\ngo 1.22\n',
      'cmd/run/main.go': 'package main\n\nimport (\n  "fmt"\n  "example.com/tool/internal/core"\n  "github.com/spf13/cobra"\n)\n',
      'internal/core/core.go': 'package core\n',
      'rs/src/main.rs': 'mod net;\nuse crate::net::client::Client;\nuse serde::Serialize;\n',
      'rs/src/net/mod.rs': 'pub mod client;\n',
      'rs/src/net/client.rs': 'pub struct Client;\n',
      'java/src/com/acme/App.java': 'package com.acme;\nimport com.acme.util.Strings;\nimport org.springframework.boot.SpringApplication;\nimport java.util.List;\n',
      'java/src/com/acme/util/Strings.java': 'package com.acme.util;\n',
      'node_modules/react/index.js': 'module.exports = 1\n',
      'image.png': '\u0000PNG',
    })
    cleanups.push(cleanup)
    const map = await scanProject(root)
    expect(map.files.some(file => file.path.startsWith('node_modules/'))).toBe(false)
    expect(importsOf(map)).toEqual([
      'cmd/run/main.go -> internal/core/core.go',
      'crypto_tools/rsa.py -> crypto_tools/common.py',
      'java/src/com/acme/App.java -> java/src/com/acme/util/Strings.java',
      'native/main.c -> native/util.h',
      'rs/src/main.rs -> rs/src/net/client.rs',
      'rs/src/main.rs -> rs/src/net/mod.rs',
      'rs/src/net/mod.rs -> rs/src/net/client.rs',
      'solve.py -> crypto_tools/rsa.py',
      'solve.py -> utils.py',
      'src/app.tsx -> lib/helper/index.ts',
      'src/app.tsx -> src/lazy.ts',
      'src/main.tsx -> src/app.tsx',
      'src/main.tsx -> src/styles/base.css',
      'src/styles/base.css -> src/styles/_tokens.scss',
    ])
    const packages = Object.fromEntries(map.packages.map(item => [item.name, item.files.map(index => map.files[index]!.path)]))
    expect(packages).toMatchObject({ react: ['src/main.tsx'], zod: ['src/main.tsx'], pwn: ['solve.py'], Crypto: ['solve.py'],
      'github.com/spf13/cobra': ['cmd/run/main.go'], serde: ['rs/src/main.rs'], 'org.springframework.boot': ['java/src/com/acme/App.java'] })
    expect(packages.os).toBeUndefined()
    expect(packages.fs).toBeUndefined()
    const names = map.stack.map(item => item.name)
    expect(names).toEqual(expect.arrayContaining(['TypeScript', 'Python', 'React', 'pwntools', 'PyCryptodome', 'Zod', 'Vite', 'Cobra', 'Serde', 'Spring']))
    expect(map.stack.findIndex(item => item.kind === 'framework')).toBeGreaterThan(map.stack.findLastIndex(item => item.kind === 'language'))
    expect(map.files.find(file => file.path === 'image.png')).toMatchObject({ kind: 'asset', lines: 0 })
    expect(map.files.find(file => file.path === 'solve.py')).toMatchObject({ lines: 5, language: 'python' })
  })

  it('does not count third-party, generated or minified code as the project\'s own', async () => {
    const { root, cleanup } = await project({
      '.gitattributes': '# vendored copies\ngen/** linguist-generated\n*.pb.go linguist-generated=true\ndocs/** -linguist-documentation\n',
      'app.js': "import './vendor/lib.js'\n",
      'vendor/lib.js': 'export const lib = 1\n',
      'gen/api.ts': 'export const api = 1\n',
      'proto/user.pb.go': 'package proto\n',
      'dist.min.js': 'x',
      'bundle.js': `${'a'.repeat(20000)}\n`,
    })
    cleanups.push(cleanup)
    const map = await scanProject(root)
    const kinds = Object.fromEntries(map.files.map(file => [file.path, file.kind]))
    expect(kinds).toMatchObject({ 'app.js': 'code', 'vendor/lib.js': 'other', 'gen/api.ts': 'other', 'proto/user.pb.go': 'other', 'dist.min.js': 'other', 'bundle.js': 'other' })
    expect(importsOf(map)).toEqual(['app.js -> vendor/lib.js'])
    expect(vendoredPatterns('a/b/ linguist-vendored\n').map(pattern => pattern.test('a/b/c/d.js'))).toEqual([true])
  })

  it('reads dependencies from manifests', () => {
    expect(declaredDependencies('pyproject.toml', '[project]\ndependencies = ["fastapi>=0.1", "uvicorn[standard]"]\n')).toEqual(['fastapi', 'uvicorn'])
    expect(declaredDependencies('Cargo.toml', '[package]\nname = "x"\n[dependencies]\ntokio = "1"\nserde = { version = "1" }\n[dev-dependencies]\ninsta = "1"\n'))
      .toEqual(['tokio', 'serde', 'insta'])
    expect(declaredDependencies('go.mod', 'module m\nrequire (\n\tgithub.com/gin-gonic/gin v1.9.0\n)\n')).toEqual(['github.com/gin-gonic/gin'])
    expect(declaredDependencies('package.json', '{ broken')).toEqual([])
  })
})

function fakeMap(paths: Record<string, number>, imports: [string, string][] = []): ProjectMap {
  const files: MapFile[] = Object.entries(paths).map(([path, lines]) => ({ path, language: 'typescript', kind: /\.md$/u.test(path) ? 'doc' : 'code', lines, bytes: lines * 30 }))
  const index = (path: string): number => files.findIndex(file => file.path === path)
  return { workspaceId: 'w' as WorkspaceId, title: 'p', generatedAt: 0, truncated: false, files, imports: imports.flatMap(([from, to]) => [index(from), index(to)]), packages: [], stack: [] }
}

describe('architecture boxes', () => {
  it('splits the directories that hold most of the code and links them by their imports', () => {
    const paths: Record<string, number> = { 'README.md': 10, 'package.json': 5 }
    for (const part of ['host', 'renderer', 'shared', 'main']) for (let index = 0; index < 8; index++) paths[`src/${part}/f${index}.ts`] = part === 'renderer' ? 400 : 100
    for (let index = 0; index < 6; index++) paths[`tests/t${index}.test.ts`] = 50
    const map = fakeMap(paths, [['src/renderer/f0.ts', 'src/shared/f0.ts'], ['src/renderer/f1.ts', 'src/shared/f1.ts'], ['src/host/f0.ts', 'src/shared/f0.ts'], ['tests/t0.test.ts', 'src/host/f1.ts']])
    const view = mapView(map, '')
    expect(view.nodes.map(node => node.id)).toEqual(['src/host', 'src/main', 'src/renderer', 'src/shared', 'tests', 'README.md', 'package.json'])
    expect(view.edges).toEqual([
      { from: 'src/host', to: 'src/shared', weight: 1 }, { from: 'src/renderer', to: 'src/shared', weight: 2 }, { from: 'tests', to: 'src/host', weight: 1 },
    ])
    expect(view.nodes.find(node => node.id === 'src/renderer')).toMatchObject({ openable: true, outside: false, lines: 3200 })
    expect(nodeForPath(view, 'src/renderer/f3.ts')).toBe('src/renderer')

    // Inside one directory, the directories it imports from and is imported by show outside it.
    const inner = mapView(map, 'src/renderer')
    expect(inner.nodes.filter(node => !node.outside).map(node => node.type)).toEqual(Array(8).fill('file'))
    expect(inner.nodes.filter(node => node.outside).map(node => node.id)).toEqual(['src/shared'])
    expect(inner.edges).toEqual([{ from: 'src/renderer/f0.ts', to: 'src/shared', weight: 1 }, { from: 'src/renderer/f1.ts', to: 'src/shared', weight: 1 }])
    expect(outsideId('src/renderer', 'package.json')).toBe('package.json')
    expect(outsideId('src/renderer', 'src/shared/a/b.ts')).toBe('src/shared')
  })

  it('groups many loose files of a directory into one box', () => {
    const paths: Record<string, number> = {}
    for (let index = 0; index < 30; index++) paths[`script${index}.py`] = 20
    const view = mapView(fakeMap(paths), '')
    expect(view.nodes.map(node => [node.id, node.type])).toEqual([['*', 'loose']])
    expect(nodeForPath(view, 'script7.py')).toBe('*')
  })
})

describe('line counts', () => {
  it('counts added and removed lines', () => {
    expect(lineChangeCounts('a\nb\nc\n', 'a\nB\nc\nd\n')).toEqual({ added: 2, removed: 1 })
    expect(lineChangeCounts('', 'x\ny')).toEqual({ added: 2, removed: 0 })
    expect(lineChangeCounts('x\r\ny', 'x\ny')).toEqual({ added: 0, removed: 0 })
    expect(lineChangeCounts('1\n2\n3\n4\n5', '0\n1\n3\n5\n6')).toEqual({ added: 2, removed: 2 })
  })
})

describe('the map service', () => {
  const cleanups: (() => Promise<void>)[] = []
  let server: FakeOpenAI | undefined
  beforeEach(() => { cleanups.length = 0 })
  afterEach(async () => {
    for (const cleanup of cleanups.reverse()) await cleanup()
    await server?.stop()
    server = undefined
  })

  it('maps a project and keeps the explanation the model wrote for its parts', async () => {
    server = new FakeOpenAI([{ text: 'Here it is: {"summary": "A tiny solver.", "modules": {"solve.py": "Solves the challenge", "made-up": "ignored"}}' }])
    await server.start()
    const h = await harness(server, cleanups)
    await writeFile(join(h.project, 'solve.py'), 'import helper\nprint(helper.key)\n')
    await writeFile(join(h.project, 'helper.py'), 'key = 1\n')
    await writeFile(join(h.project, 'README.md'), '# Baby XOR\nRecover the flag.\n')
    const map = await h.call('map.get', { workspaceId: h.workspaceId })
    expect(map.files.map(file => file.path).sort()).toEqual(['README.md', 'helper.py', 'solve.py'])
    expect(importsOf(map)).toEqual(['solve.py -> helper.py'])
    expect(map.explanation).toBeUndefined()
    const explanation = await h.call('map.explain', { workspaceId: h.workspaceId })
    expect(explanation).toMatchObject({ summary: 'A tiny solver.', modules: { 'solve.py': 'Solves the challenge' }, locale: 'zh', model: 'fake/fake-model' })
    const prompt = JSON.stringify(server.requests[0])
    expect(prompt).toContain('Baby XOR')
    expect(prompt).toContain('solve.py')
    expect((await h.call('map.get', { workspaceId: h.workspaceId })).explanation).toEqual(explanation)
    expect(JSON.parse(await readFile(join(h.home, 'maps', `${encodeURIComponent(h.workspaceId)}.json`), 'utf8'))).toMatchObject({ version: 1 })
  })

  it('reports an unusable reply', async () => {
    server = new FakeOpenAI([{ text: 'I cannot help with that.' }])
    await server.start()
    const h = await harness(server, cleanups)
    await writeFile(join(h.project, 'a.py'), 'print(1)\n')
    await expect(h.call('map.explain', { workspaceId: h.workspaceId })).rejects.toThrow(/explain-failed/u)
  })
})
