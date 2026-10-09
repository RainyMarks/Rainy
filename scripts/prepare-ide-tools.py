"""Prepare publisher-pinned IDE helpers; source configuration stays separate from generated files."""
import argparse
import hashlib
import json
from pathlib import Path, PurePosixPath
import shutil
import tarfile
import urllib.request
import zipfile

parser = argparse.ArgumentParser()
parser.add_argument('--offline', action='store_true')
parser.add_argument('--verify', action='store_true')
options = parser.parse_args()
app = Path(__file__).resolve().parent.parent
sources_path = app / 'toolpacks/ide-tools.sources.json'
sources = json.loads(sources_path.read_text(encoding='utf-8'))
output = app / 'resources/ide'
cache = app / 'toolpacks/cache/ide'


def digest(path):
    result = hashlib.sha256()
    with path.open('rb') as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b''):
            result.update(chunk)
    return result.hexdigest()


def cached_download(asset, extension):
    archive = cache / (asset['sha256'] + extension)
    if not archive.is_file() or archive.stat().st_size != asset['bytes'] or digest(archive) != asset['sha256']:
        if options.offline:
            raise RuntimeError('Pinned IDE resource is missing from the offline cache: ' + asset['id'])
        temporary = archive.with_suffix('.download')
        with urllib.request.urlopen(asset['url'], timeout=90) as response, temporary.open('wb') as target:
            shutil.copyfileobj(response, target)
        if temporary.stat().st_size != asset['bytes'] or digest(temporary) != asset['sha256']:
            raise RuntimeError('Publisher digest mismatch: ' + asset['id'])
        temporary.replace(archive)
    return archive


if options.verify:
    manifest = json.loads((output / 'manifest.json').read_text(encoding='utf-8'))
    if manifest['sourceSha256'] != digest(sources_path):
        raise RuntimeError('IDE helper source definition changed; prepare the helpers again')
    if manifest.get('preparerSha256') != digest(Path(__file__)) or manifest.get('installerSourceSha256') != digest(app / 'scripts/install-ide-system-packages.py'):
        raise RuntimeError('IDE helper preparation scripts changed; prepare the helpers again')
    if manifest.get('licenses') != sources['licenses']:
        raise RuntimeError('IDE license source definition changed; prepare the helpers again')
    recorded = {row['path']: row for row in manifest['files']}
    for license in sources['licenses']:
        row = recorded.get(license['path'])
        if row is None or row['bytes'] != license['bytes'] or row['sha256'] != license['sha256']:
            raise RuntimeError('Pinned IDE license is missing from the inventory: ' + license['path'])
    for row in manifest['files']:
        path = output / row['path']
        if path.stat().st_size != row['bytes'] or digest(path) != row['sha256']:
            raise RuntimeError('IDE helper inventory mismatch: ' + row['path'])
    print(json.dumps({'verified': len(manifest['files']), 'bytes': manifest['bytes']}))
    raise SystemExit(0)

output.mkdir(parents=True, exist_ok=True)
cache.mkdir(parents=True, exist_ok=True)
for asset in sources['assets']:
    archive = cached_download(asset, '.zip' if asset['format'] == 'zip' else '.tar.gz')
    destination = output / asset['destination']
    destination.mkdir(parents=True, exist_ok=True)
    if asset['format'] == 'zip':
        with zipfile.ZipFile(archive) as package:
            for entry in package.infolist():
                parts = PurePosixPath(entry.filename).parts
                if entry.filename.startswith('/') or '..' in parts or '\\' in entry.filename:
                    raise RuntimeError('Unsafe IDE helper archive member')
            package.extractall(destination)
    else:
        with tarfile.open(archive, 'r:gz') as package:
            package.extractall(destination, filter='data')
    print('Prepared ' + asset['id'], flush=True)

for license in sources['licenses']:
    parts = PurePosixPath(license['path']).parts
    if not parts or license['path'].startswith('/') or '..' in parts or '\\' in license['path']:
        raise RuntimeError('Unsafe IDE license destination')
    destination = output / license['path']
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(cached_download(license, '.txt'), destination)
    print('Prepared ' + license['id'], flush=True)

ruff = output / 'ruff-x86_64-unknown-linux-gnu/ruff'
if not ruff.is_file():
    raise RuntimeError('Ruff archive did not contain the expected executable')
(output / 'bin').mkdir(exist_ok=True)
shutil.copy2(ruff, output / 'bin/ruff')
(output / 'bin/ruff').chmod(0o755)
shutil.copy2(app / 'scripts/install-ide-system-packages.py', output / 'install-system-packages.py')
# The upstream standalone adapter is CommonJS and ships without a package scope.
(output / 'js-debug/package.json').write_text(json.dumps({'private': True, 'type': 'commonjs'}, indent=2) + '\n', encoding='utf-8')
required = ['python/debugpy/__init__.py', 'js-debug/src/dapDebugServer.js', 'js-debug/package.json', 'bin/ruff', 'install-system-packages.py',
            *[license['path'] for license in sources['licenses']]]
for name in required:
    if not (output / name).is_file():
        raise RuntimeError('IDE helper entry is missing: ' + name)
files = []
for path in sorted(output.rglob('*')):
    if path.is_file() and path.name != 'manifest.json' and 'system-packages' not in path.relative_to(output).parts:
        files.append({'path': path.relative_to(output).as_posix(), 'bytes': path.stat().st_size, 'sha256': digest(path)})
manifest = {'version': 1, 'sourceSha256': digest(sources_path), 'preparerSha256': digest(Path(__file__)),
            'installerSourceSha256': digest(app / 'scripts/install-ide-system-packages.py'), 'assets': sources['assets'], 'licenses': sources['licenses'],
            'required': required, 'files': files, 'bytes': sum(row['bytes'] for row in files)}
(output / 'manifest.json').write_text(json.dumps(manifest, indent=2) + '\n', encoding='utf-8')
print(json.dumps({'files': len(files), 'bytes': manifest['bytes']}))
