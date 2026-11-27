/** Regression tests for shared build helpers, installer process checks, runtime installer helpers, and NSIS invariants. */
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { execFile, spawnSync } from 'node:child_process'
import { promisify } from 'node:util'
import { mkdir, mkdtemp, readFile, rm, symlink, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { isAbsolute, join, relative, resolve } from 'node:path'
import { pathToFileURL } from 'node:url'
import { fileDigest, isInsidePath, isMainEntry, isOutsidePath, parseFlagPairs, sha256Hex } from '../../scripts/lib/common.mjs'

const run = promisify(execFile)
const repoRoot = resolve(import.meta.dirname, '../..')

function findPython() {
  for (const cmd of process.platform === 'win32' ? ['python', 'py'] : ['python3', 'python']) {
    const probe = spawnSync(cmd, cmd === 'py' ? ['-3', '--version'] : ['--version'], { stdio: 'ignore', windowsHide: true })
    if (probe.status === 0) return cmd === 'py' ? { cmd, prefix: ['-3'] } : { cmd, prefix: [] }
  }
  return null
}

async function tempDir(t, prefix) {
  const dir = await mkdtemp(join(tmpdir(), prefix))
  t.after(async () => {
    const child = relative(tmpdir(), dir)
    assert(child && !child.startsWith('..') && !isAbsolute(child))
    await rm(dir, { recursive: true, force: true, maxRetries: 5 })
  })
  return dir
}

test('scripts/lib/common.mjs hashes files, parses flag pairs, checks CLI entrypoints, and enforces realpath boundaries', async t => {
  const root = await tempDir(t, 'rainy-common-')
  const sample = join(root, 'sample.txt')
  await writeFile(sample, 'rainy-common-test')
  const { bytes, digest } = await fileDigest(sample)
  assert.equal(bytes, 17)
  assert.equal(digest, sha256Hex('rainy-common-test'))

  assert.deepEqual(parseFlagPairs(['--a', '1', '--b', 'two'], ['--a', '--b'], 'usage'), { '--a': '1', '--b': 'two' })
  assert.throws(() => parseFlagPairs(['--a'], ['--a'], 'bad-usage'), /bad-usage/)
  assert.throws(() => parseFlagPairs(['--x', '1'], ['--a'], 'usage'), /Unknown option: --x/)

  const entryUrl = pathToFileURL(sample).href
  assert.equal(isMainEntry(entryUrl, sample), true)
  assert.equal(isMainEntry(entryUrl, join(root, 'other.txt')), false)
  if (process.platform === 'win32') {
    assert.equal(isMainEntry(entryUrl, sample.toUpperCase()), true)
  }

  const protectedDir = join(root, 'protected')
  const innerDir = join(protectedDir, 'inner')
  const outsideDir = join(root, 'outside')
  await mkdir(innerDir, { recursive: true })
  await mkdir(outsideDir, { recursive: true })
  assert.equal(await isInsidePath(protectedDir, join(innerDir, 'future.bin')), true)
  assert.equal(await isOutsidePath(protectedDir, join(outsideDir, 'key.pem')), true)
  assert.equal(await isOutsidePath(protectedDir, protectedDir), false)

  const alias = join(outsideDir, 'link-into-protected')
  await symlink(innerDir, alias, process.platform === 'win32' ? 'junction' : 'dir')
  assert.equal(await isInsidePath(protectedDir, join(alias, 'key.pem')), true)
  assert.equal(await isOutsidePath(protectedDir, join(alias, 'key.pem')), false)
})

test('check-running.ps1 skips installer ancestor chains and detects quoted space-containing paths', {
  skip: process.platform !== 'win32' ? 'Windows-only installer helper' : false,
}, async t => {
  const root = await tempDir(t, 'rainy-check-running-')
  const emptyInstall = join(root, 'install-root')
  await mkdir(emptyInstall, { recursive: true })
  const checkScript = join(repoRoot, 'native/installer/check-running.ps1')
  const harness = join(root, 'harness.ps1')
  await writeFile(harness, [
    'param([string]$CheckScript, [string]$EmptyRoot)',
    '$ErrorActionPreference = "Stop"',
    '$src = [IO.File]::ReadAllText($CheckScript, [Text.Encoding]::UTF8)',
    '$idx = $src.IndexOf("`ntry {")',
    'if ($idx -lt 0) { $idx = $src.IndexOf("`r`ntry {") }',
    'if ($idx -lt 0) { throw "Could not locate try block in check-running.ps1" }',
    '$defs = $src.Substring(0, $idx) -replace "(?ms)^\\s*param\\s*\\([^)]*\\)", ""',
    'Invoke-Expression $defs',
    '$roots = @("c:\\program files\\rainyagent")',
    `if (-not (Contains-RootExecutable 'cmd /c "C:\\Program Files\\RainyAgent\\Uninstall RainyAgent.exe" /S' $roots)) { throw "missed double-quoted path with space" }`,
    `if (-not (Contains-RootExecutable "powershell -Command '& ''C:\\Program Files\\RainyAgent\\Uninstall RainyAgent.exe'''" $roots)) { throw "missed single-quoted path with space" }`,
    `if (-not (Contains-RootExecutable 'powershell -Command "& C:\\Program Files\\RainyAgent\\resources\\helper.ps1"' @("c:\\program files\\rainyagent\\resources"))) { throw "missed unquoted script under root" }`,
    `if (Contains-RootExecutable 'notepad.exe "C:\\Program Files\\RainyAgent\\README.md"' $roots) { throw "false positive on non-executable file" }`,
    '$fakeProcs = @(',
    '  [pscustomobject]@{ ProcessId = 100; ParentProcessId = 4 },',
    '  [pscustomobject]@{ ProcessId = 200; ParentProcessId = 100 },',
    '  [pscustomobject]@{ ProcessId = 300; ParentProcessId = 200 },',
    '  [pscustomobject]@{ ProcessId = 400; ParentProcessId = 300 },',
    '  [pscustomobject]@{ ProcessId = 500; ParentProcessId = 100 }',
    ')',
    '$excluded = Get-ExcludedPids $fakeProcs 400 300',
    'foreach ($pidId in @(100, 200, 300, 400)) { if (-not $excluded.Contains($pidId)) { throw "missing ancestor pid $pidId" } }',
    'if ($excluded.Contains(500)) { throw "unexpected sibling pid 500 excluded" }',
    '& $CheckScript -InstallRoot $EmptyRoot -InstallerPid $PID -WaitSeconds 0',
    'if ($LASTEXITCODE -ne 0) { throw "check-running.ps1 returned $LASTEXITCODE on idle directory" }',
  ].join('\n') + '\n', 'utf8')

  await run('powershell.exe', [
    '-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-File', harness,
    '-CheckScript', checkScript, '-EmptyRoot', emptyInstall,
  ], { windowsHide: true })
})

test('install-runtime.py validates piece URLs, RAINY_HOME paths, and legacy tar extraction safety', async t => {
  const py = findPython()
  if (!py) t.skip('Python 3 is not installed')
  const root = await tempDir(t, 'rainy-install-runtime-py-')
  const checker = join(root, 'check.py')
  await writeFile(checker, [
    'import importlib.util, io, os, sys, tarfile, tempfile',
    'from pathlib import Path',
    'spec = importlib.util.spec_from_file_location("install_runtime", sys.argv[1])',
    'mod = importlib.util.module_from_spec(spec)',
    'spec.loader.exec_module(mod)',
    'assert mod.validate_piece_url("https://github.com/RainyMarks/Rainy/releases/download/v5.0.0/a.001").startswith("https://")',
    'assert mod.validate_piece_url("http://127.0.0.1:9999/piece").startswith("http://127.0.0.1")',
    'for bad in ("file:///etc/passwd", "http://example.com/p", "https://user:pass@github.com/p", "ftp://example.com/p"):',
    '    try:',
    '        mod.validate_piece_url(bad)',
    '        raise AssertionError(f"accepted bad URL: {bad}")',
    '    except RuntimeError:',
    '        pass',
    'assert str(mod.resolve_rainy_home("/opt/rainy-agent")).replace("\\\\", "/") == "/opt/rainy-agent"',
    'for bad_home in ("relative/dir", "/opt/../etc", "/", "/tmp/bad\\npath"):',
    '    try:',
    '        mod.resolve_rainy_home(bad_home)',
    '        raise AssertionError(f"accepted bad home: {bad_home}")',
    '    except RuntimeError:',
    '        pass',
    'work = Path(sys.argv[2])',
    'def make_tar(path, entries):',
    '    with tarfile.open(path, "w:gz") as tf:',
    '        for info, data in entries:',
    '            tf.addfile(info, io.BytesIO(data) if data is not None else None)',
    'good = work / "good.tar.gz"',
    'i_good = tarfile.TarInfo("dir/ok.txt"); i_good.size = 2; i_good.mode = 0o4755',
    'make_tar(good, [(i_good, b"ok")])',
    'out_good = work / "out-good"; out_good.mkdir()',
    'with tarfile.open(good, "r:*") as tf: mod._unpack_legacy(tf, str(out_good))',
    'assert (out_good / "dir/ok.txt").read_bytes() == b"ok"',
    'for label, entries in [',
    '    ("traversal", [(tarfile.TarInfo("../escape.txt"), b"x")]),',
    '    ("absolute", [(tarfile.TarInfo("/abs.txt"), b"x")]),',
    ']:',
    '    entries[0][0].size = 1',
    '    bad_tar = work / f"{label}.tar.gz"',
    '    make_tar(bad_tar, entries)',
    '    out_bad = work / f"out-{label}"; out_bad.mkdir()',
    '    try:',
    '        with tarfile.open(bad_tar, "r:*") as tf: mod._unpack_legacy(tf, str(out_bad))',
    '        raise AssertionError(f"legacy unpack accepted {label}")',
    '    except RuntimeError:',
    '        pass',
    'sym = tarfile.TarInfo("sub"); sym.type = tarfile.SYMTYPE; sym.linkname = "real"',
    'child = tarfile.TarInfo("sub/pwn.txt"); child.size = 1',
    'real_dir = tarfile.TarInfo("real"); real_dir.type = tarfile.DIRTYPE',
    'sym_tar = work / "symparent.tar.gz"',
    'make_tar(sym_tar, [(real_dir, None), (sym, None), (child, b"x")])',
    'out_sym = work / "out-sym"; out_sym.mkdir()',
    'try:',
    '    with tarfile.open(sym_tar, "r:*") as tf: mod._unpack_legacy(tf, str(out_sym))',
    '    raise AssertionError("legacy unpack accepted member through symlink parent")',
    'except RuntimeError:',
    '    pass',
  ].join('\n') + '\n', 'utf8')

  await run(py.cmd, [...py.prefix, '-B', checker, join(repoRoot, 'scripts/install-runtime.py'), root], { windowsHide: true })
})

test('package.json and native/Cargo.toml share the exact version and rainy.nsi preserves hardening invariants', async () => {
  const pkg = JSON.parse(await readFile(join(repoRoot, 'package.json'), 'utf8'))
  const cargo = await readFile(join(repoRoot, 'native/Cargo.toml'), 'utf8')
  const section = cargo.split(/^\[/mu).find(part => part.startsWith('workspace.package]'))
  const cargoVersion = section?.match(/^version\s*=\s*"([^"]+)"/mu)?.[1]
  assert.equal(pkg.version, cargoVersion)

  const nsi = await readFile(join(repoRoot, 'native/installer/rainy.nsi'), 'utf8')
  const sectionBody = nsi.slice(nsi.indexOf('Section "-Rainy"'))
  const reparseAt = sectionBody.indexOf('!insertmacro rainyRejectReparse "$INSTDIR"')
  const existsAt = sectionBody.indexOf('${If} ${FileExists} "$INSTDIR\\*.*"')
  assert(reparseAt > 0 && existsAt > reparseAt, 'rainyRejectReparse must run unconditionally before checking $INSTDIR contents')
  assert.match(nsi, /!macro rainyPickLanguage[\s\S]*?!ifdef QA[\s\S]*?!macroend/u)
  assert.match(nsi, /EnumRegKey \$R1 HKCU "Software\\Classes"[\s\S]*?"Rainy\."/u)
  assert.match(nsi, /\.rainy-prev/u)
})
