"""Verify and unpack the Host into Rainy's private directory on a WSL distribution or an SSH server.

The runtime comes in two layers. The base (`linux-runtime.tar.xz`: Node.js, the dependency packages, the IDE resources
and ripgrep) is downloaded and changes only when those change; it is identified by the digest of its content. The
application layer (`linux-app.tar.gz`: the Host, the interface and the small resources) ships with every desktop
version. A version installs as `<application digest>/`, which holds the application layer and links to `base-<id>/`, so
an update whose base did not change unpacks a few megabytes and downloads nothing.

`install-runtime.py BASE APP METADATA` installs this version; BASE may be missing when the base is already installed.
`install-runtime.py --prune METADATA` removes what earlier versions left.
`install-runtime.py --fetch METADATA PIECES` installs on an SSH server, the application layer arriving on standard input and
the base downloaded there when needed.
`install-runtime.py --receive METADATA` installs on an SSH server from the application layer followed by the base archive on
standard input.

METADATA is a path to `linux-runtime.json`, or its JSON text (the SSH modes, which pass every argument inline):
{sha256, bytes} of the base archive, `content` (the base's identity), `app: {sha256, bytes}`, and the Node and Rainy versions.
PIECES is a JSON list of `{url, bytes, sha256}` whose concatenation is the base archive.
"""
import fcntl
import hashlib
import json
from pathlib import Path
import os
import platform
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import time

DIGEST = re.compile('[0-9a-f]{64}')
CHUNK = 1024 * 1024
# A server that downloads slower than this after a minute leaves the transfer to the desktop's upload.
SLOW_BYTES_PER_SECOND = 256 * 1024
SLOW_AFTER_SECONDS = 60
# Parts of `app/` that belong to the base: dependency links, the ripgrep link and the IDE resources.
BASE_IN_APP = ('app/node_modules', 'app/bin', 'app/resources/ide')


def in_app_layer(relative):
    """Whether a path of the runtime (relative, with `/`) belongs to the application layer rather than the base."""
    if relative == 'runtime.json':
        return True
    if relative != 'app' and not relative.startswith('app/'):
        return False
    return not any(relative == kept or relative.startswith(kept + '/') for kept in BASE_IN_APP)


def in_base(relative):
    """Whether a path belongs to the base, or is a directory on the way to a base part under `app/`."""
    return not in_app_layer(relative) or any(kept == relative or kept.startswith(relative + '/') for kept in BASE_IN_APP)


def read_metadata(value):
    metadata = json.loads(value if value.lstrip().startswith('{') else Path(value).read_text())
    for digest in (metadata.get('sha256', ''), metadata.get('content', ''), metadata.get('app', {}).get('sha256', '')):
        if not DIGEST.fullmatch(digest):
            raise RuntimeError('Invalid Rainy runtime digest')
    return metadata


def file_digest(path):
    digest = hashlib.sha256()
    with path.open('rb') as file:
        for block in iter(lambda: file.read(CHUNK), b''):
            digest.update(block)
    return digest.hexdigest()


def content_digest(root):
    """Identity of the base under `root`: every directory, file (content and executable bit) and link, in a fixed order.
    Times and owners are left out, so the same files give the same identity whichever build or archive they came from."""
    digest = hashlib.sha256()
    root = Path(root)
    for directory, directories, files in os.walk(root):
        directories.sort()
        here = Path(directory)
        relative_dir = here.relative_to(root).as_posix()
        entries = []
        for name in sorted(directories + files):
            path = here / name
            relative = name if relative_dir == '.' else f'{relative_dir}/{name}'
            if not in_base(relative) or relative == '.complete':
                continue
            if path.is_symlink():
                entries.append(f'L {relative} {os.readlink(path)}')
            elif path.is_dir():
                entries.append(f'D {relative}')
            else:
                executable = 'x' if path.stat().st_mode & 0o111 else '-'
                entries.append(f'F {relative} {executable} {file_digest(path)}')
        # Linked directories are recorded as links and not entered; application-layer directories are not entered.
        directories[:] = [name for name in directories if not (here / name).is_symlink()
                          and in_base(name if relative_dir == '.' else f'{relative_dir}/{name}')]
        for entry in entries:
            digest.update(entry.encode() + b'\n')
    return digest.hexdigest()


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


def base_of(runtime):
    """The base a layered runtime links to, or None for a runtime from before the layers."""
    node = runtime / 'node'
    if not node.is_symlink():
        return None
    return Path(os.readlink(node)).parts[1] if len(Path(os.readlink(node)).parts) > 1 else None


def prune(owned, metadata):
    """Move every complete earlier runtime, unused base and abandoned staging directory aside under the lock, then delete
    them. A runtime a live process uses stays with its base; a busy lock means another start is installing, so nothing
    is pruned now."""
    removed = []
    current = metadata['app']['sha256']
    with (owned / '.install.lock').open('a') as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            return removed
        busy = running_runtimes(owned)
        keep = {current, 'base-' + metadata['content']} | busy
        for name in list(busy):
            base = base_of(owned / name) if DIGEST.fullmatch(name) else None
            if base:
                keep.add(base)
        for entry in owned.iterdir():
            name = entry.name
            if entry.is_symlink() or not entry.is_dir() or name in keep:
                continue
            earlier = (DIGEST.fullmatch(name) or (name.startswith('base-') and DIGEST.fullmatch(name[5:]))) and (entry / '.complete').is_file()
            if earlier or name.startswith('.install-') or name.startswith('.download-'):
                entry.rename(Path(tempfile.mkdtemp(prefix='.trash-', dir=owned)) / name)
                removed.append(name)
    # Deleting hundreds of megabytes per runtime happens outside the lock, so a new start is not held up.
    for trash in owned.glob('.trash-*'):
        if trash.is_dir() and not trash.is_symlink():
            shutil.rmtree(trash, ignore_errors=True)
    return removed


XZ_MAGIC = b'\xfd7zXZ\x00'


def has_lzma():
    try:
        import lzma  # noqa: F401 -- Pythons built without liblzma lack the module.
        return True
    except ImportError:
        return False


def extract(archive, destination):
    """Unpack a gzip or xz archive (the base is xz, the application layer gzip). A Python without `lzma` hands an xz
    archive to the system's `xz`, which every distribution's package manager depends on."""
    with open(archive, 'rb') as stream:
        xz = stream.read(len(XZ_MAGIC)) == XZ_MAGIC
    if not xz or has_lzma():
        unpack(archive, destination)
        return
    plain = Path(tempfile.mkdtemp(prefix='.install-unxz-', dir=os.path.dirname(destination))) / 'base.tar'
    try:
        with plain.open('wb') as output:
            result = subprocess.run(['xz', '--decompress', '--stdout', str(archive)], stdout=output, stderr=subprocess.PIPE)
        if result.returncode:
            raise RuntimeError('This Python has no lzma module and xz failed: ' + result.stderr.decode(errors='replace').strip())
        unpack(plain, destination)
    finally:
        shutil.rmtree(plain.parent, ignore_errors=True)


def unpack(archive, destination):
    """Unpack with the `data` filter; Pythons before it (3.8 on older servers) get the same checks by hand."""
    with tarfile.open(archive, 'r:*') as package:
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


def matches(archive, expected):
    return archive is not None and archive.is_file() and archive.stat().st_size == expected['bytes'] and file_digest(archive) == expected['sha256']


def seed(owned, base, identity):
    """Reuse the base of a runtime an earlier version unpacked when its content is the same, by hard links, so the
    first update to layered runtimes downloads nothing either. Returns whether the base is now installed."""
    candidates = [entry for entry in owned.iterdir() if DIGEST.fullmatch(entry.name) and not entry.is_symlink()
                  and (entry / '.complete').is_file() and (entry / 'node').is_dir() and not (entry / 'node').is_symlink()]
    for candidate in sorted(candidates, key=lambda entry: entry.stat().st_mtime, reverse=True):
        if content_digest(candidate) != identity:
            continue
        staging = Path(tempfile.mkdtemp(prefix='.install-', dir=owned))
        os.rmdir(staging)

        def ignore(directory, names):
            relative_dir = Path(directory).relative_to(candidate).as_posix()
            return {name for name in names if not in_base(name if relative_dir == '.' else f'{relative_dir}/{name}')
                    or (relative_dir == '.' and name == '.complete')}
        shutil.copytree(candidate, staging, symlinks=True, copy_function=os.link, ignore=ignore)
        (staging / '.complete').write_text(identity + '\n')
        staging.rename(base)
        return True
    return False


def install(owned, base_archive, app_archive, metadata):
    app = metadata['app']
    identity = metadata['content']
    target = owned / app['sha256']
    base = owned / ('base-' + identity)
    with (owned / '.install.lock').open('a') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        if (target / '.complete').is_file():
            return installed(target)
        if target.exists():
            raise RuntimeError('Incomplete runtime directory; preserved for diagnosis')
        if not matches(app_archive, app):
            raise RuntimeError('The Rainy application files do not match this version')
        if not (base / '.complete').is_file():
            if base.exists():
                base.rename(Path(tempfile.mkdtemp(prefix='.trash-', dir=owned)) / base.name)
            if matches(base_archive, metadata):
                staging = Path(tempfile.mkdtemp(prefix='.install-', dir=owned))
                extract(base_archive, staging)
                if not (staging / 'node/bin/node').is_file():
                    raise RuntimeError('Rainy runtime is missing its executable')
                (staging / '.complete').write_text(identity + '\n')
                staging.rename(base)
            elif not seed(owned, base, identity):
                # The carrier downloads the base only when no installed one has this content.
                return {'needsArchive': True}
        staging = Path(tempfile.mkdtemp(prefix='.install-', dir=owned))
        extract(app_archive, staging)
        if not (staging / 'app/dist/host.js').is_file():
            raise RuntimeError('Rainy runtime is missing its Host')
        # The application layer links to the base: its top-level entries, and the base parts that live under app/.
        for entry in base.iterdir():
            if entry.name not in ('app', '.complete'):
                (staging / entry.name).symlink_to(f'../{base.name}/{entry.name}')
        for kept in BASE_IN_APP:
            if (base / kept).exists() or (base / kept).is_symlink():
                link = staging / kept
                link.parent.mkdir(parents=True, exist_ok=True)
                link.symlink_to('../' * len(kept.split('/')) + f'{base.name}/{kept}')
        (staging / '.complete').write_text(app['sha256'] + '\n')
        staging.rename(target)
    return installed(target)


def progress(done, total):
    sys.stderr.write(f'RAINY_PROGRESS {done} {total}\n')
    sys.stderr.flush()


def fetch(owned, metadata, pieces):
    """Download the base piece by piece; verified pieces are kept, so a dropped connection resumes from there."""
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
    archive = folder / 'linux-runtime.base'
    with archive.open('wb') as output:
        for index in range(len(pieces)):
            with (folder / f'{index:03d}').open('rb') as source:
                shutil.copyfileobj(source, output, CHUNK)
    return folder, archive


def read_exactly(stream, count, path):
    with path.open('wb') as output:
        left = count
        while left > 0:
            block = stream.read(min(CHUNK, left))
            if not block:
                break
            output.write(block)
            left -= len(block)


def remote(owned, mode, metadata, pieces):
    """Install on an SSH server. Standard input starts with the application layer; with --receive the base follows."""
    if platform.system() != 'Linux' or platform.machine() not in ('x86_64', 'amd64'):
        sys.stdin.buffer.read()
        return {'unsupported': f'{platform.system()} {platform.machine()}'}
    folder = Path(tempfile.mkdtemp(prefix='.download-', dir=owned))
    try:
        app_archive = folder / 'linux-app.tar.gz'
        read_exactly(sys.stdin.buffer, metadata['app']['bytes'], app_archive)
        base_archive = None
        if mode == '--receive':
            base_archive = folder / 'linux-runtime.base'
            with base_archive.open('wb') as output:
                shutil.copyfileobj(sys.stdin.buffer, output, CHUNK)
        else:
            sys.stdin.buffer.read()
        if not matches(app_archive, metadata['app']):
            return {'needsArchive': True, 'error': 'the application layer did not arrive intact'}
        result = install(owned, base_archive, app_archive, metadata)
        if mode == '--receive' or 'needsArchive' not in result:
            return result
        if not pieces:
            return result
        try:
            download, base_archive = fetch(owned, metadata, pieces)
        except Exception as error:  # Any network failure leaves the upload from the desktop.
            return {'needsArchive': True, 'error': f'{type(error).__name__}: {error}'}
        try:
            return install(owned, base_archive, app_archive, metadata)
        finally:
            shutil.rmtree(download, ignore_errors=True)
    finally:
        shutil.rmtree(folder, ignore_errors=True)


if __name__ == '__main__':
    owned = Path.home() / '.rainy-agent/runtime'
    owned.mkdir(mode=0o700, parents=True, exist_ok=True)
    if sys.argv[1] == '--prune':
        print(json.dumps({'removed': prune(owned, read_metadata(sys.argv[2]))}))
    elif sys.argv[1] in ('--fetch', '--receive'):
        print(json.dumps(remote(owned, sys.argv[1], read_metadata(sys.argv[2]), json.loads(sys.argv[3]) if len(sys.argv) > 3 else [])))
    else:
        print(json.dumps(install(owned, Path(sys.argv[1]).resolve(), Path(sys.argv[2]).resolve(), read_metadata(sys.argv[3]))))
