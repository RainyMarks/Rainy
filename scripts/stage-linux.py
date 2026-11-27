"""Assemble the Linux Host runtime (WSL distributions and SSH servers) from finished build outputs.

The runtime holds `rainy-host` (the Rust Host, built for glibc 2.17 by native/scripts/build-hosts.sh), `app/` (the
license files, `dist/renderer` and `resources`, whose `ide/` has the Linux IDE tools of
`prepare-ide-tools.py --platform linux-x64`) and `runtime.json`. It is written into --output as two layers split by
install-runtime.py's rules: the base `linux-runtime.tar.xz` (the IDE tools; downloaded, identified by its content) and
the application layer `linux-app.tar.gz` (everything else; downloaded on demand as the optional `linux-app` module), with `linux-runtime.json`
({sha256, bytes, unpackedBytes} of the base, its `content` identity, `app: {sha256, bytes}` and the version).
"""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import sys
import tarfile
import tempfile

app = Path(__file__).resolve().parent.parent
profile = os.environ.get('RAINY_PROFILE') or 'release'
parser = argparse.ArgumentParser()
parser.add_argument('--output', required=True)
parser.add_argument('--host', default=str(app / 'native/target/x86_64-unknown-linux-gnu' / profile / 'rainy-host'))
parser.add_argument('--skip-pack', action='store_true')
args = parser.parse_args()
output = Path(args.output).resolve()
output.mkdir(parents=True, exist_ok=True)
host = Path(args.host)
for path, hint in ((host, 'run native/scripts/build-hosts.sh'), (app / 'dist/renderer/index.html', 'run pnpm run build:release'),
                   (app / 'resources/ide/manifest.json', 'run prepare-ide-tools.py --platform linux-x64')):
    if not path.is_file():
        raise RuntimeError(f'{path} is missing: {hint}')
tools = json.loads((app / 'resources/ide/manifest.json').read_text(encoding='utf-8'))
if tools.get('version') != 2 or tools.get('platform') != 'linux-x64':
    raise RuntimeError('resources/ide holds no Linux IDE tools')
version = json.loads((app / 'package.json').read_text(encoding='utf-8'))['version']
staging = Path(tempfile.mkdtemp(prefix='rainy-runtime-', dir='/var/tmp'))
try:
    shutil.copy2(host, staging / 'rainy-host')
    (staging / 'rainy-host').chmod(0o755)
    target = staging / 'app'
    (target / 'dist').mkdir(parents=True)
    for name in ('LICENSE', 'NOTICE', 'TRADEMARKS.md', 'LICENSE.upstream', 'THIRD_PARTY_NOTICES.md'):
        shutil.copy2(app / name, target / name)
    shutil.copytree(app / 'dist/renderer', target / 'dist/renderer', ignore=shutil.ignore_patterns('*.map'))

    def ignore_resources(path, names):
        """The offline Ubuntu packages are the optional module `linux-dev-packages` (prepare-optional-modules.mjs), of which
        the base keeps the manifest the environment check reads; unused source assets and legacy strata-runtime are skipped."""
        ignored = set(shutil.ignore_patterns('*.map')(path, names))
        if Path(path) == app / 'resources':
            ignored.update(('strata-runtime', 'icon.svg'))
        if Path(path) == app / 'resources/ide/system-packages':
            ignored.update(name for name in names if name != 'manifest.json')
        return ignored

    shutil.copytree(app / 'resources', target / 'resources', ignore=ignore_resources)
    ide = target / 'resources/ide'
    for executable in [*(ide / 'bin').iterdir(), ide / 'typescript/lib/tsc']:
        executable.chmod(0o755)
    (staging / 'runtime.json').write_text(json.dumps({'schemaVersion': 2, 'version': version}, indent=2) + '\n', encoding='utf-8')
    if args.skip_pack:
        (output / 'staging-path.txt').write_text(str(staging) + '\n', encoding='utf-8')
        print(json.dumps({'staging': str(staging), 'packed': False}), flush=True)
        raise SystemExit(0)

    sys.dont_write_bytecode = True
    spec = importlib.util.spec_from_file_location('install_runtime', Path(__file__).with_name('install-runtime.py'))
    layers = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(layers)

    def pack(path, keep, mode, **options):
        unpacked = 0
        with tarfile.open(path, mode, **options) as package:
            def member_filter(member):
                nonlocal unpacked
                if not keep(member.name):
                    return None
                unpacked += member.size if member.isfile() else 0
                # Reproducible layers: the same inputs give the same archive regardless of build timestamps.
                member.uid = member.gid = 0
                member.uname = member.gname = ''
                member.mtime = 0
                return member
            for entry in sorted(staging.iterdir()):
                package.add(entry, arcname=entry.name, recursive=True, filter=member_filter)
        return {'sha256': hashlib.sha256(path.read_bytes()).hexdigest(), 'bytes': path.stat().st_size}, unpacked

    # The base is downloaded, so it gets xz (about a third smaller than gzip); the application layer is downloaded on demand
    # as the `linux-app` module and unpacked at each version update, so it stays gzip (max level for smaller transfer).
    base, unpacked = pack(output / 'linux-runtime.tar.xz', layers.in_base, 'w:xz', preset=6)
    application, _ = pack(output / 'linux-app.tar.gz', layers.in_app_layer, 'w:gz', compresslevel=9)
    content = layers.content_digest(staging)
    (output / 'linux-runtime.json').write_text(json.dumps({**base, 'unpackedBytes': unpacked, 'content': content, 'app': application, 'version': version},
                                                          indent=2) + '\n', encoding='utf-8')
    print(json.dumps({'archiveBytes': base['bytes'], 'sha256': base['sha256'], 'content': content, 'appBytes': application['bytes']}), flush=True)
finally:
    if not args.skip_pack:
        shutil.rmtree(staging, ignore_errors=True)
