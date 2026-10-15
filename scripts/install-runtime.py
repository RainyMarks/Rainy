"""Verify and unpack the downloaded Host into Rainy's private directory on a WSL distribution or an SSH server.

`install-runtime.py ARCHIVE METADATA` installs this version's runtime.
`install-runtime.py --prune METADATA` removes the runtimes earlier versions left.
`install-runtime.py --fetch METADATA PIECES` installs it on an SSH server, downloading the archive there when needed.
`install-runtime.py --receive METADATA` installs it from the archive sent on standard input.

METADATA is a path to `linux-runtime.json`, or its JSON text (the SSH modes, which pass every argument inline).
PIECES is a JSON list of `{url, bytes, sha256}` whose concatenation is the archive.
"""
import fcntl
import hashlib
import json
from pathlib import Path
import os
import platform
import re
import shutil
import sys
import tarfile
import tempfile
import time

DIGEST = re.compile('[0-9a-f]{64}')
CHUNK = 1024 * 1024
# A server that downloads slower than this after a minute leaves the transfer to the desktop's upload.
SLOW_BYTES_PER_SECOND = 256 * 1024
SLOW_AFTER_SECONDS = 60


def read_metadata(value):
    metadata = json.loads(value if value.lstrip().startswith('{') else Path(value).read_text())
    if not DIGEST.fullmatch(metadata.get('sha256', '')):
        raise RuntimeError('Invalid Rainy runtime digest')
    return metadata


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
            if earlier or name.startswith('.install-') or name.startswith('.download-'):
                entry.rename(Path(tempfile.mkdtemp(prefix='.trash-', dir=owned)) / name)
                removed.append(name)
    # Deleting hundreds of megabytes per runtime happens outside the lock, so a new start is not held up.
    for trash in owned.glob('.trash-*'):
        if trash.is_dir() and not trash.is_symlink():
            shutil.rmtree(trash, ignore_errors=True)
    return removed


def file_digest(path):
    digest = hashlib.sha256()
    with path.open('rb') as file:
        for block in iter(lambda: file.read(CHUNK), b''):
            digest.update(block)
    return digest.hexdigest()


def extract(archive, destination):
    """Unpack with the `data` filter; Pythons before it (3.8 on older servers) get the same checks by hand."""
    with tarfile.open(archive, 'r:gz') as package:
        if hasattr(tarfile, 'data_filter'):
            package.extractall(destination, filter='data')
            return
        root = os.path.realpath(destination)
        for member in package.getmembers():
            target = os.path.realpath(os.path.join(destination, member.name))
            if not (target == root or target.startswith(root + os.sep)) or member.isdev():
                raise RuntimeError('Rainy runtime archive has an unsafe entry: ' + member.name)
            if member.issym() or member.islnk():
                link = os.path.realpath(os.path.join(os.path.dirname(target), member.linkname) if member.issym()
                                        else os.path.join(destination, member.linkname))
                if not link.startswith(root + os.sep):
                    raise RuntimeError('Rainy runtime archive has an unsafe link: ' + member.name)
        package.extractall(destination)


def installed(target):
    return {'node': str(target / 'node/bin/node'), 'host': str(target / 'app/dist/host.js')}


def install(owned, archive, metadata):
    digest = metadata['sha256']
    target = owned / digest
    with (owned / '.install.lock').open('a') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        if not (target / '.complete').is_file():
            if target.exists():
                raise RuntimeError('Incomplete runtime directory; preserved for diagnosis')
            # The carrier downloads the archive only when this version's runtime is not installed yet;
            # an archive left by an earlier version is replaced the same way.
            if archive is None or not archive.is_file() or archive.stat().st_size != metadata['bytes'] or file_digest(archive) != digest:
                return {'needsArchive': True}
            staging = Path(tempfile.mkdtemp(prefix='.install-', dir=owned))
            extract(archive, staging)
            if not (staging / 'node/bin/node').is_file() or not (staging / 'app/dist/host.js').is_file():
                raise RuntimeError('Rainy runtime is missing its executable')
            (staging / '.complete').write_text(digest + '\n')
            staging.rename(target)
    return installed(target)


def progress(done, total):
    sys.stderr.write(f'RAINY_PROGRESS {done} {total}\n')
    sys.stderr.flush()


def fetch(owned, metadata, pieces):
    """Download the archive piece by piece; verified pieces are kept, so a dropped connection resumes from there."""
    from urllib.request import Request, urlopen
    folder = owned / f'.download-{metadata["sha256"]}'
    folder.mkdir(mode=0o700, exist_ok=True)
    total = sum(piece['bytes'] for piece in pieces)
    done = 0
    started, fetched = time.monotonic(), 0
    for index, piece in enumerate(pieces):
        path = folder / f'{index:03d}'
        if not (path.is_file() and path.stat().st_size == piece['bytes'] and file_digest(path) == piece['sha256']):
            partial = folder / f'{index:03d}.part'
            digest = hashlib.sha256()
            with urlopen(Request(piece['url'], headers={'User-Agent': 'Rainy'}), timeout=60) as response, partial.open('wb') as file:
                reported = done
                for block in iter(lambda: response.read(CHUNK), b''):
                    file.write(block)
                    digest.update(block)
                    fetched += len(block)
                    elapsed = time.monotonic() - started
                    if elapsed > SLOW_AFTER_SECONDS and fetched / elapsed < SLOW_BYTES_PER_SECOND:
                        raise RuntimeError(f'The download is too slow ({fetched // 1024 // int(elapsed)} KiB/s)')
                    if done + file.tell() - reported >= 8 * CHUNK:
                        reported = done + file.tell()
                        progress(reported, total)
            if partial.stat().st_size != piece['bytes'] or digest.hexdigest() != piece['sha256']:
                partial.unlink()
                raise RuntimeError('The downloaded Rainy runtime does not match this version')
            partial.rename(path)
        done += piece['bytes']
        progress(done, total)
    archive = folder / 'linux-runtime.tar.gz'
    with archive.open('wb') as output:
        for index in range(len(pieces)):
            with (folder / f'{index:03d}').open('rb') as source:
                shutil.copyfileobj(source, output, CHUNK)
    return folder, archive


def receive(owned, metadata):
    folder = Path(tempfile.mkdtemp(prefix='.download-', dir=owned))
    archive = folder / 'linux-runtime.tar.gz'
    with archive.open('wb') as output:
        shutil.copyfileobj(sys.stdin.buffer, output, CHUNK)
    return folder, archive


def remote(owned, mode, metadata, pieces):
    """Install on an SSH server: an installed runtime is used as it is; otherwise the archive is fetched or received."""
    if platform.system() != 'Linux' or platform.machine() not in ('x86_64', 'amd64'):
        return {'unsupported': f'{platform.system()} {platform.machine()}'}
    target = owned / metadata['sha256']
    if (target / '.complete').is_file():
        return installed(target)
    if mode == '--fetch' and not pieces:
        return {'needsArchive': True}
    try:
        folder, archive = fetch(owned, metadata, pieces) if mode == '--fetch' else receive(owned, metadata)
    except Exception as error:  # Any network failure leaves the upload from the desktop.
        return {'needsArchive': True, 'error': f'{type(error).__name__}: {error}'}
    try:
        return install(owned, archive, metadata)
    finally:
        shutil.rmtree(folder, ignore_errors=True)


owned = Path.home() / '.rainy-agent/runtime'
owned.mkdir(mode=0o700, parents=True, exist_ok=True)
if sys.argv[1] == '--prune':
    print(json.dumps({'removed': prune(owned, read_metadata(sys.argv[2])['sha256'])}))
elif sys.argv[1] in ('--fetch', '--receive'):
    print(json.dumps(remote(owned, sys.argv[1], read_metadata(sys.argv[2]), json.loads(sys.argv[3]) if len(sys.argv) > 3 else [])))
else:
    print(json.dumps(install(owned, Path(sys.argv[1]).resolve(), read_metadata(sys.argv[2]))))
