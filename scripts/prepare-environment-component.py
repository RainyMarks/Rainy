"""Build hash-locked offline language/science components in an isolated application-owned directory."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import tarfile
import tempfile
import urllib.request
import urllib.parse
import zipfile
import time
import urllib.error

parser = argparse.ArgumentParser()
parser.add_argument('--component', choices=['basic', 'science-cpu', 'science-cuda', 'cpp'], required=True)
parser.add_argument('--output', required=True)
parser.add_argument('--cache', required=True)
parser.add_argument('--export', required=True)
options = parser.parse_args()
app = Path(__file__).resolve().parent.parent
repository = app.parent.parent
lock = json.loads((repository / 'scripts/primary-runtime/lock.json').read_text(encoding='utf-8'))
component_sources = json.loads((app / 'toolpacks/environment-components.sources.json').read_text(encoding='utf-8'))
windows = platform.system() == 'Windows'
target = 'windows' if windows else 'linux'
artifact = lock['targets']['win-x64' if windows else 'linux-x64']
output = Path(options.output).resolve()
cache = Path(options.cache).resolve()
release = Path(options.export).resolve()
for path in [output, cache, release]:
    path.mkdir(parents=True, exist_ok=True)
component_id = target + '-' + options.component
component = output / component_id
component.mkdir(parents=True, exist_ok=True)
source_log = cache / (component_id + '-sources.json')
sources = json.loads(source_log.read_text(encoding='utf-8')) if source_log.is_file() else []

def digest(path):
    value = hashlib.sha256()
    with path.open('rb') as stream:
        for chunk in iter(lambda: stream.read(8 * 1024 * 1024), b''):
            value.update(chunk)
    return value.hexdigest()

def download(url, checksum):
    if len(checksum) != 64 or any(c not in '0123456789abcdef' for c in checksum):
        raise RuntimeError('An artifact must have a pinned SHA-256 digest')
    path = cache / checksum
    source = {'url': url, 'sha256': checksum}
    if source not in sources:
        sources.append(source)
        source_log.write_text(json.dumps(sources, indent=2) + '\n')
    if path.is_file() and digest(path) == checksum:
        return path
    pending = cache / (checksum + '.' + component_id + '.pending')
    print('Download ' + url, flush=True)
    for attempt in range(6):
        offset = pending.stat().st_size if pending.is_file() else 0
        request = urllib.request.Request(url, headers={'User-Agent': 'RainyAgent-build/1', **({'Range': f'bytes={offset}-'} if offset else {})})
        try:
            with urllib.request.urlopen(request, timeout=120) as response, pending.open('ab' if offset and response.status == 206 else 'wb') as stream:
                shutil.copyfileobj(response, stream, 8 * 1024 * 1024)
            break
        except (urllib.error.URLError, TimeoutError, OSError) as error:
            if attempt == 5:
                raise
            print(f'Retry artifact {attempt + 1}: {type(error).__name__}', flush=True)
            time.sleep(min(2 ** attempt, 8))
    if digest(pending) != checksum:
        raise RuntimeError('Artifact checksum mismatch: ' + url)
    os.replace(pending, path)
    return path

def extract(archive, destination, kind):
    destination.mkdir(parents=True, exist_ok=True)
    if kind == 'zip':
        with zipfile.ZipFile(archive) as package:
            for name in package.namelist():
                path = (destination / name).resolve()
                if not path.is_relative_to(destination):
                    raise RuntimeError('Archive entry escapes component staging')
            package.extractall(destination)
    else:
        with tarfile.open(archive) as package:
            package.extractall(destination, filter='data')

def github_asset(repository, tag, filename):
    key = f'{repository}/{tag}/{filename}'
    checksum = component_sources['githubAssets'].get(key)
    if checksum is None:
        raise RuntimeError('The requested release asset is not locked: ' + key)
    return download(f'https://github.com/{repository}/releases/download/{tag}/{filename}', checksum)

def clean_environment():
    return {key: value for key, value in os.environ.items() if not any(word in key.upper() for word in ['KEY', 'PASSWORD', 'SECRET', 'TOKEN'])
            and key.upper() not in ['PYTHONHOME', 'PYTHONPATH', 'VIRTUAL_ENV', 'CONDA_PREFIX', 'PIP_INDEX_URL', 'PIP_EXTRA_INDEX_URL']}

def run(argv, cwd=None):
    environment = clean_environment()
    environment.update({'PYTHONDONTWRITEBYTECODE': '1', 'PIP_CONFIG_FILE': os.devnull, 'HF_HUB_OFFLINE': '1', 'TRANSFORMERS_OFFLINE': '1'})
    if windows:
        environment['RAINY_PHP_EXTENSION_DIR'] = str(component / 'php/ext')
    subprocess.run([str(value) for value in argv], cwd=cwd, env=environment, check=True)

python_name = f"cpython-{lock['pythonVersion']}+{lock['pythonRelease']}-{artifact['pythonTarget']}-install_only_stripped.tar.gz"
python_url = f"https://github.com/astral-sh/python-build-standalone/releases/download/{lock['pythonRelease']}/{urllib.parse.quote(python_name)}"
if options.component != 'cpp':
    python = component / 'python' / ('python.exe' if windows else 'bin/python3')
    if not python.is_file():
        extract(download(python_url, artifact['pythonSha256']), component, 'tar')
    else:
        sources.append({'url': python_url, 'sha256': artifact['pythonSha256']})
    run([python, '-I', '-c', 'import sys; assert sys.version_info[:2]==(3,12); print(sys.version)'])

if options.component == 'basic':
    node_name = f"node-v{lock['nodeVersion']}-{artifact['nodeArchive']}"
    node_directory = component / 'node'
    node_archive = download(f"https://nodejs.org/dist/v{lock['nodeVersion']}/{node_name}", artifact['nodeSha256'])
    if not node_directory.is_dir():
        temporary = component / '.node-extract'
        extract(node_archive, temporary, 'zip' if windows else 'tar')
        unpacked = next(path for path in temporary.iterdir() if path.is_dir())
        unpacked.rename(node_directory)
        temporary.rmdir()
    if windows:
        entries = [
            ('pwsh', 'PowerShell/PowerShell', 'v7.6.6', 'PowerShell-7.6.6-win-x64.zip'),
            ('git', 'git-for-windows/git', 'v2.56.0.windows.1', 'MinGit-2.56.0-64-bit.zip'),
        ]
        for directory, repo, tag, filename in entries:
            archive = github_asset(repo, tag, filename)
            if not (component / directory).is_dir():
                extract(archive, component / directory, 'zip')
        php = component / 'php'
        if not php.is_dir():
            extract(download('https://windows.php.net/downloads/releases/php-8.5.11-nts-Win32-vs17-x64.zip',
                             '0ea96e0d2b9b737a6036f05cf4e95c49313faa6d0f27bd97edb2742503f0c043'), php, 'zip')
        (php / 'php.ini').write_text('extension_dir="${RAINY_PHP_EXTENSION_DIR}"\nextension=curl\nextension=mbstring\nextension=openssl\nextension=pdo_sqlite\nextension=sqlite3\nextension=zip\n', encoding='utf-8')
        run([php / 'php.exe', '-v'], cwd=php)
        run([component / 'pwsh/pwsh.exe', '-NoLogo', '-NoProfile', '-Command', '$PSVersionTable.PSVersion.ToString()'])
    run([node_directory / ('node.exe' if windows else 'bin/node'), '--version'])
    requirements = ['pip', 'setuptools', 'wheel', 'debugpy', 'ruff', 'requests', 'httpx', 'beautifulsoup4', 'lxml', 'cryptography', 'pycryptodome', 'z3-solver', 'pyelftools', 'pefile', 'lief', 'capstone', 'sympy', 'pillow']
elif options.component.startswith('science-'):
    channel = 'cpu' if options.component == 'science-cpu' else 'cu130'
    wheel_platform = 'win_amd64' if windows else 'manylinux_2_28_x86_64'
    requirements = ['pip', 'setuptools', 'wheel', 'debugpy', 'ruff', 'numpy<2.6', 'scipy', 'pandas', 'scikit-learn', 'matplotlib', 'seaborn', 'sympy', 'pillow', 'opencv-python-headless', 'jupyterlab', 'ipykernel', 'transformers', 'datasets', 'accelerate', 'safetensors', 'tokenizers', 'sentencepiece', 'soundfile', 'librosa', 'requests', 'httpx', 'cryptography', 'pycryptodome', 'z3-solver', 'pyelftools', 'pefile', 'lief', 'capstone']
    for package, version in [('torch', '2.11.0'), ('torchvision', '0.26.0'), ('torchaudio', '2.11.0')]:
        url = f'https://download.pytorch.org/whl/{channel}/{package}/'
        with urllib.request.urlopen(url, timeout=60) as response:
            html = response.read().decode()
        import re
        entries = re.findall(r'href="([^"]+)"', html)
        expected = f'{package}-{version}%2B{channel}-cp312-cp312-{wheel_platform}.whl'
        entry = next(value for value in entries if expected in value)
        requirements.append(package + ' @ ' + urllib.parse.urljoin(url, entry))
elif windows:
    requirements = []
    if not (component / 'cpp').is_dir():
        temporary = component / '.cpp-extract'
        extract(github_asset('mstorsjo/llvm-mingw', '20260922', 'llvm-mingw-20260922-ucrt-x86_64.zip'), temporary, 'zip')
        next(path for path in temporary.iterdir() if path.is_dir()).rename(component / 'cpp')
        temporary.rmdir()
    if not (component / 'cmake').is_dir():
        temporary = component / '.cmake-extract'
        extract(github_asset('Kitware/CMake', 'v4.4.4', 'cmake-4.4.4-windows-x86_64.zip'), temporary, 'zip')
        next(path for path in temporary.iterdir() if path.is_dir()).rename(component / 'cmake')
        temporary.rmdir()
    if not (component / 'ninja').is_dir():
        extract(github_asset('ninja-build/ninja', 'v1.13.2', 'ninja-win.zip'), component / 'ninja', 'zip')
    if not (component / 'llvm').is_dir():
        temporary = component / '.llvm-extract'
        extract(github_asset('llvm/llvm-project', 'llvmorg-23.1.2', 'clang+llvm-23.1.2-x86_64-pc-windows-msvc.tar.xz'), temporary, 'tar')
        next(path for path in temporary.iterdir() if path.is_dir()).rename(component / 'llvm')
        temporary.rmdir()
    if not (component / 'codelldb').is_dir():
        extract(github_asset('vadimcn/codelldb', 'v1.12.3', 'codelldb-win32-x64.vsix'), component / 'codelldb', 'zip')
    run([component / 'cpp/bin/clang.exe', '--version'])
    run([component / 'codelldb/extension/adapter/codelldb.exe', '--help'])
else:
    raise RuntimeError('Linux C/C++ and PHP use the separately verified Ubuntu development component')

if options.component != 'cpp':
    requested = cache / (component_id + '-requested.txt')
    report_path = cache / (component_id + '-resolution.json')
    requested.write_text('\n'.join(requirements) + '\n')
    if not report_path.is_file():
        proxy = os.environ.get('HTTPS_PROXY') or os.environ.get('https_proxy')
        run([python, '-I', '-m', 'pip', '--isolated', '--disable-pip-version-check', *(['--proxy', proxy] if proxy else []), 'install', '--dry-run', '--ignore-installed',
             '--only-binary=:all:', '--index-url', 'https://pypi.org/simple', '--report', report_path, '-r', requested])
    report = json.loads(report_path.read_text(encoding='utf-8'))
    wheels = cache / (component_id + '-wheels')
    wheels.mkdir(exist_ok=True)
    lines = []
    for entry in report['install']:
        url = entry['download_info']['url']
        checksum = entry['download_info']['archive_info']['hashes']['sha256']
        archive = download(url, checksum)
        name = urllib.parse.unquote(urllib.parse.urlparse(url).path.rsplit('/', 1)[1])
        wheel = wheels / name
        if not wheel.exists():
            shutil.copyfile(archive, wheel)
        lines.append(f"{entry['metadata']['name']}=={entry['metadata']['version']} --hash=sha256:{checksum}")
    locked = component / 'requirements.lock.txt'
    locked.write_text('\n'.join(sorted(lines)) + '\n')
    run([python, '-I', '-m', 'pip', '--isolated', '--disable-pip-version-check', 'install', '--no-index', '--find-links', wheels,
         '--require-hashes', '-r', locked])
    run([python, '-I', '-m', 'pip', '--isolated', 'check'])
    # pip's generated launchers embed the build prefix; published components use relative launchers.
    probe = subprocess.check_output([str(python), '-I', '-c', "import importlib.metadata,json; print(json.dumps({e.name:e.value for e in importlib.metadata.entry_points(group='console_scripts')}))"], env=clean_environment(), text=True, encoding='utf-8')
    entrypoints = json.loads(probe)
    if 'pip' in entrypoints:
        entrypoints['pip3.12'] = entrypoints['pip']
    python_root = component / 'python'
    (python_root / '_rainy_entrypoints.json').write_text(json.dumps(entrypoints, indent=2) + '\n', encoding='utf-8')
    (python_root / '_rainy_entrypoint.py').write_text("import importlib,json,sys\nfrom pathlib import Path\nentries=json.loads(Path(__file__).with_name('_rainy_entrypoints.json').read_text(encoding='utf-8'))\nname=sys.argv.pop(1)\nmodule,attribute=entries[name].split(':',1)\nfunction=importlib.import_module(module)\nfor part in attribute.split('.'):\n function=getattr(function,part)\nsys.exit(function())\n", encoding='utf-8')
    import re
    scripts = python_root / ('Scripts' if windows else 'bin')
    for name in entrypoints:
        if not re.fullmatch(r'[A-Za-z0-9_.-]+', name):
            raise RuntimeError('Unsupported console entry point name')
        if windows:
            (scripts / (name + '.exe')).unlink(missing_ok=True)
            (scripts / (name + '.cmd')).write_text('@"%~dp0..\\python.exe" -I "%~dp0..\\_rainy_entrypoint.py" "' + name + '" %*\n@exit /b %errorlevel%\n', encoding='utf-8')
        else:
            script = scripts / name
            script.write_text('#!/bin/sh\nHERE="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"\nexec "$HERE/python3" -I "$HERE/../_rainy_entrypoint.py" "' + name + '" "$@"\n', encoding='utf-8')
            script.chmod(0o755)
    if options.component.startswith('science-'):
        run([python, '-I', '-c', "import numpy as n,pandas as p,scipy.linalg,sklearn.tree,torch,torchvision,torchaudio; assert n.dot([1,2],[3,4])==11; sklearn.tree.DecisionTreeClassifier().fit([[0],[1]],[0,1]); x=torch.ones(2,requires_grad=True); (x*x).sum().backward(); print({'numpy':n.__version__,'torch':torch.__version__,'torchaudio':torchaudio.__version__,'cuda_available':torch.cuda.is_available()})"])

(component / 'SOURCES.json').write_text(json.dumps({'version': 1, 'sources': sources}, indent=2) + '\n')
files = []
for path in sorted(component.rglob('*')):
    if path == component / 'component.json':
        continue
    if path.is_symlink():
        files.append({'path': path.relative_to(component).as_posix(), 'link': os.readlink(path)})
    elif path.is_file():
        files.append({'path': path.relative_to(component).as_posix(), 'bytes': path.stat().st_size, 'sha256': digest(path)})
manifest = {'version': 1, 'id': component_id, 'platform': target, 'architecture': 'x64', 'files': files,
            'unpackedBytes': sum(row.get('bytes', 0) for row in files), 'python': lock['pythonVersion'] if options.component != 'cpp' else None}
(component / 'component.json').write_text(json.dumps(manifest, indent=2) + '\n')
archive = release / (component_id + '.tar.gz')
with tarfile.open(str(archive) + '.pending', 'w:gz', compresslevel=3) as package:
    for path in sorted(component.iterdir()):
        package.add(path, arcname=path.name)
os.replace(str(archive) + '.pending', archive)
descriptor = {'version': 1, 'id': component_id, 'platform': target, 'architecture': 'x64', 'file': archive.name,
              'bytes': archive.stat().st_size, 'sha256': digest(archive), 'unpackedBytes': manifest['unpackedBytes'],
              'manifestSha256': digest(component / 'component.json')}
(release / (component_id + '.json')).write_text(json.dumps(descriptor, indent=2) + '\n')
print(json.dumps({'complete': descriptor, 'staging': str(component)}), flush=True)
