"""Verify and explicitly install the offline Ubuntu development layer in the selected distribution."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile

parser = argparse.ArgumentParser()
parser.add_argument('directory')
parser.add_argument('--verify-only', action='store_true')
options = parser.parse_args()
root = Path(options.directory).resolve()
manifest = json.loads((root / 'manifest.json').read_text())
release = dict(line.split('=', 1) for line in Path('/etc/os-release').read_text().splitlines() if '=' in line)
if manifest.get('version') != 1 or release.get('ID', '').strip('"') != manifest.get('os') or release.get('VERSION_ID', '').strip('"') != manifest.get('osVersion'):
    raise RuntimeError('The offline development layer supports Ubuntu 26.04 only')
architecture = subprocess.check_output(['dpkg', '--print-architecture'], text=True).strip()
if architecture != manifest['architecture']:
    raise RuntimeError('The offline development layer architecture does not match this distribution')
files = []
for row in manifest['packages']:
    path = (root / row['file']).resolve()
    if path.parent != root or not path.is_file():
        raise RuntimeError('Invalid offline package path')
    digest = hashlib.sha256()
    with path.open('rb') as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b''):
            digest.update(chunk)
    if path.stat().st_size != row['bytes'] or digest.hexdigest() != row['sha256']:
        raise RuntimeError('Offline package verification failed: ' + row['file'])
    files.append(str(path))
if options.verify_only:
    print(json.dumps({'verifiedPackages': len(files)}))
    raise SystemExit(0)
if os.geteuid() != 0:
    raise RuntimeError('Explicit development setup must run as the distribution administrator')
environment = dict(os.environ, DEBIAN_FRONTEND='noninteractive')
with tempfile.TemporaryDirectory(prefix='rainy-ide-apt-') as temporary:
    sources = Path(temporary) / 'sources.list'
    sources.write_text('')
    source_parts = Path(temporary) / 'sources.list.d'
    source_parts.mkdir()
    archives = Path(temporary) / 'archives'
    (archives / 'partial').mkdir(parents=True)
    for file in files:
        shutil.copyfile(file, archives / Path(file).name)
    result = subprocess.run([
        'apt-get', '--no-download', '--yes',
        '-o', f'Dir::Etc::sourcelist={sources}',
        '-o', f'Dir::Etc::sourceparts={source_parts}',
        '-o', f'Dir::Cache::archives={archives}',
        'install', *files,
    ], env=environment)
if result.returncode:
    raise RuntimeError('Offline development package installation did not complete')
for command in ['python3', 'gcc', 'g++', 'gdb', 'cmake', 'ninja', 'clangd', 'clang-format', 'php']:
    subprocess.run(['/usr/bin/which', command], check=True, stdout=subprocess.DEVNULL)
print(json.dumps({'installedPackages': len(files), 'offline': True}))
