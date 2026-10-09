/** Install the release archives into an isolated root and exercise their relocated runtime entrypoints. */
import assert from 'node:assert/strict'
import { execFile } from 'node:child_process'
import { promisify } from 'node:util'
import { mkdir, readFile, writeFile } from 'node:fs/promises'
import { join, resolve } from 'node:path'
import { tmpdir } from 'node:os'
import { environmentComponentSchema, installWindowsComponent } from '../src/main/environment-components.ts'

const execute = promisify(execFile)
const app = resolve(import.meta.dirname, '..')
const media = resolve(process.argv[2] ?? join(app, 'release/offline-0.3.1/environment-components'))
const root = join(tmpdir(), 'rainy-env-validation')
const results: Record<string, unknown>[] = []
async function run(executable: string, arguments_: string[], environment: Record<string, string> = {}): Promise<string> {
  return (await execute(executable, arguments_, { env: { ...process.env, ...environment, PYTHONDONTWRITEBYTECODE: '1', HF_HUB_OFFLINE: '1', TRANSFORMERS_OFFLINE: '1' },
    cwd: root, timeout: 120000, windowsHide: true, maxBuffer: 1024 * 1024, encoding: 'utf8' })).stdout.trim()
}
for (const id of ['windows-basic', 'windows-science-cpu', 'windows-science-cuda', 'windows-cpp']) {
  const component = environmentComponentSchema.parse(JSON.parse(await readFile(join(media, `${id}.json`), 'utf8')))
  const installed = await installWindowsComponent({ mediaDirectory: media, root, component, progress: message => console.log(id, message) })
  const row: Record<string, unknown> = { id, sha256: component.sha256, installed }
  if (id !== 'windows-cpp') {
    const python = join(installed, 'python/python.exe')
    row.python = await run(python, ['-I', '-c', 'import sys; print(sys.executable)'])
    assert.equal(row.python, python)
    row.pipCheck = await run(python, ['-I', '-m', 'pip', '--isolated', 'check'])
    const pip = join(installed, 'python/Scripts/pip.cmd').replaceAll("'", "''")
    const script = `& '${pip}' --version; if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }`
    row.pipLauncher = await run('powershell.exe', ['-NoProfile', '-NonInteractive', '-EncodedCommand', Buffer.from(script, 'utf16le').toString('base64')])
    if (id === 'windows-basic') {
      row.node = await run(join(installed, 'node/node.exe'), ['-e', 'console.log(6*7)'])
      row.php = await run(join(installed, 'php/php.exe'), ['-r', 'foreach (["curl","mbstring","openssl","pdo_sqlite","zip"] as $x) { if (!extension_loaded($x)) exit(1); } echo 42;'],
        { RAINY_PHP_EXTENSION_DIR: join(installed, 'php/ext') })
    } else {
      const device = id.endsWith('cuda') ? 'cuda' : 'cpu'
      row.science = JSON.parse(await run(python, ['-I', '-c', `import json,numpy,scipy.linalg,sklearn.tree,torch,torchvision,torchaudio; assert numpy.dot([1,2],[3,4])==11; sklearn.tree.DecisionTreeClassifier().fit([[0],[1]],[0,1]); x=torch.ones(8,8,device='${device}',requires_grad=True); (x@x).sum().backward(); assert x.grad.sum().item()==1024; y=torchaudio.functional.resample(torch.ones(1,1600,device='${device}'),16000,8000); assert y.shape[-1]==800; boxes=torch.tensor([[0,0,2,2],[0,0,1,1]],device='${device}',dtype=torch.float); torchvision.ops.nms(boxes,torch.tensor([0.9,0.8],device='${device}'),0.5); print(json.dumps({'device':'${device}','torch':torch.__version__,'vision':torchvision.__version__,'audio':torchaudio.__version__,'backward':True}))`]))
      row.jupyter = await run(python, ['-I', '-m', 'jupyterlab', '--version'])
    }
  } else {
    const source = join(root, 'relocated.cpp')
    const output = join(root, 'relocated.exe')
    await writeFile(source, '#include <iostream>\nint main(){std::cout << 42;}\n')
    await run(join(installed, 'cpp/bin/clang++.exe'), [source, '-o', output])
    row.compilerOutput = await run(output, [], { PATH: `${join(installed, 'cpp/bin')};${process.env.PATH ?? ''}` })
    assert.equal(row.compilerOutput, '42')
    row.debugAdapter = await run(join(installed, 'codelldb/extension/adapter/codelldb.exe'), ['--help'])
  }
  results.push(row)
  console.log(JSON.stringify(row))
}
await mkdir(join(app, 'validation'), { recursive: true })
await writeFile(join(app, 'validation/environment-components-windows.json'), JSON.stringify({ verifiedAt: new Date().toISOString(), results }, null, 2) + '\n')
