"""Verify and import one release-approved Linux component under the current Rainy home."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import tarfile
import tempfile
import uuid
import re

parser = argparse.ArgumentParser()
parser.add_argument('archive')
parser.add_argument('descriptor')
parser.add_argument('--root', default=str(Path.home() / '.rainy-agent/components'))
args = parser.parse_args()
expected = json.loads(args.descriptor)
component_id = expected['id']
if expected.get('version') != 1 or expected.get('platform') != 'linux' or expected.get('architecture') != 'x64' or component_id not in ['linux-basic', 'linux-science-cpu', 'linux-science-cuda', 'linux-development']:
    raise RuntimeError('Unsupported Linux component')
if any(not isinstance(expected.get(key), str) or not re.fullmatch('[a-f0-9]{64}', expected[key]) for key in ['sha256', 'manifestSha256']):
    raise RuntimeError('Invalid component digest')
root = Path(args.root).resolve()
root.mkdir(parents=True, exist_ok=True)
archive = Path(args.archive)
def digest(path):
    checksum = hashlib.sha256()
    with path.open('rb') as source:
        for chunk in iter(lambda: source.read(8 * 1024 * 1024), b''):
            checksum.update(chunk)
    return checksum.hexdigest()
if archive.stat().st_size != expected['bytes'] or digest(archive) != expected['sha256']:
    raise RuntimeError('Offline component checksum mismatch')
def verify(directory):
    manifest_path = directory / 'component.json'
    if digest(manifest_path) != expected['manifestSha256']:
        raise RuntimeError('Component inventory does not match the release')
    manifest = json.loads(manifest_path.read_text(encoding='utf-8'))
    if manifest.get('id') != component_id or manifest.get('platform') != 'linux' or manifest.get('unpackedBytes') != expected['unpackedBytes']:
        raise RuntimeError('Component inventory identity differs from the release')
    known = {'component.json'}
    for entry in manifest['files']:
        parts = Path(entry['path']).parts
        if not parts or Path(entry['path']).is_absolute() or '..' in parts or entry['path'] in known:
            raise RuntimeError('Invalid component inventory path')
        known.add(entry['path'])
        path = directory / entry['path']
        if not path.resolve().is_relative_to(directory):
            raise RuntimeError('Component entry points outside its directory')
        if 'link' in entry:
            if not path.is_symlink() or os.readlink(path) != entry['link']:
                raise RuntimeError('Component link differs from its inventory')
        elif not path.is_file() or path.is_symlink() or path.stat().st_size != entry['bytes'] or digest(path) != entry['sha256']:
            raise RuntimeError('Component file differs: ' + entry['path'])
    for path in directory.rglob('*'):
        if not path.is_dir() and path.relative_to(directory).as_posix() not in known:
            raise RuntimeError('Unlisted component entry')

destination = root / component_id / expected['sha256']
staging = None
try:
    destination.parent.mkdir(parents=True, exist_ok=True)
    if destination.exists():
        verify(destination)
    else:
        required = expected['unpackedBytes'] + max(32 * 1024 * 1024, expected['unpackedBytes'] // 20)
        if shutil.disk_usage(root).free < required:
            raise RuntimeError('Insufficient free space for verified component extraction')
        staging = Path(tempfile.mkdtemp(prefix='.component-', dir=root))
        with tarfile.open(archive) as package:
            package.extractall(staging, filter='data')
        verify(staging)
        staging.rename(destination)
    active_path = root / 'active.json'
    active = json.loads(active_path.read_text(encoding='utf-8')) if active_path.is_file() else {}
    active[component_id] = destination.relative_to(root).as_posix()
    pending = root / ('.active-' + str(uuid.uuid4()) + '.json')
    with pending.open('x', encoding='utf-8') as target:
        json.dump(active, target, indent=2)
        target.write('\n'); target.flush(); os.fsync(target.fileno())
    pending.replace(active_path)
    print(json.dumps({'installed': component_id, 'path': str(destination)}))
finally:
    if staging is not None and staging.exists():
        if not staging.resolve().is_relative_to(root) or staging.parent != root:
            raise RuntimeError('Refusing cleanup outside component staging')
        shutil.rmtree(staging)
