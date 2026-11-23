/** Static single-source-of-truth check comparing `native/crates/protocol/src/*.rs` against `src/shared/*.ts`
 *  so shared preferences, RPC models, IDE/execution structs and map structures cannot drift silently. */
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { readFile } from 'node:fs/promises'
import { resolve } from 'node:path'

const ROOT = resolve(import.meta.dirname, '..', '..')

/** Convert Rust `snake_case` field names to serde's `camelCase`. */
function toCamelCase(name) {
  return name.replace(/^r#/u, '').replace(/_([a-z0-9])/gu, (_whole, ch) => ch.toUpperCase())
}

/** Remove `//` and `/* ... *\/` comments while keeping line structure readable. */
function stripComments(source) {
  return source
    .replace(/\/\*[\s\S]*?\*\//gu, '')
    .replace(/\/\/[^\n]*/gu, '')
}

/** Extract the brace-delimited body starting at `openIndex` (`source[openIndex] === '{'`). */
function readBraceBlock(source, openIndex) {
  let depth = 0
  let inString = false
  let quote = ''
  for (let i = openIndex; i < source.length; i++) {
    const ch = source[i]
    if (inString) {
      if (ch === '\\') { i++; continue }
      if (ch === quote) inString = false
      continue
    }
    if (ch === '"' || ch === "'") {
      inString = true
      quote = ch
      continue
    }
    if (ch === '{') depth++
    else if (ch === '}') {
      depth--
      if (depth === 0) return { body: source.slice(openIndex + 1, i), end: i + 1 }
    }
  }
  throw new Error(`Unclosed brace block at offset ${openIndex}`)
}

/**
 * Parse all `pub struct <Name> { ... }` definitions from a Rust protocol file.
 * Resolves `#[serde(rename_all = "camelCase")]`, `#[serde(rename = "...")]`, `#[serde(skip)]`,
 * and expands non-map `#[serde(flatten)]` structs declared in the same file.
 */
function parseRustStructs(rawSource) {
  const source = stripComments(rawSource)
  const rawStructs = new Map()
  const headerRe = /((?:#\[[^\]]*\]\s*)*)pub\s+struct\s+([A-Za-z_][A-Za-z0-9_]*)\s*(?:<[^>]*>)?\s*\{/gu
  for (const match of source.matchAll(headerRe)) {
    const attrs = match[1] ?? ''
    const name = match[2]
    const openBrace = match.index + match[0].length - 1
    const { body } = readBraceBlock(source, openBrace)
    const camelCase = /rename_all\s*=\s*"camelCase"/u.test(attrs)
    const fields = []
    const flattenTypes = []
    let pendingAttrs = ''
    for (const rawLine of body.split('\n')) {
      const line = rawLine.trim()
      if (line === '') continue
      if (line.startsWith('#[')) {
        pendingAttrs += ` ${line}`
        continue
      }
      const fieldMatch = /^(?:pub(?:\([^)]*\))?\s+)?(r#[A-Za-z_][A-Za-z0-9_]*|[A-Za-z_][A-Za-z0-9_]*)\s*:\s*(.+?)\s*,?$/u.exec(line)
      if (fieldMatch === null) {
        pendingAttrs = ''
        continue
      }
      const rawName = fieldMatch[1]
      const rawType = fieldMatch[2].trim()
      const fieldAttrs = pendingAttrs
      pendingAttrs = ''
      if (/\bserde\([^)]*\bskip\b/u.test(fieldAttrs)) continue
      if (/\bserde\([^)]*\bflatten\b/u.test(fieldAttrs)) {
        // Ignore catch-all `rest: Map<String, Value>` maps; expand nested typed structs such as `MapTour`.
        if (!/\bMap\s*</u.test(rawType)) flattenTypes.push(rawType.replace(/^crate::[a-z0-9_:]+::/u, ''))
        continue
      }
      const rename = /\bserde\([^)]*\brename\s*=\s*"([^"]+)"/u.exec(fieldAttrs)?.[1]
      const jsonName = rename ?? (camelCase ? toCamelCase(rawName) : rawName.replace(/^r#/u, ''))
      fields.push(jsonName)
    }
    rawStructs.set(name, { fields, flattenTypes })
  }

  const resolved = new Map()
  for (const [name, entry] of rawStructs) {
    const set = new Set(entry.fields)
    for (const flat of entry.flattenTypes) {
      for (const nested of rawStructs.get(flat)?.fields ?? []) set.add(nested)
    }
    resolved.set(name, [...set].sort())
  }
  return resolved
}

/**
 * Parse `#[serde(tag = "type")] pub enum <Name>` variant wire names from a Rust file.
 */
function parseRustTaggedEnumVariants(rawSource, enumName) {
  const source = stripComments(rawSource)
  const re = new RegExp(`pub\\s+enum\\s+${enumName}\\s*\\{`, 'u')
  const match = re.exec(source)
  assert.ok(match, `Rust enum ${enumName} not found`)
  const { body } = readBraceBlock(source, match.index + match[0].length - 1)
  const variants = []
  let depth = 0
  let pendingAttrs = ''
  for (const rawLine of body.split('\n')) {
    const line = rawLine.trim()
    if (line === '') continue
    if (depth === 0) {
      if (line.startsWith('#[')) {
        pendingAttrs += ` ${line}`
      } else {
        const variantMatch = /^([A-Za-z_][A-Za-z0-9_]*)\b/u.exec(line)
        if (variantMatch !== null) {
          const rename = /\bserde\([^)]*\brename\s*=\s*"([^"]+)"/u.exec(pendingAttrs)?.[1]
          variants.push(rename ?? variantMatch[1])
          pendingAttrs = ''
        }
      }
    }
    for (const ch of line) {
      if (ch === '{') depth++
      else if (ch === '}') depth--
    }
  }
  return variants.sort()
}

/**
 * Parse top-level property names of every `export interface <Name> { ... }` in a TypeScript file.
 */
function parseTsInterfaces(rawSource) {
  const source = stripComments(rawSource)
  const interfaces = new Map()
  const headerRe = /export\s+interface\s+([A-Za-z_][A-Za-z0-9_]*)(?:\s*<[^>]*>)?(?:\s+extends\s+[^{]+)?\s*\{/gu
  for (const match of source.matchAll(headerRe)) {
    const name = match[1]
    const openBrace = match.index + match[0].length - 1
    const { body } = readBraceBlock(source, openBrace)
    const fields = new Set()
    let braceDepth = 0
    let parenDepth = 0
    let bracketDepth = 0
    let token = ''
    const flush = () => {
      const segment = token.trim()
      token = ''
      if (segment === '') return
      const prop = /^(?:readonly\s+)?(?:'([^']+)'|"([^"]+)"|([A-Za-z_$][A-Za-z0-9_$]*))\s*\??\s*:/u.exec(segment)
      if (prop !== null) fields.add(prop[1] ?? prop[2] ?? prop[3])
    }
    for (let i = 0; i < body.length; i++) {
      const ch = body[i]
      if (ch === '{') { braceDepth++; token += ch; continue }
      if (ch === '}') { braceDepth--; token += ch; continue }
      if (ch === '(') { parenDepth++; token += ch; continue }
      if (ch === ')') { parenDepth--; token += ch; continue }
      if (ch === '[') { bracketDepth++; token += ch; continue }
      if (ch === ']') { bracketDepth--; token += ch; continue }
      if (braceDepth === 0 && parenDepth === 0 && bracketDepth === 0 && (ch === ';' || ch === '\n')) {
        flush()
      } else {
        token += ch
      }
    }
    flush()
    interfaces.set(name, [...fields].sort())
  }
  return interfaces
}

async function readRepo(relPath) {
  return readFile(resolve(ROOT, relPath), 'utf8')
}

test('Rust protocol structs and TypeScript shared interfaces stay in sync', async () => {
  const [prefsRs, chatRs, ideRs, framesRs, streamRs, mapRs] = await Promise.all([
    readRepo('native/crates/protocol/src/prefs.rs'),
    readRepo('native/crates/protocol/src/chat.rs'),
    readRepo('native/crates/protocol/src/ide.rs'),
    readRepo('native/crates/protocol/src/frames.rs'),
    readRepo('native/crates/protocol/src/stream.rs'),
    readRepo('native/crates/protocol/src/map.rs'),
  ])
  const [rpcTs, ideFilesTs, mapTs, streamPatchTs] = await Promise.all([
    readRepo('src/shared/rpc.ts'),
    readRepo('src/shared/ide-files-protocol.ts'),
    readRepo('src/shared/map-protocol.ts'),
    readRepo('src/shared/stream-patch.ts'),
  ])

  const rust = {
    prefs: parseRustStructs(prefsRs),
    chat: parseRustStructs(chatRs),
    ide: parseRustStructs(ideRs),
    frames: parseRustStructs(framesRs),
    stream: parseRustStructs(streamRs),
    map: parseRustStructs(mapRs),
  }
  const ts = {
    rpc: parseTsInterfaces(rpcTs),
    ideFiles: parseTsInterfaces(ideFilesTs),
    map: parseTsInterfaces(mapTs),
    streamPatch: parseTsInterfaces(streamPatchTs),
  }

  /** Assert exact bidirectional field equality between a Rust struct and a TypeScript interface. */
  const expectExact = (label, rustMap, rustName, tsMap, tsName, { ignoreTs = [] } = {}) => {
    const rustFields = rustMap.get(rustName)
    const tsFields = tsMap.get(tsName)
    assert.ok(rustFields, `${label}: Rust struct ${rustName} not found`)
    assert.ok(tsFields, `${label}: TypeScript interface ${tsName} not found`)
    const filteredTs = tsFields.filter(field => !ignoreTs.includes(field))
    assert.deepEqual(
      filteredTs,
      rustFields,
      `${label}: field drift between Rust ${rustName} and TypeScript ${tsName}`,
    )
  }

  // Preferences (includes `notify`)
  expectExact('UiPreferences', rust.prefs, 'UiPreferences', ts.rpc, 'UiPreferences')
  assert.ok(ts.rpc.get('UiPreferences').includes('notify'), 'UiPreferences must include notify')

  // Chat & Agent models
  expectExact('Usage', rust.chat, 'Usage', ts.rpc, 'Usage')
  expectExact('AssistantMessage', rust.chat, 'AssistantMessage', ts.rpc, 'AssistantMessage', { ignoreTs: ['role'] })
  expectExact('ChatChange', rust.chat, 'ChatChange', ts.rpc, 'ChatChange')
  expectExact('PendingApproval', rust.chat, 'PendingApproval', ts.rpc, 'PendingApproval')
  expectExact('ContextUsage', rust.chat, 'ContextUsage', ts.rpc, 'ContextUsage')
  expectExact('ContextBreakdownSegments', rust.chat, 'ContextBreakdownSegments', ts.rpc, 'ContextBreakdownSegments')
  expectExact('ContextBreakdown', rust.chat, 'ContextBreakdown', ts.rpc, 'ContextBreakdown')

  // RPC frames & streaming patches
  expectExact('RpcError', rust.frames, 'RpcError', ts.rpc, 'RpcErrorBody')
  expectExact('StreamPatch', rust.stream, 'StreamPatch', ts.streamPatch, 'StreamPatch')

  // IDE files, execution & one-shot Office PDF registration
  expectExact('WorkspaceRoot', rust.ide, 'WorkspaceRoot', ts.ideFiles, 'IdeWorkspaceRoot')
  expectExact('IdeWorkspace', rust.ide, 'IdeWorkspace', ts.ideFiles, 'IdeWorkspace')
  expectExact('FileEntry', rust.ide, 'FileEntry', ts.ideFiles, 'IdeFileEntry')
  expectExact('IdeDirectory', rust.ide, 'IdeDirectory', ts.ideFiles, 'IdeDirectory')
  expectExact('OfficePdfRegistration', rust.ide, 'OfficePdfRegistration', ts.ideFiles, 'OfficePdfRegistration')
  expectExact('IdeOk', rust.ide, 'IdeOk', ts.rpc, 'IdeOk')
  expectExact('ProcessExit', rust.ide, 'ProcessExit', ts.rpc, 'ProcessExit')
  expectExact('TerminalInfo', rust.ide, 'TerminalInfo', ts.rpc, 'TerminalInfo')
  expectExact('RunSpec', rust.ide, 'RunSpec', ts.rpc, 'RunSpec')
  expectExact('RunInfo', rust.ide, 'RunInfo', ts.rpc, 'RunInfo')
  expectExact('DebugCapabilities', rust.ide, 'DebugCapabilities', ts.rpc, 'DebugCapabilities')
  expectExact('DebugBreakpoint', rust.ide, 'DebugBreakpoint', ts.rpc, 'DebugBreakpoint')
  expectExact('DebugInfo', rust.ide, 'DebugInfo', ts.rpc, 'DebugInfo')
  expectExact('ExecutionStatus', rust.ide, 'ExecutionStatus', ts.rpc, 'ExecutionStatus')
  expectExact('ExecutionEvent', rust.ide, 'ExecutionEvent', ts.rpc, 'ExecutionEvent')
  expectExact('ExecutionPoll', rust.ide, 'ExecutionPoll', ts.rpc, 'ExecutionPoll')
  assert.ok(ts.rpc.get('ExecutionPoll').includes('truncated'), 'ExecutionPoll must include truncated')

  // Project map shared structures
  expectExact('MapFile', rust.map, 'MapFile', ts.map, 'MapFile')
  expectExact('MapPackage', rust.map, 'MapPackage', ts.map, 'MapPackage')
  expectExact('StackItem', rust.map, 'StackItem', ts.map, 'MapStackItem')
  expectExact('MapPlace', rust.map, 'MapPlace', ts.map, 'TourPlace')
  expectExact('MapFlow', rust.map, 'MapFlow', ts.map, 'TourFlow')
  expectExact('MapTask', rust.map, 'MapTask', ts.map, 'TourTask')
  expectExact('MapExplanation', rust.map, 'MapExplanation', ts.map, 'MapExplanation')
  expectExact('ProjectMap', rust.map, 'ProjectMap', ts.map, 'ProjectMap')
  expectExact('FootprintStep', rust.map, 'FootprintStep', ts.map, 'FootprintStep')
  expectExact('FootprintTurn', rust.map, 'FootprintTurn', ts.map, 'FootprintTurn')

  // AssistantBlock enum tags match TypeScript content block union
  const rustBlocks = parseRustTaggedEnumVariants(chatRs, 'AssistantBlock')
  const tsBlocks = ['TextContent', 'ThinkingContent', 'ToolCall'].map((iface) => {
    const m = new RegExp(`export\\s+interface\\s+${iface}\\s*\\{[^}]*type:\\s*'([^']+)'`, 'u').exec(rpcTs)
    assert.ok(m, `TS interface ${iface} missing literal type tag`)
    return m[1]
  }).sort()
  assert.deepEqual(tsBlocks, rustBlocks, 'AssistantBlock variants drifted between Rust and TypeScript')
})

test('HostMethods["ide"] does not degrade to unknown or any', async () => {
  const rpcTs = stripComments(await readRepo('src/shared/rpc.ts'))
  const match = /['"]ide['"]\s*:\s*\[\s*([^,\]]+)\s*,\s*([^\]]+)\s*\]/u.exec(rpcTs)
  assert.ok(match, 'HostMethods must define the "ide" method signature')
  const returnType = match[2].trim()
  assert.notEqual(returnType, 'unknown', 'HostMethods["ide"] return type must be strongly typed, not unknown')
  assert.notEqual(returnType, 'any', 'HostMethods["ide"] return type must not be any')
})
