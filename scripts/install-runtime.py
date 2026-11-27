"""Verify and unpack the Host into Rainy's private directory on a WSL distribution or an SSH server.

The runtime comes in two layers. The base (`linux-runtime.tar.xz`: the IDE tools in `app/resources/ide`) is downloaded
and changes only when those change; it is identified by the digest of its content. The application layer
(`linux-app.tar.gz`: the Rust Host `rainy-host`, the viewer page and the small resources) is downloaded too, for each
desktop version, only when that version is not installed yet. A version installs as `<application digest>/`, which holds the application layer and links to `base-<id>/`, so
an update whose base did not change unpacks a few megabytes and downloads nothing.

`install-runtime.py [--home DIR] BASE APP METADATA` installs this version; BASE may be missing when the base is already installed,
and APP when this version is (`{"needsApp": true}` otherwise).
`install-runtime.py [--home DIR] --prune METADATA` removes what earlier versions left.
`install-runtime.py [--home DIR] --probe METADATA [--prune]` checks whether this version is already installed on an SSH server
without streaming any archive (`{"host": ...}` or `{"needsApp": true, "needsBase": bool}`), optionally pruning older
runtimes on the same connection when already installed.
`install-runtime.py [--home DIR] --fetch METADATA PIECES [--prune]` installs on an SSH server, the application layer arriving on
standard input and the base downloaded there when needed.
`install-runtime.py [--home DIR] --receive METADATA [--prune]` installs on an SSH server from the application layer followed by the
base archive on standard input.

`--home DIR` (or the `RAINY_HOME` environment variable) overrides the default `~/.rainy-agent` parent of `runtime/`.

METADATA is a path to `linux-runtime.json`, or its JSON text (the SSH modes, which pass every argument inline):
{sha256, bytes} of the base archive, `content` (the base's identity), `app: {sha256, bytes}`, and the Rainy version.
PIECES is a JSON list of `{url, bytes, sha256}` whose concatenation is the base archive.
"""
try:
    import fcntl
except ImportError:
    fcntl = None
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
from urllib.parse import urlsplit

DIGEST = re.compile('[0-9a-f]{64}')
CHUNK = 1024 * 1024
# A server that downloads slower than this after a minute leaves the transfer to the desktop's upload.
SLOW_BYTES_PER_SECOND = 256 * 1024
SLOW_AFTER_SECONDS = 60
# The parts of the runtime that belong to the base: the IDE tools.
BASE_IN_APP = ('app/resources/ide',)


def in_app_layer(relative):
    """Whether a path of the runtime (relative, with `/`) belongs to the application layer rather than the base."""
    return not any(relative == kept or relative.startswith(kept + '/') for kept in BASE_IN_APP)


def in_base(relative):
    """Whether a path belongs to the base, or is a directory on the way to a base part under `app/`."""
    return not in_app_layer(relative) or any(kept == relative or kept.startswith(relative + '/') for kept in BASE_IN_APP)


def read_metadata(value):
    metadata = json.loads(value if value.lstrip().startswith('{') else Path(value).read_text(encoding='utf-8'))
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
            digest.update(entry.encode('utf-8') + b'\n')
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
            paths.extend(Path(f'/proc/{pid}/cmdline').read_bytes().decode('utf-8', errors='replace').split('\0'))
        except OSError:
            pass
        for path in paths:
            if path.startswith(prefix):
                names.add(path[len(prefix):].split('/', 1)[0])
    return names


def base_of(runtime):
    """The base a layered runtime links to (through its IDE tools, or its `node` in Rainy 3.6-4.0), or None."""
    for link in (runtime / BASE_IN_APP[0], runtime / 'node'):
        if link.is_symlink():
            return next((part for part in Path(os.readlink(link)).parts if part.startswith('base-')), None)
    return None


def prune(owned, metadata):
    """Move every complete earlier runtime, unused base and abandoned staging directory aside under the lock, then delete
    them. A runtime a live process uses stays with its base; a busy lock means another start is installing, so nothing
    is pruned now."""
    removed = []
    current = metadata['app']['sha256']
    with (owned / '.install.lock').open('a', encoding='utf-8') as lock:
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
            raise RuntimeError('This Python has no lzma module and xz failed: ' + result.stderr.decode('utf-8', errors='replace').strip())
        unpack(plain, destination)
    finally:
        shutil.rmtree(plain.parent, ignore_errors=True)


def _unpack_legacy(package, destination):
    """Manual equivalent of tarfile's `data` filter for Python versions before 3.12."""
    root = os.path.realpath(destination)
    members = package.getmembers()
    symlinks = set()
    for member in members:
        if member.name.startswith(('/', '\\')) or '..' in Path(member.name).parts:
            raise RuntimeError('Rainy runtime archive has an unsafe entry: ' + member.name)
        if member.isdev() or member.isfifo():
            raise RuntimeError('Rainy runtime archive has an unsafe entry: ' + member.name)
        target = os.path.realpath(os.path.join(destination, member.name))
        if not (target == root or target.startswith(root + os.sep)):
            raise RuntimeError('Rainy runtime archive has an unsafe entry: ' + member.name)
        # Reject entries traversing through a symlink created earlier in the same archive.
        parent = os.path.dirname(target)
        while parent != root and parent.startswith(root + os.sep):
            if parent in symlinks:
                raise RuntimeError('Rainy runtime archive has an unsafe symlink parent: ' + member.name)
            parent = os.path.dirname(parent)
        if member.issym() or member.islnk():
            link = os.path.realpath(os.path.join(os.path.dirname(target), member.linkname) if member.issym()
                                    else os.path.join(destination, member.linkname))
            if not link.startswith(root + os.sep) or link in symlinks:
                raise RuntimeError('Rainy runtime archive has an unsafe link: ' + member.name)
            if member.issym():
                symlinks.add(target)
        member.mode &= 0o777
        member.uid = 0
        member.gid = 0
        member.uname = ''
        member.gname = ''
    package.extractall(destination, members=members)


def unpack(archive, destination):
    """Unpack with the `data` filter; Pythons before it (3.8 on older servers) get equivalent safety checks by hand."""
    with tarfile.open(archive, 'r:*') as package:
        if hasattr(tarfile, 'data_filter'):
            package.extractall(destination, filter='data')
            return
        _unpack_legacy(package, destination)


def validate_piece_url(url):
    """Allow only HTTPS URLs or loopback HTTP URLs (used by local tests); refuse file:// and other schemes."""
    parsed = urlsplit(str(url))
    if parsed.username or parsed.password:
        raise RuntimeError('Invalid download URL: credentials in URL are not allowed')
    if parsed.scheme == 'https' and parsed.hostname:
        return url
    if parsed.scheme == 'http' and parsed.hostname in ('127.0.0.1', 'localhost', '::1'):
        return url
    raise RuntimeError(f'Unsupported download URL scheme or host: {parsed.scheme}://{parsed.hostname or ""}')


def installed(target):
    return {'host': str(target / 'rainy-host')}


def matches(archive, expected):
    return archive is not None and archive.is_file() and archive.stat().st_size == expected['bytes'] and file_digest(archive) == expected['sha256']


def install(owned, base_archive, app_archive, metadata):
    app = metadata['app']
    identity = metadata['content']
    target = owned / app['sha256']
    base = owned / ('base-' + identity)
    with (owned / '.install.lock').open('a', encoding='utf-8') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        if (target / '.complete').is_file():
            return installed(target)
        if target.exists():
            raise RuntimeError('Incomplete runtime directory; preserved for diagnosis')
        if not app_archive or not app_archive.is_file():
            # The carrier downloads the application layer only when this version is not installed.
            return {'needsApp': True}
        if not matches(app_archive, app):
            raise RuntimeError('The Rainy application files do not match this version')
        if not (base / '.complete').is_file():
            if base.exists():
                base.rename(Path(tempfile.mkdtemp(prefix='.trash-', dir=owned)) / base.name)
            if matches(base_archive, metadata):
                staging = Path(tempfile.mkdtemp(prefix='.install-', dir=owned))
                try:
                    extract(base_archive, staging)
                    if not (staging / BASE_IN_APP[0] / 'manifest.json').is_file():
                        raise RuntimeError('Rainy runtime is missing its IDE tools')
                    (staging / '.complete').write_text(identity + '\n', encoding='utf-8')
                    staging.rename(base)
                except Exception:
                    shutil.rmtree(staging, ignore_errors=True)
                    raise
            else:
                # The carrier downloads the base only when no installed one has this content.
                return {'needsArchive': True}
        staging = Path(tempfile.mkdtemp(prefix='.install-', dir=owned))
        try:
            extract(app_archive, staging)
            if not (staging / 'rainy-host').is_file():
                raise RuntimeError('Rainy runtime is missing its Host')
            # The application layer links to the base parts.
            for kept in BASE_IN_APP:
                if (base / kept).exists() or (base / kept).is_symlink():
                    link = staging / kept
                    link.parent.mkdir(parents=True, exist_ok=True)
                    link.symlink_to('../' * len(kept.split('/')) + f'{base.name}/{kept}')
            (staging / '.complete').write_text(app['sha256'] + '\n', encoding='utf-8')
            staging.rename(target)
        except Exception:
            shutil.rmtree(staging, ignore_errors=True)
            raise
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
        url = validate_piece_url(piece['url'])
        expected_bytes = int(piece['bytes'])
        if expected_bytes < 0:
            raise RuntimeError('Invalid piece size')
        path = folder / f'{index:03d}'
        if not (path.is_file() and path.stat().st_size == expected_bytes and file_digest(path) == piece['sha256']):
            partial = folder / f'{index:03d}.part'
            digest = hashlib.sha256()
            written = 0
            try:
                with urlopen(Request(url, headers={'User-Agent': 'Rainy'}), timeout=60) as response, partial.open('wb') as file:
                    reported = done
                    for block in iter(lambda: response.read(CHUNK), b''):
                        written += len(block)
                        if written > expected_bytes:
                            raise RuntimeError('The downloaded Rainy runtime exceeds its declared size')
                        file.write(block)
                        digest.update(block)
                        fetched += len(block)
                        elapsed = time.monotonic() - started
                        if elapsed > SLOW_AFTER_SECONDS and fetched / elapsed < SLOW_BYTES_PER_SECOND:
                            raise RuntimeError(f'The download is too slow ({fetched // 1024 // int(elapsed)} KiB/s)')
                        if done + written - reported >= 8 * CHUNK:
                            reported = done + written
                            progress(reported, total)
                if written != expected_bytes or partial.stat().st_size != expected_bytes or digest.hexdigest() != piece['sha256']:
                    raise RuntimeError('The downloaded Rainy runtime does not match this version')
                partial.rename(path)
            except Exception:
                try:
                    partial.unlink()
                except FileNotFoundError:
                    pass
                raise
        done += expected_bytes
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


def probe(owned, metadata, do_prune=False):
    """Check whether this version is already installed on an SSH server without streaming any archive."""
    if platform.system() != 'Linux' or platform.machine() not in ('x86_64', 'amd64'):
        return {'unsupported': f'{platform.system()} {platform.machine()}'}
    target = owned / metadata['app']['sha256']
    base = owned / ('base-' + metadata['content'])
    with (owned / '.install.lock').open('a', encoding='utf-8') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        if (target / '.complete').is_file():
            result = installed(target)
        elif target.exists():
            raise RuntimeError('Incomplete runtime directory; preserved for diagnosis')
        else:
            return {'needsApp': True, 'needsBase': not (base / '.complete').is_file()}
    if do_prune:
        try:
            prune(owned, metadata)
        except Exception:
            pass
    return result


def remote(owned, mode, metadata, pieces, do_prune=False):
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
            # Cap reads to declared size + 1 byte so an oversized stream cannot exhaust disk before digest checking.
            read_exactly(sys.stdin.buffer, int(metadata['bytes']) + 1, base_archive)
        else:
            sys.stdin.buffer.read()
        if not matches(app_archive, metadata['app']):
            return {'needsArchive': True, 'error': 'the application layer did not arrive intact'}
        result = install(owned, base_archive, app_archive, metadata)
        if mode == '--receive' or 'needsArchive' not in result:
            if do_prune and 'host' in result:
                try:
                    prune(owned, metadata)
                except Exception:
                    pass
            return result
        if not pieces:
            return result
        try:
            download, base_archive = fetch(owned, metadata, pieces)
        except Exception as error:  # Any network failure leaves the upload from the desktop.
            return {'needsArchive': True, 'error': f'{type(error).__name__}: {error}'}
        try:
            result = install(owned, base_archive, app_archive, metadata)
        finally:
            shutil.rmtree(download, ignore_errors=True)
        if do_prune and 'host' in result:
            try:
                prune(owned, metadata)
            except Exception:
                pass
        return result
    finally:
        shutil.rmtree(folder, ignore_errors=True)


def resolve_rainy_home(home_arg=None):
    """Resolve Rainy's private directory from `--home`, `RAINY_HOME`, or `~/.rainy-agent`."""
    raw = home_arg if home_arg is not None else os.environ.get('RAINY_HOME')
    if raw is None or not raw.strip():
        return Path.home() / '.rainy-agent'
    text = raw.strip()
    if '\0' in text or '\r' in text or '\n' in text:
        raise RuntimeError('Invalid RAINY_HOME path: control characters are not allowed')
    if not text.startswith('/'):
        raise RuntimeError('Invalid RAINY_HOME path: must be an absolute Linux path')
    parts = [part for part in text.split('/') if part]
    if any(part == '..' for part in parts):
        raise RuntimeError('Invalid RAINY_HOME path: ".." segments are not allowed')
    cleaned = '/' + '/'.join(parts) if parts else '/'
    if cleaned == '/':
        raise RuntimeError('Invalid RAINY_HOME path: root directory is not allowed')
    return Path(cleaned)


def extract_home_arg(argv):
    """Strip an optional `--home PATH` or `--home=PATH` flag from argv and return `(home_arg, rest)`."""
    home_arg = None
    rest = []
    i = 0
    while i < len(argv):
        arg = argv[i]
        if arg == '--home':
            if i + 1 >= len(argv):
                raise RuntimeError('Missing value for --home')
            home_arg = argv[i + 1]
            i += 2
        elif arg.startswith('--home='):
            home_arg = arg[len('--home='):]
            i += 1
        else:
            rest.append(arg)
            i += 1
    return home_arg, rest


if __name__ == '__main__':
    home_arg, args = extract_home_arg(sys.argv[1:])
    if not args:
        raise RuntimeError('Missing install-runtime mode argument')
    rainy_home = resolve_rainy_home(home_arg)
    rainy_home.mkdir(mode=0o700, parents=True, exist_ok=True)
    if os.name == 'posix':
        try:
            os.chmod(rainy_home, 0o700)
        except OSError:
            pass
    owned = rainy_home / 'runtime'
    owned.mkdir(mode=0o700, exist_ok=True)
    if args[0] == '--prune':
        if len(args) < 2:
            raise RuntimeError('Usage: install-runtime.py --prune METADATA')
        print(json.dumps({'removed': prune(owned, read_metadata(args[1]))}))
    elif args[0] == '--probe':
        if len(args) < 2:
            raise RuntimeError('Usage: install-runtime.py --probe METADATA [--prune]')
        do_prune = '--prune' in args[2:]
        print(json.dumps(probe(owned, read_metadata(args[1]), do_prune)))
    elif args[0] in ('--fetch', '--receive'):
        if len(args) < 2:
            raise RuntimeError(f'Usage: install-runtime.py {args[0]} METADATA [PIECES] [--prune]')
        extra = [arg for arg in args[2:] if arg != '--prune']
        do_prune = '--prune' in args[2:]
        pieces = json.loads(extra[0]) if extra else []
        print(json.dumps(remote(owned, args[0], read_metadata(args[1]), pieces, do_prune)))
    else:
        if len(args) < 3:
            raise RuntimeError('Usage: install-runtime.py BASE APP METADATA')
        print(json.dumps(install(owned, Path(args[0]).resolve(), Path(args[1]).resolve(), read_metadata(args[2]))))
