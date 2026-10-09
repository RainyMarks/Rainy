"""Assemble the pinned Windows Strata runtime from explicit local inputs, excluding user data and weights."""
import argparse
import gzip
import hashlib
import json
from pathlib import Path, PurePosixPath
import shutil
import tarfile
import zipfile


def digest(path):
    """Hash a regular file without retaining its bytes in memory."""
    value = hashlib.sha256()
    with path.open('rb') as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b''):
            value.update(chunk)
    return value.hexdigest()


def relative_path(value):
    """Reject absolute paths, alternate separators and parent traversal."""
    path = PurePosixPath(value)
    if not value or value.startswith('/') or '\\' in value or ':' in value or any(part in {'', '.', '..'} for part in value.split('/')):
        raise ValueError('Unsafe runtime path: ' + str(value))
    return path


def source_file(root, name, expected):
    """Match every selected source file to its fixed byte count and digest."""
    path = root.joinpath(*relative_path(name).parts)
    if path.is_symlink() or not path.is_file() or not path.resolve().is_relative_to(root.resolve()):
        raise ValueError('Runtime input is not a contained regular file: ' + name)
    if path.stat().st_size != expected['bytes'] or digest(path) != expected['sha256']:
        raise ValueError('Runtime input checksum differs: ' + name)
    return path


def write_bytes(output, name, data):
    """Write one new output file; overlapping selections fail before replacement."""
    target = output.joinpath(*relative_path(name).parts)
    target.parent.mkdir(parents=True, exist_ok=True)
    with target.open('xb') as stream:
        stream.write(data)


def main():
    """Prepare one fresh runtime directory and an optional deterministic gzip tar archive."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--source', required=True, help='Pinned Strata source and engine directory')
    parser.add_argument('--python-root', required=True, help='Prepared Windows basic component python directory')
    parser.add_argument('--science-python-root', required=True, help='Prepared Windows CPU component python directory')
    parser.add_argument('--wheels-dir', required=True, help='Directory containing the pinned dependency wheels')
    parser.add_argument('--output', required=True, help='New runtime directory')
    parser.add_argument('--manifest', default=str(Path(__file__).resolve().parent.parent / 'toolpacks/strata-runtime.sources.json'))
    parser.add_argument('--archive', help='Optional new gzip tar archive, with runtime-relative members')
    options = parser.parse_args()
    manifest_path = Path(options.manifest).resolve()
    source = json.loads(manifest_path.read_text(encoding='utf-8'))
    if source['version'] != 1 or source['platform'] != 'win32' or source['architecture'] != 'x64':
        raise ValueError('Unsupported Strata runtime source definition')
    roots = {'strata': Path(options.source).resolve(), 'python': Path(options.python_root).resolve(),
             'science-python': Path(options.science_python_root).resolve()}
    wheels = Path(options.wheels_dir).resolve()
    output = Path(options.output).resolve()
    output.mkdir(parents=True, exist_ok=False)
    for entry in source['inputs']:
        original = source_file(roots[entry['root']], entry['source'], entry)
        target = output.joinpath(*relative_path(entry['path']).parts)
        target.parent.mkdir(parents=True, exist_ok=True)
        with original.open('rb') as incoming, target.open('xb') as outgoing:
            shutil.copyfileobj(incoming, outgoing, 1024 * 1024)
    for wheel in source['wheels']:
        original = source_file(wheels, wheel['file'], wheel)
        with zipfile.ZipFile(original) as archive:
            for member in wheel['members']:
                relative_path(member['member'])
                info = archive.getinfo(member['member'])
                if info.is_dir() or info.file_size != member['bytes']:
                    raise ValueError('Wheel member differs: ' + member['member'])
                target = output.joinpath(*relative_path(member['path']).parts)
                target.parent.mkdir(parents=True, exist_ok=True)
                value = hashlib.sha256()
                with archive.open(info) as incoming, target.open('xb') as outgoing:
                    for chunk in iter(lambda: incoming.read(1024 * 1024), b''):
                        outgoing.write(chunk)
                        value.update(chunk)
                if value.hexdigest() != member['sha256']:
                    raise ValueError('Wheel member checksum differs: ' + member['member'])
    for entry in source['generated']:
        write_bytes(output, entry['path'], entry['text'].encode('utf-8'))
    files = []
    for path in sorted(output.rglob('*')):
        if path.is_symlink():
            raise ValueError('Runtime output contains a symbolic link')
        if path.is_file():
            files.append({'path': path.relative_to(output).as_posix(), 'bytes': path.stat().st_size, 'sha256': digest(path)})
    record = {'version': 1, 'platform': source['platform'], 'architecture': source['architecture'],
              'strataVersion': source['strataVersion'], 'pythonVersion': source['pythonVersion'],
              'sourceSha256': digest(manifest_path), 'preparerSha256': digest(Path(__file__)), 'files': files}
    write_bytes(output, 'runtime-manifest.json', (json.dumps(record, ensure_ascii=False, indent=2) + '\n').encode('utf-8'))
    report = {'files': len(files), 'bytes': sum(row['bytes'] for row in files), 'strataVersion': source['strataVersion'],
              'pythonVersion': source['pythonVersion'], 'sourceSha256': record['sourceSha256']}
    if options.archive:
        destination = Path(options.archive).resolve()
        destination.parent.mkdir(parents=True, exist_ok=True)
        with destination.open('xb') as target, gzip.GzipFile(filename='', mode='wb', fileobj=target, mtime=0, compresslevel=6) as compressed:
            with tarfile.open(fileobj=compressed, mode='w') as archive:
                for path in sorted(output.rglob('*')):
                    if not path.is_file():
                        continue
                    info = archive.gettarinfo(str(path), arcname=path.relative_to(output).as_posix())
                    info.uid = info.gid = info.mtime = 0
                    info.uname = info.gname = ''
                    info.mode = 0o755 if path.suffix == '.exe' else 0o644
                    with path.open('rb') as stream:
                        archive.addfile(info, stream)
        report['archive'] = {'file': destination.name, 'bytes': destination.stat().st_size, 'sha256': digest(destination)}
    print(json.dumps(report))


if __name__ == '__main__':
    main()
