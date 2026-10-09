"""Verify and unpack the downloaded Host into Rainy's private WSL directory.

`install-runtime.py ARCHIVE METADATA` installs this version's runtime.
`install-runtime.py --prune METADATA` removes the runtimes earlier versions left.
"""
import fcntl
import hashlib
import json
from pathlib import Path
import os
import re
import shutil
import sys
import tarfile
import tempfile

DIGEST = re.compile('[0-9a-f]{64}')


def runtime_digest(metadata_path):
    digest = json.loads(Path(metadata_path).read_text())['sha256']
    if not DIGEST.fullmatch(digest):
        raise RuntimeError('Invalid Rainy runtime digest')
    return digest


def running_runtimes(owned):
    """Runtime directories a live process runs from, by its executable, working directory or arguments."""
    names = set()
    prefix = str(owned) + '/'
    for pid in os.listdir('/proc'):
        if not pid.isdigit():
            continue
        paths = []
        for link in ('exe', 'cwd'):
            try:
                paths.append(os.readlink(f'/proc/{pid}/{link}'))
            except OSError:
                pass
        try:
            paths.extend(Path(f'/proc/{pid}/cmdline').read_bytes().decode(errors='replace').split('\0'))
        except OSError:
            pass
        for path in paths:
            if path.startswith(prefix):
                names.add(path[len(prefix):].split('/', 1)[0])
    return names


def prune(owned, current):
    """Move every complete earlier runtime and abandoned staging directory aside under the lock, then delete them.
    A runtime a live process uses stays; a busy lock means another start is installing, so nothing is pruned now."""
    removed = []
    with (owned / '.install.lock').open('a') as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            return removed
        busy = running_runtimes(owned)
        for entry in owned.iterdir():
            name = entry.name
            if entry.is_symlink() or not entry.is_dir() or name in busy or name == current:
                continue
            earlier = DIGEST.fullmatch(name) and (entry / '.complete').is_file()
            if earlier or name.startswith('.install-'):
                entry.rename(Path(tempfile.mkdtemp(prefix='.trash-', dir=owned)) / name)
                removed.append(name)
    # Deleting hundreds of megabytes per runtime happens outside the lock, so a new start is not held up.
    for trash in owned.glob('.trash-*'):
        if trash.is_dir() and not trash.is_symlink():
            shutil.rmtree(trash, ignore_errors=True)
    return removed


def install(owned, archive, metadata_path):
    metadata = json.loads(Path(metadata_path).read_text())
    digest = runtime_digest(metadata_path)
    target = owned / digest
    with (owned / '.install.lock').open('a') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        if not (target / '.complete').is_file():
            if target.exists():
                raise RuntimeError('Incomplete runtime directory; preserved for diagnosis')
            # The carrier downloads the archive only when this version's runtime is not installed yet;
            # an archive left by an earlier version is replaced the same way.
            if not archive.is_file() or archive.stat().st_size != metadata['bytes'] or (
                    hashlib.sha256(archive.read_bytes()).hexdigest() != digest):
                return {'needsArchive': True}
            staging = Path(tempfile.mkdtemp(prefix='.install-', dir=owned))
            with tarfile.open(archive, 'r:gz') as package:
                package.extractall(staging, filter='data')
            if not (staging / 'node/bin/node').is_file() or not (staging / 'app/dist/host.js').is_file():
                raise RuntimeError('Rainy runtime is missing its executable')
            (staging / '.complete').write_text(digest + '\n')
            staging.rename(target)
    return {'node': str(target / 'node/bin/node'), 'host': str(target / 'app/dist/host.js')}


owned = Path.home() / '.rainy-agent/runtime'
owned.mkdir(mode=0o700, parents=True, exist_ok=True)
if sys.argv[1] == '--prune':
    print(json.dumps({'removed': prune(owned, runtime_digest(sys.argv[2]))}))
else:
    print(json.dumps(install(owned, Path(sys.argv[1]).resolve(), sys.argv[2])))
