"""Assemble the WSL Host runtime archive with real files and relative package links.

Reads the graph written by `runtime-graph.mjs` (Linux target) and writes `linux-runtime.tar.gz` and
`linux-runtime.json` ({sha256, bytes, node, version}) into --output. The archive holds `node/`, `app/`
(package.json, dist/host.js, dist/renderer, resources, bin/rg), `packages/<id>/` with relative `node_modules`
links and `runtime.json`.
"""
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
if graph.get('platform') != 'linux' or graph.get('version') != 2:
    raise RuntimeError('Prepare the Linux dependency graph first: node scripts/runtime-graph.mjs')
output = Path(args.output).resolve()
output.mkdir(parents=True, exist_ok=True)
staging = Path(args.reuse).resolve() if args.reuse else Path(tempfile.mkdtemp(prefix='rainy-runtime-', dir='/var/tmp'))
if args.reuse and (staging.parent not in {Path('/tmp'), Path('/var/tmp')} or not staging.name.startswith('rainy-runtime-')):
    raise RuntimeError('Unexpected development staging directory')
packages = {item['id']: item for item in graph['packages']}
directories = {key: staging / ('app' if key == graph['root'] else 'packages/' + key) for key in packages}
seed = Path(args.seed).resolve() if args.seed else None

def linux_path(value):
    """Graphs written on Windows carry Windows paths; a graph written on Linux is used as is."""
    if value.startswith('/'):
        return Path(value)
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

def copy_app(source, destination):
    """Copy only what the Host reads at run time; Strata is an optional module of the Windows carrier."""
    for required in ('dist/host.js', 'dist/renderer/index.html'):
        if not (source / required).is_file():
            raise RuntimeError(f'Build the application first (pnpm run build:release): {required} is missing')
    manifest = json.loads((source / 'package.json').read_text())
    (destination / 'package.json').write_text(json.dumps({'name': manifest['name'], 'productName': manifest['productName'],
        'version': manifest['version'], 'private': True, 'type': 'module', 'license': manifest['license']}, indent=2) + '\n')
    for name in ('LICENSE', 'LICENSE.RainyAgent', 'LICENSE.upstream', 'THIRD_PARTY_NOTICES.md'):
        shutil.copy2(source / name, destination / name)
    (destination / 'dist').mkdir(exist_ok=True)
    shutil.copy2(source / 'dist/host.js', destination / 'dist/host.js')
    shutil.copytree(source / 'dist/renderer', destination / 'dist/renderer', dirs_exist_ok=True, ignore=shutil.ignore_patterns('*.map'))

    def ignore_resources(path, names):
        ignored = set(shutil.ignore_patterns('*.map')(path, names))
        if Path(path) == source / 'resources':
            ignored.add('strata-runtime')
        return ignored
    shutil.copytree(source / 'resources', destination / 'resources', dirs_exist_ok=True, ignore=ignore_resources)

for index, (key, item) in enumerate(packages.items()):
    source = linux_path(item['source'])
    destination = directories[key]
    if args.reuse and key != graph['root'] and (destination / 'package.json').exists():
        continue
    if seed and key != graph['root'] and (seed / 'packages' / key / 'package.json').exists():
        shutil.copytree(seed / 'packages' / key, destination, copy_function=os.link, symlinks=True)
        continue
    destination.mkdir(parents=True, exist_ok=True)
    if key == graph['root']:
        copy_app(source, destination)
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

node_pty = next((item for item in packages.values() if item['name'] == 'node-pty'), None)
if node_pty:
    pty_dir = directories[node_pty['id']]
    binaries = list(pty_dir.glob('prebuilds/linux-x64/pty.node'))
    if not binaries:
        raise RuntimeError('node-pty Linux binary missing from the locked package')
    for executable in pty_dir.rglob('spawn-helper'):
        executable.chmod(executable.stat().st_mode | 0o111)
manifest = {'schemaVersion': 1, 'node': version, 'version': graph['appVersion'], 'packages': [{'name': p['name'], 'version': p['version']} for p in packages.values()]}
(staging / 'runtime.json').write_text(json.dumps(manifest, indent=2) + '\n')
rg_download = next((dependency for dependency in graph.get('platformDownloads', []) if dependency['name'] == '@vscode/ripgrep-linux-x64'), None)
rg_package = next((key for key, item in packages.items() if item['name'] == '@vscode/ripgrep-linux-x64'), None)
rg_root = directories[rg_download['owner']] / 'node_modules' / rg_download['name'] if rg_download else directories[rg_package] if rg_package else None
if rg_root is None:
    raise RuntimeError('The Linux ripgrep package is absent from the runtime graph')
binaries = [path for path in rg_root.rglob('rg') if path.is_file()]
if len(binaries) != 1:
    raise RuntimeError('Bundled ripgrep executable was not found')
# The Host puts <app>/bin on PATH for its shell tool.
(directories[graph['root']] / 'bin').mkdir(exist_ok=True)
link = directories[graph['root']] / 'bin/rg'
if not link.exists():
    link.symlink_to(os.path.relpath(binaries[0], link.parent))
binaries[0].chmod(binaries[0].stat().st_mode | 0o111)
if args.skip_pack:
    (output / 'staging-path.txt').write_text(str(staging) + '\n')
    print(json.dumps({'staging': str(staging), 'packed': False}), flush=True)
    raise SystemExit(0)
archive = output / 'linux-runtime.tar.gz'
def production_member(member):
    return None if member.name.endswith('.map') else member
with tarfile.open(archive, 'w:gz', compresslevel=3) as package:
    for entry in staging.iterdir():
        package.add(entry, arcname=entry.name, recursive=True, filter=production_member)
digest = hashlib.sha256(archive.read_bytes()).hexdigest()
(output / 'linux-runtime.json').write_text(json.dumps({'sha256': digest, 'bytes': archive.stat().st_size, 'node': version, 'version': graph['appVersion']}, indent=2) + '\n')
(output / 'staging-path.txt').write_text(str(staging) + '\n')
print(json.dumps({'archiveBytes': archive.stat().st_size, 'sha256': digest, 'staging': str(staging)}), flush=True)
