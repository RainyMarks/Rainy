"""Prepare the publisher-pinned IDE tools of one platform from `toolpacks/ide-tools.sources.json` (version 2).

`--platform linux-x64` fills `resources/ide`, which the Linux runtime base carries; its `system-packages/` (the offline
Ubuntu packages, `prepare-ide-system-packages.py`) is kept. `--platform win32-x64` fills `runtime/ide-windows`, which
`stage-windows.mjs` puts into the Windows Host. The tools: Ruff (Python formatting), ty (Python language service),
TypeScript 7's native compiler (JavaScript and TypeScript language service and formatting), debugpy and js-debug (the
debug adapters the user's Python and Node.js run).

`--offline` uses only the download cache; `--verify` checks an earlier preparation instead of preparing.
"""
import argparse
import hashlib
import json
from pathlib import Path, PurePosixPath
import shutil
import tarfile
import tempfile
import zipfile

app = Path(__file__).resolve().parent.parent
DEFAULT_OUTPUT = {'linux-x64': app / 'resources/ide', 'win32-x64': app / 'runtime/ide-windows'}
parser = argparse.ArgumentParser()
parser.add_argument('--platform', required=True, choices=sorted(DEFAULT_OUTPUT))
parser.add_argument('--output')
parser.add_argument('--offline', action='store_true')
parser.add_argument('--verify', action='store_true')
options = parser.parse_args()
sources_path = app / 'toolpacks/ide-tools.sources.json'
sources = json.loads(sources_path.read_text(encoding='utf-8'))
if sources.get('version') != 2 or options.platform not in sources['platforms']:
    raise RuntimeError('toolpacks/ide-tools.sources.json is not a version 2 source list for ' + options.platform)
linux = options.platform == 'linux-x64'
output = Path(options.output).resolve() if options.output else DEFAULT_OUTPUT[options.platform]
cache = app / 'toolpacks/cache/ide'
installer = app / 'scripts/install-ide-system-packages.py'
assets = [asset for asset in sources['assets'] if asset['platform'] in ('any', options.platform)]
exe = '' if linux else '.exe'
required = ['python/debugpy/__init__.py', 'js-debug/src/dapDebugServer.js', 'js-debug/package.json', f'bin/ruff{exe}', f'bin/ty{exe}',
            f'typescript/lib/tsc{exe}', *(['install-system-packages.py'] if linux else []), *[license['path'] for license in sources['licenses']]]
# debugpy is one wheel for every platform; its sources, debug symbols and other platforms' binaries are never loaded.
PRUNED = {'.c', '.cpp', '.h', '.hpp', '.pyx', '.pxd', '.pdb', '.dylib', *({'.dll', '.exe', '.pyd'} if linux else {'.so'})}


def digest(path):
    result = hashlib.sha256()
    with path.open('rb') as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b''):
            result.update(chunk)
    return result.hexdigest()


def safe(relative):
    parts = PurePosixPath(relative).parts
    if relative.startswith('/') or '\\' in relative or '..' in parts:
        raise RuntimeError('Unsafe IDE tool path: ' + relative)
    return relative


def manifest_base():
    return {'version': 2, 'platform': options.platform, 'sourceSha256': digest(sources_path), 'preparerSha256': digest(Path(__file__)),
            'installerSourceSha256': digest(installer), 'assets': assets, 'licenses': sources['licenses'], 'required': required}


def inventory():
    files = []
    for path in sorted(output.rglob('*')):
        relative = path.relative_to(output)
        if path.is_file() and relative.as_posix() != 'manifest.json' and relative.parts[0] != 'system-packages':
            files.append({'path': relative.as_posix(), 'bytes': path.stat().st_size, 'sha256': digest(path)})
    return files


if options.verify:
    manifest = json.loads((output / 'manifest.json').read_text(encoding='utf-8'))
    expected = manifest_base()
    for key, value in expected.items():
        if manifest.get(key) != value:
            raise RuntimeError(f'The IDE tools in {output} were prepared from another definition ({key}); prepare them again')
    if manifest['files'] != inventory():
        raise RuntimeError(f'The IDE tools in {output} differ from their inventory; prepare them again')
    print(json.dumps({'verified': len(manifest['files']), 'bytes': manifest['bytes']}))
    raise SystemExit(0)


def cached(asset, extension):
    archive = cache / (asset['sha256'] + extension)
    if not archive.is_file() or archive.stat().st_size != asset['bytes'] or digest(archive) != asset['sha256']:
        if options.offline:
            raise RuntimeError('A pinned IDE tool is missing from the offline cache: ' + asset['id'])
        import urllib.request
        temporary = archive.with_suffix('.download')
        with urllib.request.urlopen(asset['url'], timeout=300) as response, temporary.open('wb') as target:
            shutil.copyfileobj(response, target)
        if temporary.stat().st_size != asset['bytes'] or digest(temporary) != asset['sha256']:
            raise RuntimeError('Publisher digest mismatch: ' + asset['id'])
        temporary.replace(archive)
    return archive


def unpack(asset, archive, target):
    if asset['format'] == 'zip':
        with zipfile.ZipFile(archive) as package:
            for entry in package.infolist():
                safe(entry.filename)
            package.extractall(target)
    else:
        with tarfile.open(archive, 'r:gz') as package:
            package.extractall(target, filter='data')


cache.mkdir(parents=True, exist_ok=True)
output.mkdir(parents=True, exist_ok=True)
# A fresh tree, so a tool this version dropped does not linger; the offline Ubuntu packages are prepared separately.
for entry in output.iterdir():
    if entry.name != 'system-packages':
        shutil.rmtree(entry) if entry.is_dir() and not entry.is_symlink() else entry.unlink()
with tempfile.TemporaryDirectory(prefix='rainy-ide-') as scratch:
    for index, asset in enumerate(assets):
        unpacked = Path(scratch) / str(index)
        unpack(asset, cached(asset, '.zip' if asset['format'] == 'zip' else '.tar.gz'), unpacked)
        for place in asset['place']:
            source = unpacked / safe(place['from']) if place['from'] else unpacked
            destination = output / safe(place['to'])
            destination.parent.mkdir(parents=True, exist_ok=True)
            if source.is_dir():
                shutil.copytree(source, destination, dirs_exist_ok=True)
            elif source.is_file():
                shutil.copy2(source, destination)
            else:
                raise RuntimeError(f'{asset["id"]} {asset["version"]} has no {place["from"]}')
        print(f'Prepared {asset["id"]} {asset["version"]}', flush=True)
for path in list((output / 'python').rglob('*')):
    if path.is_file() and path.suffix in PRUNED:
        path.unlink()
for license in sources['licenses']:
    destination = output / safe(license['path'])
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(cached(license, '.txt'), destination)
if linux:
    shutil.copy2(installer, output / 'install-system-packages.py')
    for executable in [*(output / 'bin').iterdir(), output / 'typescript/lib/tsc']:
        executable.chmod(0o755)
# The upstream standalone adapter is CommonJS and ships without a package scope.
(output / 'js-debug/package.json').write_bytes((json.dumps({'private': True, 'type': 'commonjs'}, indent=2) + '\n').encode())
for name in required:
    if not (output / name).is_file():
        raise RuntimeError('An IDE tool entry is missing: ' + name)
files = inventory()
manifest = {**manifest_base(), 'files': files, 'bytes': sum(row['bytes'] for row in files)}
(output / 'manifest.json').write_bytes((json.dumps(manifest, indent=2) + '\n').encode())
print(json.dumps({'platform': options.platform, 'files': len(files), 'bytes': manifest['bytes']}))
