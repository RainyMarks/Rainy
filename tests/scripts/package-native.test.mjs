/** The checks scripts/package-native.mjs makes before it stages, signs and packages a release. */
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { createPublicKey, generateKeyPairSync } from 'node:crypto'
import { carriedKeys, compareHistory, deletablePath, linkedCrates, parseHistory, payloadPaths, releaseMismatches, tarMember, uninstallList,
  versionStrings } from '../../scripts/package-native.mjs'

/** A VERSIONINFO node: wLength, wValueLength, wType, the key, and the value and children on 32-bit boundaries. */
function versionNode(key, value, children = []) {
  const text = typeof value === 'string'
  const pad = length => Buffer.alloc((4 - (length % 4)) % 4)
  let node = Buffer.concat([Buffer.alloc(6), Buffer.from(`${key}\0`, 'utf16le')])
  node = Buffer.concat([node, pad(node.length), text ? Buffer.from(`${value}\0`, 'utf16le') : value])
  for (const child of children) node = Buffer.concat([node, pad(node.length), child])
  node.writeUInt16LE(node.length, 0)
  node.writeUInt16LE(text ? value.length + 1 : value.length, 2)
  node.writeUInt16LE(text ? 1 : 0, 4)
  return node
}

function versionResource(fileVersion, productVersion) {
  const fixed = Buffer.alloc(52)
  fixed.writeUInt32LE(0xfeef04bd, 0)
  const strings = versionNode('StringFileInfo', Buffer.alloc(0), [versionNode('040904B0', Buffer.alloc(0), [
    versionNode('CompanyName', 'RainyMarks'), versionNode('FileVersion', fileVersion), versionNode('ProductVersion', productVersion)])])
  return versionNode('VS_VERSION_INFO', fixed, [strings])
}

test('reads FileVersion and ProductVersion from the version resource only', () => {
  const decoy = Buffer.from('FileVersion\0 9.9.9\0', 'utf16le')
  const exe = Buffer.concat([Buffer.alloc(13, 7), decoy, versionResource('4.0.0', '4.0.0-beta.1'), Buffer.alloc(9)])
  assert.deepEqual(versionStrings(exe), { fileVersion: '4.0.0', productVersion: '4.0.0-beta.1' })
  assert.throws(() => versionStrings(Buffer.concat([decoy, Buffer.alloc(64)])), /one version resource, found 0/u)
})

test('a release needs every input of its own version', () => {
  const inputs = version => ({
    host: { version: 2, host: version },
    linux: { version },
    exe: { fileVersion: version, productVersion: version },
    modules: { modules: [
      { id: 'strata', baseUrl: 'https://github.com/RainyMarks/RainyAgent/releases/download/v1.0.0-resources/' },
      { id: 'php', baseUrl: `https://github.com/RainyMarks/Rainy/releases/download/v${version}-resources/` },
    ] },
  })
  assert.deepEqual(releaseMismatches('4.0.0', inputs('4.0.0')), [])
  const problems = releaseMismatches('4.0.0', inputs('3.9.1'))
  assert.equal(problems.length, 4)
  assert.match(problems.join('\n'), /windows-host.*3\.9\.1/u)
  assert.match(problems.join('\n'), /linux-runtime\.json is for 3\.9\.1/u)
  assert.match(problems.join('\n'), /Rainy\.exe is 3\.9\.1/u)
  assert.match(problems.join('\n'), /downloads php from .*v3\.9\.1-resources/u)
  assert.equal(releaseMismatches('4.0.0', { ...inputs('4.0.0'), host: { version: 2 }, linux: {} }).length, 2)
  assert.equal(releaseMismatches('4.0.0', { ...inputs('4.0.0'), exe: { fileVersion: '4.0.0' } }).length, 1)
})

test('finds the raw release keys an executable carries', () => {
  const pem = () => generateKeyPairSync('ed25519').publicKey.export({ format: 'pem', type: 'spki' })
  const [first, second] = [pem(), pem()]
  const raw = createPublicKey(first).export({ format: 'der', type: 'spki' }).subarray(-32)
  const keyring = { version: 1, keys: { aaa: first, bbb: second } }
  assert.deepEqual(carriedKeys(Buffer.concat([Buffer.alloc(100, 1), raw, Buffer.alloc(5)]), keyring), ['aaa'])
  assert.deepEqual(carriedKeys(Buffer.alloc(200, 1), keyring), [])
})

function tarEntry(name, data, { prefix = '', type = '0', magic = 'ustar\0' } = {}) {
  const header = Buffer.alloc(512)
  header.write(name, 0, 'utf8')
  header.write(`${data.length.toString(8).padStart(11, '0')}\0`, 124, 'latin1')
  header.write(type, 156, 'latin1')
  header.write(magic, 257, 'latin1')
  if (prefix) header.write(prefix, 345, 'utf8')
  return Buffer.concat([header, data, Buffer.alloc((512 - (data.length % 512)) % 512)])
}

test('takes one regular file out of a tar archive', () => {
  const archive = Buffer.concat([
    tarEntry('7zip/', Buffer.alloc(0), { type: '5' }),
    tarEntry('7zip/bin/7za.exe', Buffer.from('decoy'), { type: '2' }),
    tarEntry('./7zip/LICENSE.txt', Buffer.alloc(700, 65)),
    tarEntry('bin/7za.exe', Buffer.from('MZ seven'), { prefix: '7zip' }),
    Buffer.alloc(1024),
  ])
  assert.equal(tarMember(archive, '7zip/bin/7za.exe').toString(), 'MZ seven')
  assert.equal(tarMember(archive, '7zip/LICENSE.txt').length, 700)
  assert.throws(() => tarMember(archive, '7zip/bin/7z.dll'), /not in the tar archive/u)
  // A GNU header keeps other data where ustar has the prefix.
  const gnu = Buffer.concat([tarEntry('7za.exe', Buffer.from('gnu'), { prefix: 'junk', magic: 'ustar  \0' }), Buffer.alloc(1024)])
  assert.equal(tarMember(gnu, '7za.exe').toString(), 'gnu')
})

test('setup deletes what an earlier 4.x shipped and this one does not', () => {
  const history = parseHistory('# comment\r\nRainyAgent.exe\nLICENSE.upstream\nresources\\\nlicenses\\\nlicenses\\crates.txt\nresources\\old.json\n')
  const entries = [
    { path: 'RainyAgent.exe', directory: false }, { path: 'resources', directory: true }, { path: 'resources\\windows-host', directory: true },
    { path: 'resources\\windows-host\\runtime.json', directory: false }, { path: 'resources\\New.json', directory: false },
  ]
  const payload = payloadPaths(entries)
  assert.deepEqual(payload, ['RainyAgent.exe', 'resources\\', 'resources\\New.json'])
  const { retired, added } = compareHistory(history, ['rainyagent.exe', ...payload.slice(1)])
  assert.deepEqual(retired, ['LICENSE.upstream', 'licenses\\', 'licenses\\crates.txt', 'resources\\old.json'])
  assert.deepEqual(added, ['resources\\New.json'])
  const include = uninstallList(entries, '4.1.0', retired)
  const macro = include.slice(include.indexOf('!macro rainyRemoveRetired'))
  assert.match(macro, /rainyDeleteFile "\$INSTDIR\\LICENSE\.upstream"/u)
  assert.match(macro, /rainyDeleteFile "\$INSTDIR\\licenses\\crates\.txt"/u)
  assert.match(macro, /RMDir "\$INSTDIR\\licenses"\n!macroend/u)
  assert.doesNotMatch(include, /windows-host\\runtime/u)
})

test('shipped-files.txt holds only paths setup may delete', () => {
  for (const path of ['RainyAgent.exe', 'resources\\icon.ico', 'licenses\\']) assert.ok(deletablePath(path), path)
  for (const path of ['C:\\Windows\\x.dll', '\\x', '..\\x', 'resources\\..\\..\\x', 'resources\\windows-host\\host.js', 'runtime\\wsl',
    'Tools\\x', '.rainy-toolpack\\x', 'locales\\*.pak', 'a/b', 'Uninstall RainyAgent.exe', 'a\\\\b']) {
    assert.ok(!deletablePath(path), path)
  }
  assert.throws(() => parseHistory('resources\\windows-host\\x\n'), /may not delete/u)
})

test('lists the registry crates the executable links, not build or dev dependencies', () => {
  const crate = (name, source = 'registry+https://github.com/rust-lang/crates.io-index') => ({ id: name, name, version: '1.0.0', source })
  const normal = pkg => ({ pkg, dep_kinds: [{ kind: null, target: null }] })
  const metadata = {
    packages: [crate('rainy', null), crate('rainy-carrier', null), crate('serde'), crate('cc'), crate('tempfile'), crate('itoa'), crate('unused')],
    resolve: { nodes: [
      { id: 'rainy', deps: [normal('rainy-carrier'), { pkg: 'cc', dep_kinds: [{ kind: 'build', target: null }] },
        { pkg: 'tempfile', dep_kinds: [{ kind: 'dev', target: null }] }] },
      { id: 'rainy-carrier', deps: [normal('serde')] },
      { id: 'serde', deps: [normal('itoa')] },
      { id: 'cc', deps: [] }, { id: 'tempfile', deps: [] }, { id: 'itoa', deps: [] }, { id: 'unused', deps: [] },
    ] },
  }
  assert.deepEqual(linkedCrates(metadata).map(item => item.name), ['itoa', 'serde'])
  // A registry crate copied into native/vendor and patched in is listed; a workspace crate is not.
  metadata.packages.push({ ...crate('gpui-pre', null), manifest_path: 'C:\\r\\native\\vendor\\gpui-pre\\Cargo.toml' })
  metadata.resolve.nodes[1].deps.push(normal('gpui-pre'))
  metadata.resolve.nodes.push({ id: 'gpui-pre', deps: [] })
  assert.deepEqual(linkedCrates(metadata).map(item => item.name), ['gpui-pre', 'itoa', 'serde'])
  metadata.packages.push(crate('gitdep', 'git+https://example.com/x'))
  metadata.resolve.nodes[2].deps.push(normal('gitdep'))
  metadata.resolve.nodes.push({ id: 'gitdep', deps: [] })
  assert.throws(() => linkedCrates(metadata), /only registry crates/u)
})
