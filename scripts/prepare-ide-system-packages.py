"""Inventory APT-verified Ubuntu development packages acquired in an isolated baseline image."""
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import subprocess

parser = argparse.ArgumentParser()
parser.add_argument('--archives', default='/var/cache/apt/archives')
options = parser.parse_args()
app = Path(__file__).resolve().parent.parent
release = Path('/etc/os-release').read_text()
if '\nID=ubuntu\n' not in '\n' + release or 'VERSION_ID="26.04"' not in release:
    raise RuntimeError('Development package preparation requires the Ubuntu 26.04 baseline')
output = app / 'resources/ide/system-packages'
output.mkdir(parents=True, exist_ok=True)


def digest(path):
    value = hashlib.sha256()
    with path.open('rb') as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b''):
            value.update(chunk)
    return value.hexdigest()


def fields(text):
    return dict(line.split(': ', 1) for line in text.splitlines() if line and not line.startswith(' ') and ': ' in line)


packages = []
for archive in sorted(Path(options.archives).glob('*.deb')):
    info = fields(subprocess.check_output(['dpkg-deb', '--field', str(archive)], text=True))
    name, version, architecture = info['Package'], info['Version'], info['Architecture']
    raw = subprocess.check_output(['apt-cache', 'show', f'{name}={version}'], text=True)
    candidates = [fields(block) for block in raw.split('\n\n') if block.strip()]
    expected = next((row for row in candidates if row.get('Architecture') == architecture and row.get('Version') == version and 'SHA256' in row), None)
    checksum = digest(archive)
    if expected is None or expected['SHA256'] != checksum or int(expected['Size']) != archive.stat().st_size:
        raise RuntimeError('APT package metadata mismatch: ' + archive.name)
    target = output / archive.name
    if not target.is_file() or digest(target) != checksum:
        shutil.copy2(archive, target)
    packages.append({'name': name, 'version': version, 'architecture': architecture, 'file': archive.name,
                     'repositoryPath': expected['Filename'], 'bytes': archive.stat().st_size, 'sha256': checksum})
if not packages:
    raise RuntimeError('No APT-verified development packages were found')
sources = json.loads((app / 'toolpacks/ide-tools.sources.json').read_text())
manifest = {'version': 1, 'os': 'ubuntu', 'osVersion': '26.04', 'architecture': 'amd64',
            'requestedPackages': sources['systemPackages'],
            'signedIndexes': [{'file': file.name, 'sha256': digest(file)} for file in sorted(Path('/var/lib/apt/lists').glob('*InRelease'))],
            'packages': packages, 'bytes': sum(row['bytes'] for row in packages)}
(output / 'manifest.json').write_text(json.dumps(manifest, indent=2) + '\n')
print(json.dumps({'packages': len(packages), 'bytes': manifest['bytes']}))
