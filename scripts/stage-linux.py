"""Assemble a portable Linux runtime with real files and relative package links."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
import urllib.request
import urllib.parse
import base64

parser = argparse.ArgumentParser()
parser.add_argument('--graph', required=True)
parser.add_argument('--output', required=True)
parser.add_argument('--reuse', help='Existing development staging directory to refresh')
parser.add_argument('--skip-pack', action='store_true')
parser.add_argument('--seed', help='Read-only source of already staged identical dependency packages')
args = parser.parse_args()
graph = json.loads(Path(args.graph).read_text())
output = Path(args.output).resolve()
output.mkdir(parents=True, exist_ok=True)
staging = Path(args.reuse).resolve() if args.reuse else Path(tempfile.mkdtemp(prefix='rainy-runtime-', dir='/var/tmp'))
if args.reuse and (staging.parent not in {Path('/tmp'), Path('/var/tmp')} or not staging.name.startswith('rainy-runtime-')):
    raise RuntimeError('Unexpected development staging directory')
packages = {item['id']: item for item in graph['packages']}
directories = {key: staging / ('app' if key == graph['root'] else 'packages/' + key) for key in packages}
seed = Path(args.seed).resolve() if args.seed else None

def linux_path(value):
    return Path(subprocess.check_output(['wslpath', '-u', value], text=True).strip())

version = graph['node']
cache = Path.home() / '.cache/rainy-agent-build'
cache.mkdir(parents=True, exist_ok=True)
archive_name = f'node-v{version}-linux-x64.tar.xz'
archive = cache / archive_name
base = f'https://nodejs.org/dist/v{version}/'
checksums = urllib.request.urlopen(base + 'SHASUMS256.txt', timeout=60).read().decode()
expected = next(line.split()[0] for line in checksums.splitlines() if line.endswith('  ' + archive_name))
if not archive.exists() or hashlib.sha256(archive.read_bytes()).hexdigest() != expected:
    with urllib.request.urlopen(base + archive_name, timeout=120) as response, archive.open('wb') as target:
        shutil.copyfileobj(response, target)
if hashlib.sha256(archive.read_bytes()).hexdigest() != expected:
    raise RuntimeError('Node runtime checksum mismatch')
if not (staging / 'node/bin/node').exists():
    if seed and (seed / 'node/bin/node').exists():
        shutil.copytree(seed / 'node', staging / 'node', copy_function=os.link, symlinks=True)
    else:
        with tarfile.open(archive) as package:
            package.extractall(staging, filter='data')
        (staging / f'node-v{version}-linux-x64').rename(staging / 'node')
print('Verified pinned Node runtime', flush=True)

for index, (key, item) in enumerate(packages.items()):
    source = linux_path(item['source'])
    destination = directories[key]
    if args.reuse and key != graph['root'] and not item['workspace'] and (destination / 'package.json').exists():
        continue
    if seed and key != graph['root'] and not item['workspace'] and (seed / 'packages' / key / 'package.json').exists():
        shutil.copytree(seed / 'packages' / key, destination, copy_function=os.link, symlinks=True)
        continue
    destination.mkdir(parents=True, exist_ok=True)
    if item['workspace']:
        for entry in source.iterdir():
            if entry.name in {'node_modules', 'src', 'tests', 'scripts', 'runtime', '.git'}:
                continue
            if entry.is_dir() and entry.name not in {'lib', 'dist', 'bin', 'resources', 'presets'}:
                continue
            if entry.is_file() and not (entry.name in {'package.json', 'prebuilds.json', 'LICENSE', 'LICENSE.md', 'THIRD_PARTY_NOTICES.md'} or entry.suffix in {'.yml', '.yaml'}):
                continue
            if entry.is_dir():
                def ignore_workspace(path, names):
                    ignored = set(shutil.ignore_patterns('*.map', '*.tsbuildinfo')(path, names))
                    if key == graph['root'] and Path(path) == source / 'resources':
                        ignored.add('strata-runtime')
                    return ignored
                shutil.copytree(entry, destination / entry.name, ignore=ignore_workspace, dirs_exist_ok=True)
            else:
                shutil.copy2(entry, destination / entry.name)
    else:
        shutil.copytree(source, destination, dirs_exist_ok=True, ignore=shutil.ignore_patterns('node_modules', '.git', 'test', 'tests', '*.map'))
    if index % 50 == 0:
        print(f'Copied {index + 1}/{len(packages)} packages', flush=True)

for key, item in packages.items():
    for name, child in item['dependencies'].items():
        link = directories[key] / 'node_modules' / name
        link.parent.mkdir(parents=True, exist_ok=True)
        if not link.exists():
            link.symlink_to(os.path.relpath(directories[child], link.parent), target_is_directory=True)
app = directories[graph['root']]
self_link = app / 'node_modules' / packages[graph['root']]['name']
self_link.parent.mkdir(parents=True, exist_ok=True)
if not self_link.exists():
    self_link.symlink_to(os.path.relpath(app, self_link.parent), target_is_directory=True)

for dependency in graph.get('platformDownloads', []):
    destination = directories[dependency['owner']] / 'node_modules' / dependency['name']
    if (destination / 'package.json').exists():
        continue
    metadata_url = 'https://registry.npmjs.org/' + urllib.parse.quote(dependency['name'], safe='') + '/' + dependency['version']
    metadata = json.load(urllib.request.urlopen(metadata_url, timeout=60))
    data = urllib.request.urlopen(metadata['dist']['tarball'], timeout=120).read()
    algorithm, expected_hash = dependency['integrity'].split('-', 1)
    if algorithm != 'sha512' or base64.b64encode(hashlib.sha512(data).digest()).decode() != expected_hash:
        raise RuntimeError('Locked platform package checksum mismatch: ' + dependency['name'])
    archive_path = cache / (dependency['name'].replace('/', '_') + '-' + dependency['version'] + '.tgz')
    archive_path.write_bytes(data)
    temporary = Path(tempfile.mkdtemp(prefix='native-', dir=cache))
    with tarfile.open(archive_path) as package:
        package.extractall(temporary, filter='data')
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.move(str(temporary / 'package'), destination)
    print('Verified Linux dependency: ' + dependency['name'], flush=True)

# Native loaders may intentionally resolve sibling platform packages from their own module scope.
# Explicit per-package links above retain version selection; this hoisted fallback supplies those siblings.
for key, item in packages.items():
    link = staging / 'node_modules' / item['name']
    link.parent.mkdir(parents=True, exist_ok=True)
    if not link.exists():
        link.symlink_to(os.path.relpath(directories[key], link.parent), target_is_directory=True)
for dependency in graph.get('platformDownloads', []):
    destination = directories[dependency['owner']] / 'node_modules' / dependency['name']
    link = staging / 'node_modules' / dependency['name']
    link.parent.mkdir(parents=True, exist_ok=True)
    if not link.exists():
        link.symlink_to(os.path.relpath(destination, link.parent), target_is_directory=True)

native = next(item for item in packages.values() if item['name'] == '@deepseek-ai/node-addon-system-linux-x64')
source = linux_path(native['source']).parent / 'entry/src'
native_dir = directories[native['id']]
(native_dir / 'bin/glibc').mkdir(parents=True, exist_ok=True)
subprocess.run(['cc', '-std=c11', '-O2', '-fPIC', '-fvisibility=hidden', '-DNAPI_VERSION=8', '-I', str(staging / 'node/include/node'), '-shared', '-o', str(native_dir / 'bin/glibc/system.node.pending'), str(source / 'flock.c')], check=True)
os.replace(native_dir / 'bin/glibc/system.node.pending', native_dir / 'bin/glibc/system.node')
subprocess.run(['cc', '-std=c11', '-Os', '-static', '-s', '-o', str(native_dir / 'bin/landlock-run.pending'), str(source / 'main.c')], check=True)
os.replace(native_dir / 'bin/landlock-run.pending', native_dir / 'bin/landlock-run')

node_pty = next((item for item in packages.values() if item['name'] == 'node-pty'), None)
if node_pty:
    pty_dir = directories[node_pty['id']]
    binaries = list(pty_dir.glob('prebuilds/linux-x64/pty.node'))
    if not binaries:
        raise RuntimeError('node-pty Linux binary missing from the locked package')
    for executable in pty_dir.rglob('spawn-helper'):
        executable.chmod(executable.stat().st_mode | 0o111)
manifest = {'schemaVersion': 1, 'node': version, 'upstream': graph['upstream'], 'packages': [{'name': p['name'], 'version': p['version']} for p in packages.values()]}
(staging / 'runtime.json').write_text(json.dumps(manifest, indent=2) + '\n')
rg = next((dependency for dependency in graph.get('platformDownloads', []) if dependency['name'] == '@vscode/ripgrep-linux-x64'), None)
if rg:
    binaries = list((directories[rg['owner']] / 'node_modules' / rg['name']).rglob('rg'))
    if len(binaries) != 1:
        raise RuntimeError('Bundled ripgrep executable was not found')
    (staging / 'bin').mkdir(exist_ok=True)
    link = staging / 'bin/rg'
    if not link.exists():
        link.symlink_to(os.path.relpath(binaries[0], link.parent))
    binaries[0].chmod(binaries[0].stat().st_mode | 0o111)
if args.skip_pack:
    (output / 'staging-path.txt').write_text(str(staging) + '\n')
    print(json.dumps({'staging': str(staging), 'packed': False}), flush=True)
    raise SystemExit(0)
archive = output / 'linux-runtime.tar.gz'
def production_member(member):
    # Integration harnesses and their fixtures run in the staging tree, not in a user's installation.
    name = Path(member.name).name
    return None if name.endswith(('.map', '-test.js', '-smoke.js')) or name in ['mcp-fixture.mjs', 'panel.js'] else member
with tarfile.open(archive, 'w:gz', compresslevel=3) as package:
    for entry in staging.iterdir():
        package.add(entry, arcname=entry.name, recursive=True, filter=production_member)
digest = hashlib.sha256(archive.read_bytes()).hexdigest()
(output / 'linux-runtime.json').write_text(json.dumps({'sha256': digest, 'bytes': archive.stat().st_size, 'node': version, 'upstream': graph['upstream']}, indent=2) + '\n')
(output / 'staging-path.txt').write_text(str(staging) + '\n')
print(json.dumps({'archiveBytes': archive.stat().st_size, 'sha256': digest, 'staging': str(staging)}), flush=True)
