"""Exercise the complete offline Linux importer and relocated Python entrypoints in an owned temporary root."""
import argparse
import json
import os
from pathlib import Path
import subprocess

parser = argparse.ArgumentParser()
parser.add_argument('--media', required=True)
parser.add_argument('--root', required=True)
args = parser.parse_args()
app = Path(__file__).resolve().parent.parent
media = Path(args.media).resolve()
root = Path(args.root).resolve()
results = []
environment = dict(os.environ, PYTHONDONTWRITEBYTECODE='1', HF_HUB_OFFLINE='1', TRANSFORMERS_OFFLINE='1')
def run(arguments):
    return subprocess.check_output([str(value) for value in arguments], env=environment, text=True, timeout=180).strip()
for component_id in ['linux-basic', 'linux-science-cpu', 'linux-science-cuda']:
    descriptor = json.loads((media / (component_id + '.json')).read_text())
    completed = subprocess.check_output(['python3', str(app / 'scripts/install-environment-component.py'), str(media / descriptor['file']), json.dumps(descriptor), '--root', str(root)], text=True, timeout=1800)
    installed = Path(json.loads(completed)['path'])
    python = installed / 'python/bin/python3'
    row = {'id': component_id, 'sha256': descriptor['sha256'], 'installed': str(installed),
           'python': run([python, '-I', '-c', 'import sys; print(sys.executable)']),
           'pipCheck': run([python, '-I', '-m', 'pip', '--isolated', 'check']),
           'pipLauncher': run([installed / 'python/bin/pip', '--version'])}
    if not Path(row['python']).resolve().is_relative_to(installed):
        raise RuntimeError('The relocated Python process resolved outside the installed component')
    if component_id == 'linux-basic':
        row['node'] = run([installed / 'node/bin/node', '-e', 'console.log(6*7)'])
    else:
        device = 'cuda' if component_id.endswith('cuda') else 'cpu'
        code = f"import json,numpy,scipy.linalg,sklearn.tree,torch,torchvision,torchaudio; assert numpy.dot([1,2],[3,4])==11; sklearn.tree.DecisionTreeClassifier().fit([[0],[1]],[0,1]); x=torch.ones(8,8,device='{device}',requires_grad=True); (x@x).sum().backward(); assert x.grad.sum().item()==1024; y=torchaudio.functional.resample(torch.ones(1,1600,device='{device}'),16000,8000); assert y.shape[-1]==800; boxes=torch.tensor([[0,0,2,2],[0,0,1,1]],device='{device}',dtype=torch.float); torchvision.ops.nms(boxes,torch.tensor([0.9,0.8],device='{device}'),0.5); print(json.dumps({{'device':'{device}','torch':torch.__version__,'vision':torchvision.__version__,'audio':torchaudio.__version__,'backward':True}}))"
        row['science'] = json.loads(run([python, '-I', '-c', code]))
        row['jupyterLauncher'] = run([installed / 'python/bin/jupyter-lab', '--version'])
    results.append(row)
    print(json.dumps(row), flush=True)
report_path = app / 'validation/environment-components-linux.json'
report_path.parent.mkdir(parents=True, exist_ok=True)
report_path.write_text(json.dumps({'results': results}, indent=2) + '\n')
