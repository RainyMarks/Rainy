"""Verify and import one release-approved Linux component under the current Rainy home."""
import argparse
try:
    import fcntl
except ImportError:
    fcntl = None
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import tarfile
import tempfile
import uuid

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


def is_within(path, directory):
    resolved_dir = os.path.realpath(str(directory))
    resolved_path = os.path.realpath(str(path))
    return resolved_path == resolved_dir or resolved_path.startswith(resolved_dir + os.sep)


def digest(path):
    checksum = hashlib.sha256()
    with path.open('rb') as source:
        for chunk in iter(lambda: source.read(8 * 1024 * 1024), b''):
            checksum.update(chunk)
    return checksum.hexdigest()


def safe_extract(archive_path, destination):
    with tarfile.open(archive_path) as package:
        if hasattr(tarfile, 'data_filter'):
            package.extractall(destination, filter='data')
            return
        dest_root = os.path.realpath(str(destination))
        members = package.getmembers()
        symlinks = set()
        for member in members:
            if member.name.startswith('/') or '..' in Path(member.name).parts or member.isdev() or member.isfifo():
                raise RuntimeError('Unsafe component archive entry: ' + member.name)
            target = os.path.realpath(os.path.join(dest_root, member.name))
            if not (target == dest_root or target.startswith(dest_root + os.sep)):
                raise RuntimeError('Unsafe component archive entry: ' + member.name)
            parent = os.path.dirname(target)
            while parent != dest_root and parent.startswith(dest_root + os.sep):
                if parent in symlinks:
                    raise RuntimeError('Unsafe component archive symlink parent: ' + member.name)
                parent = os.path.dirname(parent)
            if member.issym() or member.islnk():
                link = os.path.realpath(os.path.join(os.path.dirname(target), member.linkname) if member.issym()
                                        else os.path.join(dest_root, member.linkname))
                if not link.startswith(dest_root + os.sep) or link in symlinks:
                    raise RuntimeError('Unsafe component archive link: ' + member.name)
                if member.issym():
                    symlinks.add(target)
            member.mode &= 0o777
            member.uid = 0
            member.gid = 0
            member.uname = ''
            member.gname = ''
        package.extractall(destination, members=members)


if archive.stat().st_size != expected['bytes'] or digest(archive) != expected['sha256']:
    raise RuntimeError('Offline component checksum mismatch')


def verify(directory):
    manifest_path = directory / 'component.json'
    if not manifest_path.is_file() or digest(manifest_path) != expected['manifestSha256']:
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
        if not is_within(path, directory):
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
trash_dir = None
try:
    with (root / '.component.lock').open('a', encoding='utf-8') as lock:
        if fcntl is not None:
            fcntl.flock(lock, fcntl.LOCK_EX)
        destination.parent.mkdir(parents=True, exist_ok=True)
        need_extract = True
        if destination.exists():
            try:
                verify(destination)
                need_extract = False
            except Exception:
                # Self-heal a partially extracted or damaged component directory by moving it aside before re-importing.
                trash_dir = Path(tempfile.mkdtemp(prefix='.trash-', dir=root))
                destination.rename(trash_dir / destination.name)
        if need_extract:
            required = expected['unpackedBytes'] + max(32 * 1024 * 1024, expected['unpackedBytes'] // 20)
            if shutil.disk_usage(root).free < required:
                raise RuntimeError('Insufficient free space for verified component extraction')
            staging = Path(tempfile.mkdtemp(prefix='.component-', dir=root))
            safe_extract(archive, staging)
            verify(staging)
            staging.rename(destination)
        active_path = root / 'active.json'
        active = json.loads(active_path.read_text(encoding='utf-8')) if active_path.is_file() else {}
        active[component_id] = destination.relative_to(root).as_posix()
        pending = root / ('.active-' + str(uuid.uuid4()) + '.json')
        with pending.open('x', encoding='utf-8') as target:
            json.dump(active, target, indent=2)
            target.write('\n')
            target.flush()
            os.fsync(target.fileno())
        pending.replace(active_path)
    print(json.dumps({'installed': component_id, 'path': str(destination)}))
finally:
    if staging is not None and staging.exists():
        if not is_within(staging, root) or staging.parent != root:
            raise RuntimeError('Refusing cleanup outside component staging')
        shutil.rmtree(staging)
    if trash_dir is not None and trash_dir.exists():
        if is_within(trash_dir, root) and trash_dir.parent == root:
            shutil.rmtree(trash_dir, ignore_errors=True)
