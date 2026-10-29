/**
 * Package the native Rainy (`native/`, GPUI) for Windows x64 from finished build outputs:
 *
 * 1. check that every input belongs to the version in native/Cargo.toml (a QA build only warns), then stage
 *    `release/win-unpacked` afresh: `RainyAgent.exe` (native/target/release/Rainy.exe), `WebView2Loader.dll`, the
 *    license files, `THIRD_PARTY_NOTICES.md` with the Rust crates of Rainy.exe and both Hosts and their license texts
 *    appended, and `resources\` (runtime/windows-host plus the resources beside it);
 * 2. check that RainyAgent.exe carries the public release keys and sign the resources with the release key
 *    (`scripts/sign-release.mjs`); with --skip-sign, check that it carries none;
 * 3. build `Rainy-<version>-windows-x64-setup.exe` from `native/installer/rainy.nsi` with NSIS 3.04, extracted afresh from
 *    the pinned electron-builder archives. Setup also deletes what an earlier 4.x installed and this version no longer
 *    ships (`native/installer/shipped-files.txt`, which a release extends);
 * 4. write `latest.yml` (the electron-builder shape every 3.x and 4.x updater reads) and `latest.yml.sig`.
 *
 * Inputs come from `native/scripts/build-release.sh`, `scripts/build.ts --release`, `scripts/stage-windows.mjs`, the
 * Linux layers in runtime/ and `scripts/prepare-optional-modules.mjs` (all run by `scripts/package.ps1`). Options:
 *   --qa              the test identity ("Rainy QA", its own registry keys, no shortcuts); output in <qa-root>/out only
 *   --qa-root <dir>   QA_ROOT: the QA default folder's parent and setup.log, and the parent of the QA user data that
 *                     a QA setup passes to the app (default tmp/qa/installer)
 *   --skip-sign       no resource signature and no latest.yml.sig (only for a Rainy.exe built with RAINY_ALLOW_UNSIGNED=1)
 * RAINY_NSIS_DIR names another NSIS folder (makensis.exe; plugins from RAINY_NSIS_PLUGINS or its Plugins\x86-unicode),
 * and RAINY_RELEASE_SIGNING_KEY another private key, as for 3.x.
 */
import { createHash, createPublicKey, sign, verify } from 'node:crypto'
import { createReadStream, existsSync } from 'node:fs'
import { appendFile, copyFile, cp, lstat, mkdir, readFile, readdir, rm, stat, writeFile } from 'node:fs/promises'
import { homedir } from 'node:os'
import { basename, dirname, isAbsolute, join, relative, resolve } from 'node:path'
import { spawnSync } from 'node:child_process'
import { pathToFileURL } from 'node:url'
import { gunzipSync } from 'node:zlib'
import { releaseSigningKeyPath } from './release-signing-key.mjs'
import { SIGNED_RESOURCES, resolveReleaseSigner, signReleaseResources } from './sign-release.mjs'

const root = resolve(import.meta.dirname, '..')
const native = join(root, 'native')
const UPDATE_DOMAIN = 'Rainy/update/v1\0'
/** electron-builder's 7-Zip and NSIS archives with the SHA-256 electron-builder pins them by (app-builder-lib toolsets). */
const PINNED_7ZIP = { folder: '7zip@1.0.0', archive: '7zip-win-x64.tar.gz', sha256: 'be071f15bd6da2f78fe81c6ddef2009b0c4d8a51f36b780cb806c7e6df95e1b3',
  member: '7zip/bin/7za.exe' }
const PINNED_NSIS = [
  { folder: 'nsis-3.0.4.1', archive: 'nsis-3.0.4.1.7z', sha256: '9877df902530f96357d13a7a31ae2b9df67f48b11ffc9a1700a7c961574ec5fa', marker: 'makensis.exe' },
  { folder: 'nsis-resources-3.4.1', archive: 'nsis-resources-3.4.1.7z', sha256: '593a9a92ef958321293ac6a2ee61e64bf1bd543142a5bd6b3d310709cc924103',
    marker: 'plugins/x86-unicode/StdUtils.dll' },
]
const PLUGINS = ['StdUtils.dll', 'UAC.dll', 'WinShell.dll']
const WINDOWS_HOST = /^resources\\windows-host(?:\\|$)/iu
const HISTORY = join(native, 'installer/shipped-files.txt')
const LICENSE_FILE = /^(?:licen[cs]e|copying|copyright|notice|unlicense)/iu

function options(argv) {
  const parsed = { qa: false, qaRoot: join(root, 'tmp/qa/installer'), skipSign: false }
  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index]
    if (argument === '--qa') parsed.qa = true
    else if (argument === '--skip-sign') parsed.skipSign = true
    else if (argument === '--qa-root' && argv[index + 1]) parsed.qaRoot = resolve(argv[(index += 1)])
    else throw new Error(`Unknown option: ${argument}`)
  }
  return parsed
}

async function sha(path, algorithm = 'sha256', encoding = 'hex') {
  const hash = createHash(algorithm)
  for await (const chunk of createReadStream(path)) hash.update(chunk)
  return hash.digest(encoding)
}

async function exists(path) {
  try { await lstat(path); return true } catch (error) { if (error.code === 'ENOENT') return false; throw error }
}

function inside(folder, path) {
  const child = relative(resolve(folder), resolve(path))
  return child !== '' && !isAbsolute(child) && child !== '..' && !child.startsWith('..\\') && !child.startsWith('../')
}

function cargoHome() {
  return process.env.CARGO_HOME ? resolve(process.env.CARGO_HOME) : join(homedir(), '.cargo')
}

/** The version every part of the package carries: `[workspace.package] version` in native/Cargo.toml. */
async function nativeVersion() {
  const text = await readFile(join(native, 'Cargo.toml'), 'utf8')
  const section = text.split(/^\[/mu).find(part => part.startsWith('workspace.package]'))
  const version = section?.match(/^version\s*=\s*"([^"]+)"/mu)?.[1]
  if (!version || !/^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/u.test(version)) throw new Error('native/Cargo.toml has no [workspace.package] version')
  return version
}

/** FileVersion and ProductVersion of the executable's version resource (`VS_VERSION_INFO`, as app/build.rs writes it). */
export function versionStrings(exe) {
  const wide = text => Buffer.from(`${text}\0`, 'utf16le')
  const key = wide('VS_VERSION_INFO')
  const blocks = []
  for (let at = exe.indexOf(key); at >= 0; at = exe.indexOf(key, at + 2)) {
    // wLength, wValueLength and wType precede the key; the VS_FIXEDFILEINFO after it starts on a 32-bit boundary.
    const start = at - 6
    const fixed = start + ((at + key.length - start + 3) & ~3)
    if (start >= 0 && fixed + 4 <= exe.length && exe.readUInt32LE(fixed) === 0xfeef04bd) blocks.push(exe.subarray(start, start + exe.readUInt16LE(start)))
  }
  if (blocks.length !== 1) throw new Error(`Expected one version resource, found ${blocks.length}`)
  const [block] = blocks
  const value = name => {
    const at = block.indexOf(wide(name))
    if (at < 6 || block.readUInt16LE(at - 2) !== 1) return undefined
    const characters = block.readUInt16LE(at - 4)
    const start = (at + wide(name).length + 3) & ~3
    return block.subarray(start, start + characters * 2).toString('utf16le').replace(/\0[\s\S]*$/u, '')
  }
  return { fileVersion: value('FileVersion'), productVersion: value('ProductVersion') }
}

/**
 * How the package inputs disagree with `version`, the version setup and latest.yml carry: the Host in windows-host, the
 * Linux runtime, Rainy.exe's version resource and the release the optional modules download from.
 */
export function releaseMismatches(version, { host, linux, exe, modules }) {
  const problems = []
  const hostVersion = typeof host?.host === 'string' ? host.host : undefined
  if (hostVersion !== version) problems.push(`runtime/windows-host/runtime.json holds the Host ${hostVersion ?? 'without a version'}`)
  if (linux?.version !== version) problems.push(`runtime/linux-runtime.json is for ${linux?.version ?? 'no version'}`)
  if (exe.fileVersion !== version || exe.productVersion !== version) {
    problems.push(`native/target/release/Rainy.exe is ${exe.fileVersion ?? 'without a FileVersion'} (ProductVersion ${exe.productVersion ?? 'missing'})`)
  }
  for (const module of Array.isArray(modules?.modules) ? modules.modules : []) {
    if (!String(module?.baseUrl ?? '').endsWith(`/v${version}-resources/`)) {
      problems.push(`runtime/optional-modules.json downloads ${module?.id} from ${module?.baseUrl}`)
    }
  }
  return problems
}

/** The ids of the release keys `exe` carries as the raw 32-byte Ed25519 keys build.rs compiles in. */
export function carriedKeys(exe, keyring) {
  return Object.entries(keyring.keys ?? {}).filter(([, pem]) => {
    const der = createPublicKey(pem).export({ format: 'der', type: 'spki' })
    return exe.includes(der.subarray(der.length - 32))
  }).map(([id]) => id)
}

/** The bytes of the regular file `member` in a tar archive. */
export function tarMember(archive, member) {
  for (let at = 0; at + 512 <= archive.length;) {
    const header = archive.subarray(at, at + 512)
    if (header.every(byte => byte === 0)) break
    const field = (start, length) => header.subarray(start, start + length).toString('utf8').replace(/\0[\s\S]*$/u, '')
    const size = Number.parseInt(field(124, 12).trim() || '0', 8)
    if (!Number.isSafeInteger(size) || size < 0) throw new Error('The tar archive has an invalid header')
    // Only POSIX ustar headers have a name prefix; GNU tar keeps other fields there.
    const prefix = header.subarray(257, 263).toString('latin1') === 'ustar\0' ? field(345, 155) : ''
    const name = (prefix ? `${prefix}/${field(0, 100)}` : field(0, 100)).replace(/^\.\//u, '')
    const body = at + 512
    if ((header[156] === 0x30 || header[156] === 0) && name === member) {
      if (body + size > archive.length) throw new Error(`${member} is cut short in the tar archive`)
      return archive.subarray(body, body + size)
    }
    at = body + Math.ceil(size / 512) * 512
  }
  throw new Error(`${member} is not in the tar archive`)
}

/** Remove a staging folder this script owns; refuses links and anything outside `owner`. */
async function clean(folder, owner) {
  if (!inside(owner, folder)) throw new Error(`Refusing to remove ${folder}, which is not inside ${owner}`)
  if (!await exists(folder)) return
  if ((await lstat(folder)).isSymbolicLink()) throw new Error(`Refusing to remove the link ${folder}`)
  await rm(folder, { recursive: true, force: false, maxRetries: 3 })
}

/**
 * makensis and the plugin folder: NSIS and nsis-resources extracted into `work` (inside `owner`) from the pinned
 * archives with the pinned 7-Zip, so nothing earlier left in electron-builder's cache folders is run or embedded; or
 * RAINY_NSIS_DIR.
 */
async function toolchain(work, owner) {
  if (process.env.RAINY_NSIS_DIR) {
    const folder = resolve(process.env.RAINY_NSIS_DIR)
    const plugins = resolve(process.env.RAINY_NSIS_PLUGINS ?? join(folder, 'Plugins/x86-unicode'))
    for (const file of [join(folder, 'makensis.exe'), ...PLUGINS.map(name => join(plugins, name))]) {
      if (!await exists(file)) throw new Error(`RAINY_NSIS_DIR: ${file} is missing`)
    }
    console.warn(`Using the NSIS in ${folder} (not the pinned bundle)`)
    return { makensis: join(folder, 'makensis.exe'), plugins }
  }
  const cache = join(process.env.LOCALAPPDATA ?? join(homedir(), 'AppData/Local'), 'electron-builder/Cache')
  const pinned = async pin => {
    const path = join(cache, pin.folder, pin.archive)
    if (!await exists(path)) throw new Error(`The pinned archive is missing: ${path} (electron-builder downloads it; set RAINY_NSIS_DIR to use another NSIS)`)
    const bytes = await readFile(path)
    const digest = createHash('sha256').update(bytes).digest('hex')
    if (digest !== pin.sha256) throw new Error(`${path} has SHA-256 ${digest}, not the pinned ${pin.sha256}`)
    return bytes
  }
  await clean(work, owner)
  await mkdir(work, { recursive: true })
  const sevenZip = join(work, '7za.exe')
  await writeFile(sevenZip, tarMember(gunzipSync(await pinned(PINNED_7ZIP)), PINNED_7ZIP.member))
  const folders = []
  for (const pin of PINNED_NSIS) {
    // The checked bytes are what 7-Zip reads.
    const archive = join(work, pin.archive)
    await writeFile(archive, await pinned(pin))
    const folder = join(work, pin.folder)
    const result = spawnSync(sevenZip, ['x', '-y', '-bd', '-bso0', '-bsp0', `-o${folder}`, archive], { stdio: ['ignore', 'ignore', 'inherit'], windowsHide: true })
    if (result.error) throw result.error
    if (result.status !== 0) throw new Error(`7-Zip could not extract ${pin.archive} (exit code ${result.status})`)
    if (!await exists(join(folder, pin.marker))) throw new Error(`${pin.archive} has no ${pin.marker}`)
    folders.push(folder)
  }
  const plugins = join(folders[1], 'plugins/x86-unicode')
  for (const name of PLUGINS) if (!await exists(join(plugins, name))) throw new Error(`${join(plugins, name)} is missing`)
  return { makensis: join(folders[0], 'makensis.exe'), plugins }
}

/** A crate copied into `native/vendor/` and used through `[patch.crates-io]`: a path package outside the workspace. */
const isVendored = item => !item.source && /[\\/]vendor[\\/][^\\/]+[\\/]Cargo\.toml$/u.test(item.manifest_path ?? '')

/**
 * The registry crates `root` links, and the vendored copies of registry crates: its normal dependencies, transitively,
 * in `cargo metadata --filter-platform` output.
 */
export function linkedCrates(metadata, rootName = 'rainy') {
  const packages = new Map(metadata.packages.map(item => [item.id, item]))
  const nodes = new Map(metadata.resolve.nodes.map(node => [node.id, node]))
  const start = metadata.packages.find(item => item.name === rootName && !item.source)
  if (!start) throw new Error(`cargo metadata has no workspace package ${rootName}`)
  const seen = new Set()
  const queue = [start.id]
  while (queue.length) {
    const id = queue.pop()
    if (seen.has(id)) continue
    seen.add(id)
    for (const dependency of nodes.get(id)?.deps ?? []) {
      if (dependency.dep_kinds.some(kind => kind.kind === null)) queue.push(dependency.pkg)
    }
  }
  const crates = [...seen].map(id => packages.get(id)).filter(item => item.source || isVendored(item))
  for (const crate of crates.filter(item => item.source)) {
    if (!crate.source.startsWith('registry+')) throw new Error(`${crate.name} ${crate.version} comes from ${crate.source}; only registry crates are listed`)
  }
  return crates.sort((a, b) => a.name.localeCompare(b.name) || a.version.localeCompare(b.version))
}

/** A license file's text with line ends and trailing spaces evened out, so copies of one text compare equal. */
function normalized(text) {
  return text.replace(/^\uFEFF/u, '').replace(/\r\n?/gu, '\n').split('\n').map(line => line.trimEnd()).join('\n').trim()
}

/** The crate's license, copying and notice files, including those in a LICENSES folder and its declared license-file. */
async function licenseFiles(crate) {
  const folder = dirname(crate.manifest_path)
  const found = new Set()
  for (const entry of await readdir(folder, { withFileTypes: true })) {
    if (!LICENSE_FILE.test(entry.name)) continue
    if (entry.isFile()) found.add(join(folder, entry.name))
    else if (entry.isDirectory()) {
      for (const inner of await readdir(join(folder, entry.name), { withFileTypes: true })) if (inner.isFile()) found.add(join(folder, entry.name, inner.name))
    }
  }
  if (crate.license_file && existsSync(resolve(folder, crate.license_file))) found.add(resolve(folder, crate.license_file))
  return [...found].sort()
}

/** The Linux Host's target (native/scripts/build-hosts.sh). */
const LINUX_TARGET = 'x86_64-unknown-linux-gnu'

/**
 * The crates RainyAgent.exe and the Windows Host link on the release target of native/rust-toolchain.toml, and the
 * Linux Host on its own, with their license texts.
 */
async function rustLicenses() {
  const target = (await readFile(join(native, 'rust-toolchain.toml'), 'utf8')).match(/^targets\s*=\s*\[\s*"([^"]+)"/mu)?.[1]
  if (!target) throw new Error('native/rust-toolchain.toml names no target')
  const installed = join(cargoHome(), 'bin/cargo.exe')
  const cargo = process.env.CARGO ?? (existsSync(installed) ? installed : 'cargo')
  const metadataFor = platform => {
    const result = spawnSync(cargo, ['metadata', '--offline', '--locked', '--format-version', '1', '--filter-platform', platform],
      { cwd: native, encoding: 'utf8', maxBuffer: 1 << 28, windowsHide: true })
    if (result.error) throw result.error
    if (result.status !== 0) throw new Error(`cargo metadata failed:\n${result.stderr}`)
    return JSON.parse(result.stdout)
  }
  const [windows, linux] = [metadataFor(target), metadataFor(LINUX_TARGET)]
  const byId = new Map()
  for (const [metadata, rootName] of [[windows, 'rainy'], [windows, 'rainy-host'], [linux, 'rainy-host']]) {
    for (const crate of linkedCrates(metadata, rootName)) byId.set(crate.id, crate)
  }
  const crates = [...byId.values()].sort((a, b) => a.name.localeCompare(b.name) || a.version.localeCompare(b.version))
  const texts = new Map()
  const unlicensed = []
  for (const crate of crates) {
    const files = await licenseFiles(crate)
    if (!files.length) unlicensed.push(crate)
    for (const file of files) {
      const text = normalized(await readFile(file, 'utf8'))
      if (!texts.has(text)) texts.set(text, [])
      texts.get(text).push(`${crate.name} ${crate.version} (${relative(dirname(crate.manifest_path), file).replaceAll('\\', '/')})`)
    }
  }
  return { target, crates, texts, unlicensed }
}

function fenced(text) {
  const longest = Math.max(0, ...(text.match(/`+/gu) ?? []).map(run => run.length))
  const fence = '`'.repeat(Math.max(3, longest + 1))
  return `${fence}text\n${text}\n${fence}`
}

async function notices() {
  const base = (await readFile(join(root, 'THIRD_PARTY_NOTICES.md'), 'utf8')).trimEnd()
  const { target, crates, texts, unlicensed } = await rustLicenses()
  const loader = normalized(await readFile(join(native, 'installer/WebView2Loader-LICENSE.txt'), 'utf8'))
  const cell = text => String(text).replaceAll('|', '\\|').replaceAll('<', '\\<').replaceAll('>', '\\>')
  const license = crate => crate.license ?? (crate.license_file ? `see ${basename(crate.license_file)} in the crate` : 'not declared')
  const lines = [base, '', '## Rust crates in RainyAgent.exe and the Host', '',
    `RainyAgent.exe and the Host (\`rainy-host.exe\`, and \`rainy-host\` in the Linux runtime for WSL and SSH) are built from the Rust `
      + `workspace in \`native/\` for ${target} and ${LINUX_TARGET} and link the crates below, as \`native/Cargo.lock\` locks them. `
      + 'Each is used under the license its Cargo.toml declares; their license and notice files follow.', '',
    '| Crate | Version | License |', '| --- | --- | --- |', ...crates.map(crate => `| ${crate.name} | ${crate.version}${isVendored(crate) ? ' (changed by Rainy, see native/vendor/' + basename(dirname(crate.manifest_path)) + '/RAINY.md)' : ''} | ${cell(license(crate))} |`), '',
    '## WebView2Loader.dll', '',
    '`WebView2Loader.dll` is the Microsoft Edge WebView2 loader of the Microsoft WebView2 SDK, as the `webview2-com-sys` crate carries '
      + 'it. Microsoft licenses it under these terms:', '', fenced(loader), '',
    '## License and notice files of the Rust crates', '', 'A text that several crates ship is given once, after the files that hold it.', '']
  let index = 0
  for (const [text, files] of texts) {
    index += 1
    lines.push(`### Text ${index}`, '', `${files.join(', ')}:`, '', fenced(text), '')
  }
  if (unlicensed.length) {
    lines.push('## Rust crates without license files', '',
      'These crates ship no license file. Their Cargo.toml declares the license and names the authors, who hold the copyright; the '
        + 'license texts appear above.', '',
      '| Crate | Version | License | Authors |', '| --- | --- | --- | --- |',
      ...unlicensed.map(crate => `| ${crate.name} | ${crate.version} | ${cell(license(crate))} | ${cell((crate.authors ?? []).join(', ') || 'not named')} |`), '')
  }
  return lines.join('\n').trimEnd() + '\n'
}

async function walk(folder, base = folder, rows = []) {
  for (const entry of (await readdir(folder, { withFileTypes: true })).sort((a, b) => a.name.localeCompare(b.name))) {
    const path = join(folder, entry.name)
    if (entry.isSymbolicLink()) throw new Error(`The package cannot contain a link: ${path}`)
    if (entry.isDirectory()) {
      rows.push({ path: relative(base, path), directory: true })
      await walk(path, base, rows)
    } else if (entry.isFile()) {
      rows.push({ path: relative(base, path), directory: false, bytes: (await stat(path)).size })
    } else throw new Error(`Unexpected package entry: ${path}`)
  }
  return rows
}

/** Whether setup may delete `path` (a shipped-files.txt line) below the installation folder. */
export function deletablePath(path) {
  const parts = path.replace(/\\$/u, '').split('\\')
  return !/[/:*?"<>|]/u.test(path) && !parts.some(part => part.trim() === '' || part === '.' || part === '..')
    && !WINDOWS_HOST.test(path) && !['runtime', 'tools', '.rainy-toolpack'].includes(parts[0].toLowerCase())
    && path.toLowerCase() !== 'uninstall rainyagent.exe'
}

/** The paths in shipped-files.txt. */
export function parseHistory(text) {
  const paths = []
  for (const line of text.split(/\r?\n/u).map(raw => raw.trim())) {
    if (!line || line.startsWith('#')) continue
    if (!deletablePath(line)) throw new Error(`native/installer/shipped-files.txt: setup may not delete ${line}`)
    paths.push(line)
  }
  return paths
}

/** The package entries outside resources\windows-host as shipped-files.txt records them (a folder ends in `\`). */
export function payloadPaths(entries) {
  return entries.filter(entry => !WINDOWS_HOST.test(entry.path)).map(entry => entry.directory ? `${entry.path}\\` : entry.path)
}

/** What an earlier 4.x shipped and this payload does not (`retired`), and what this payload ships for the first time. */
export function compareHistory(history, payload) {
  const now = new Set(payload.map(path => path.toLowerCase()))
  const known = new Set(history.map(path => path.toLowerCase()))
  return { retired: history.filter(path => !now.has(path.toLowerCase())), added: payload.filter(path => !known.has(path.toLowerCase())) }
}

/**
 * `installer-files.nsh`: what setup installs besides resources\windows-host, which the uninstaller deletes once
 * RainyAgent.exe --rainy-remove-runtime removed windows-host, and what earlier 4.x versions installed that setup deletes.
 */
export function uninstallList(entries, version, retired = []) {
  const own = entries.filter(entry => !WINDOWS_HOST.test(entry.path))
  const quote = path => `"$INSTDIR\\${path.replace(/\\$/u, '').replaceAll('$', '$$$$')}"`
  const deepestFirst = (a, b) => b.split('\\').length - a.split('\\').length || a.localeCompare(b)
  const files = own.filter(entry => !entry.directory).map(entry => entry.path)
    .sort((a, b) => Number(b === 'RainyAgent.exe') - Number(a === 'RainyAgent.exe') || a.localeCompare(b))
  const folders = own.filter(entry => entry.directory).map(entry => entry.path).sort(deepestFirst)
  const retiredFiles = retired.filter(path => !path.endsWith('\\')).sort((a, b) => a.localeCompare(b))
  const retiredFolders = retired.filter(path => path.endsWith('\\')).map(path => path.slice(0, -1)).sort(deepestFirst)
  return `; Generated by scripts/package-native.mjs for Rainy ${version}: what setup installs besides resources\\windows-host,\n`
    + '; which the uninstaller deletes once RainyAgent.exe --rainy-remove-runtime removed windows-host, and what an earlier\n'
    + '; 4.x installed that this version no longer ships (native/installer/shipped-files.txt), which setup deletes.\n'
    + '!macro rainyRemoveOwnFiles\n' + files.map(path => `  !insertmacro rainyDeleteFile ${quote(path)}\n`).join('') + '!macroend\n'
    + '!macro rainyRemoveOwnFolders\n' + folders.map(path => `  RMDir ${quote(path)}\n`).join('') + '!macroend\n'
    + '!macro rainyRemoveRetired\n' + retiredFiles.map(path => `  !insertmacro rainyDeleteFile ${quote(path)}\n`).join('')
    + retiredFolders.map(path => `  RMDir ${quote(path)}\n`).join('') + '!macroend\n'
}

/** The retired paths for this payload; a release adds what it ships for the first time to shipped-files.txt. */
async function shippedHistory(entries, record) {
  const history = parseHistory(await readFile(HISTORY, 'utf8'))
  const { retired, added } = compareHistory(history, payloadPaths(entries))
  if (added.length && record) {
    for (const path of added) if (!deletablePath(path)) throw new Error(`The package path ${path} cannot be recorded in shipped-files.txt`)
    const text = await readFile(HISTORY, 'utf8')
    await appendFile(HISTORY, (text.endsWith('\n') ? '' : '\n') + added.map(path => `${path}\n`).join(''))
    console.log(`Recorded in native/installer/shipped-files.txt: ${added.join(', ')}`)
  }
  return retired
}

async function stage(target, version, release) {
  const inputs = {
    exe: join(native, 'target/release/Rainy.exe'),
    loader: join(native, 'target/release/WebView2Loader.dll'),
    windowsHost: join(root, 'runtime/windows-host'),
    icon: join(native, 'crates/app/assets/brand/rainy.ico'),
  }
  const resources = {
    'release-public-keys.json': join(root, 'runtime/release-public-keys.json'),
    'environment-component-catalog.json': join(root, 'runtime/environment-component-catalog.json'),
    'install-environment-component.py': join(root, 'scripts/install-environment-component.py'),
    'linux-runtime.json': join(root, 'runtime/linux-runtime.json'),
    'linux-app.tar.gz': join(root, 'runtime/linux-app.tar.gz'),
    'install-runtime.py': join(root, 'scripts/install-runtime.py'),
    'optional-modules.json': join(root, 'runtime/optional-modules.json'),
    'icon.ico': inputs.icon,
  }
  if (Object.keys(resources).join() !== SIGNED_RESOURCES.join()) throw new Error('The staged resources differ from the signed list')
  for (const path of [...Object.values(inputs), ...Object.values(resources), join(inputs.windowsHost, 'runtime.json')]) {
    if (!await exists(path)) throw new Error(`Missing package input: ${path}`)
  }
  const linux = JSON.parse(await readFile(resources['linux-runtime.json'], 'utf8'))
  const app = await sha(resources['linux-app.tar.gz'])
  if (linux.app?.sha256 !== app || linux.app?.bytes !== (await stat(resources['linux-app.tar.gz'])).size) {
    throw new Error('runtime/linux-app.tar.gz is not the application layer runtime/linux-runtime.json names; stage the Linux runtime again')
  }
  const host = JSON.parse(await readFile(join(inputs.windowsHost, 'runtime.json'), 'utf8'))
  if (host.platform !== 'win32' || !Array.isArray(host.files)) throw new Error('runtime/windows-host/runtime.json is not a Windows Host inventory')
  const modules = JSON.parse(await readFile(resources['optional-modules.json'], 'utf8'))
  const problems = releaseMismatches(version, { host, linux, exe: versionStrings(await readFile(inputs.exe)), modules })
  if (problems.length && release) {
    throw new Error(`The package inputs are not all Rainy ${version}:\n- ${problems.join('\n- ')}\n`
      + 'Run scripts/package.ps1 to build them for this version, or use --qa for a test build.')
  }
  if (problems.length) console.warn(`A QA build of inputs that are not all ${version}:\n- ${problems.join('\n- ')}`)
  await mkdir(join(target, 'resources'), { recursive: true })
  await copyFile(inputs.exe, join(target, 'RainyAgent.exe'))
  await copyFile(inputs.loader, join(target, 'WebView2Loader.dll'))
  for (const name of ['LICENSE', 'LICENSE.Rainy', 'LICENSE.upstream']) await copyFile(join(root, name), join(target, name))
  await writeFile(join(target, 'THIRD_PARTY_NOTICES.md'), await notices())
  await cp(inputs.windowsHost, join(target, 'resources/windows-host'), { recursive: true, force: false, errorOnExist: true, verbatimSymlinks: true })
  for (const [name, source] of Object.entries(resources)) await copyFile(source, join(target, 'resources', name))
  const built = await stat(inputs.exe)
  console.log(`Staged ${target}: RainyAgent.exe ${built.size} bytes from ${built.mtime.toISOString()}, Windows Host of ${host.files.length} files`)
}

function makensis(tool, defines, script) {
  const args = ['-INPUTCHARSET', 'UTF8', '-WX', '-V2', ...Object.entries(defines).map(([name, value]) => value === true ? `-D${name}` : `-D${name}=${value}`), script]
  const result = spawnSync(tool.makensis, args, { stdio: 'inherit', windowsHide: true })
  if (result.error) throw result.error
  if (result.status !== 0) throw new Error(`makensis failed with exit code ${result.status}`)
}

/** latest.yml exactly as electron-builder writes it, which 3.x's electron-updater and the 4.x updater both parse. */
function latestYml(version, name, sha512, size) {
  return `version: ${version}\nfiles:\n  - url: ${name}\n    sha512: ${sha512}\n    size: ${size}\npath: ${name}\nsha512: ${sha512}\n`
    + `releaseDate: '${new Date().toISOString()}'\n`
}

async function main() {
  const started = Date.now()
  const option = options(process.argv.slice(2))
  const version = await nativeVersion()
  const [major, minor, patch] = version.split(/[.-]/u)
  const out = option.qa ? join(option.qaRoot, 'out') : join(root, 'release')
  const target = join(out, 'win-unpacked')
  const nsis = join(out, 'nsis')
  const name = option.qa ? `Rainy-QA-${version}-windows-x64-setup.exe` : `Rainy-${version}-windows-x64-setup.exe`
  const setup = join(out, name)
  const keyPath = releaseSigningKeyPath(process.env.RAINY_RELEASE_SIGNING_KEY)
  if (!option.skipSign && (inside(out, keyPath) || resolve(out) === resolve(keyPath))) throw new Error('The private release key must be outside the output folder')
  await mkdir(out, { recursive: true })
  // A run that stops on the way leaves nothing of this version that could be published.
  for (const file of [name, 'latest.yml', 'latest.yml.sig', 'release-integrity.json']) await rm(join(out, file), { force: true })
  await clean(target, out)
  const tool = await toolchain(join(nsis, 'toolchain'), out)
  await stage(target, version, !option.qa)

  const keys = JSON.parse(await readFile(join(root, 'runtime/release-public-keys.json'), 'utf8'))
  // A release Rainy.exe checks its resources against the keys build.rs compiled in, as raw 32-byte Ed25519 keys.
  const carried = carriedKeys(await readFile(join(target, 'RainyAgent.exe')), keys)
  let signed
  if (option.skipSign) {
    if (carried.length) {
      throw new Error(`native/target/release/Rainy.exe carries release key ${carried[0]} and would not start without the signed resources; `
        + 'build it with RAINY_ALLOW_UNSIGNED=1 or drop --skip-sign')
    }
    console.warn('Not signing: this package runs only with a Rainy.exe built with RAINY_ALLOW_UNSIGNED=1')
  } else {
    const missing = Object.keys(keys.keys ?? {}).find(id => !carried.includes(id))
    if (missing) throw new Error(`native/target/release/Rainy.exe does not carry release key ${missing}; build it with RAINY_RELEASE_PUBLIC_KEYS=runtime\\release-public-keys.json`)
    signed = await signReleaseResources(join(target, 'resources'), { privateKeyPath: keyPath, version, packageRoot: out })
    console.log(`Signed ${signed.files} resources (release-manifest.signed.json ${signed.manifestSha256})`)
  }

  const entries = await walk(target)
  const bytes = entries.reduce((sum, entry) => sum + (entry.bytes ?? 0), 0)
  const retired = await shippedHistory(entries, !option.qa)
  await mkdir(nsis, { recursive: true })
  const include = join(nsis, 'installer-files.nsh')
  await writeFile(include, uninstallList(entries, version, retired))
  const defines = { VERSION: version, VI_VERSION: `${major}.${minor}.${patch}.0`, YEAR: String(new Date().getUTCFullYear()), PAYLOAD: target,
    PAYLOAD_FILES: include, OUTFILE: setup, ICON: join(native, 'crates/app/assets/brand/rainy.ico'), NSIS_PLUGINS: tool.plugins,
    ESTIMATED_SIZE: String(Math.ceil(bytes / 1024)) }
  if (option.qa) Object.assign(defines, { QA: true, QA_ROOT: option.qaRoot, QA_ARGS: `--user-data-dir=${join(option.qaRoot, 'user-data')}` })
  const compiling = Date.now()
  makensis(tool, defines, join(native, 'installer/rainy.nsi'))
  const compiled = Date.now() - compiling

  const size = (await stat(setup)).size
  const sha512 = await sha(setup, 'sha512', 'base64')
  const yml = Buffer.from(latestYml(version, name, sha512, size))
  await writeFile(join(out, 'latest.yml'), yml)
  if (!option.skipSign) {
    const { key } = await resolveReleaseSigner(keyPath, keys)
    const message = Buffer.concat([Buffer.from(UPDATE_DOMAIN), yml])
    const signature = sign(null, message, key)
    if (!verify(null, message, createPublicKey(key), signature)) throw new Error('latest.yml.sig does not verify')
    await writeFile(join(out, 'latest.yml.sig'), signature.toString('base64url') + '\n')
  }
  const evidence = { version, setup: name, bytes: size, sha512, packageFiles: entries.filter(entry => !entry.directory).length, packageBytes: bytes,
    signedResources: signed?.files ?? 0, manifestSha256: signed?.manifestSha256 ?? null, retiredFiles: retired, authenticode: 'not-configured', qa: option.qa }
  await writeFile(join(out, 'release-integrity.json'), JSON.stringify(evidence, null, 2) + '\n')
  console.log(JSON.stringify({ ...evidence, output: out, makensisSeconds: Math.round(compiled / 100) / 10, totalSeconds: Math.round((Date.now() - started) / 100) / 10 }))
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) await main()
