/** Resolve the production closure, keeping each dependency version and excluding development-only packages. */
import { createRequire } from 'node:module';
import { readFileSync, realpathSync, existsSync, mkdirSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { createHash } from 'node:crypto';
import { fileURLToPath } from 'node:url';
import yaml from 'js-yaml';
const rainyApp = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const repository = resolve(rainyApp, '../..');
const baseline = process.argv.includes('--baseline');
const selectedPlatform = process.argv.includes('--windows') ? 'win32' : 'linux';
const targetSuffix = selectedPlatform === 'win32' ? 'win32-x64' : 'linux-x64';
const app = baseline ? resolve(repository, 'apps/cli') : rainyApp;
const packages = new Map();
const platformDownloads = [];
const lock = yaml.load(readFileSync(resolve(repository, 'pnpm-lock.yaml'), 'utf8'));
function resolveDirectory(name, owner) {
  const require = createRequire(join(owner, 'package.json'));
  for (const directory of require.resolve.paths(name) ?? []) {
    const candidate = join(directory, name);
    if (existsSync(join(candidate, 'package.json'))) return realpathSync(candidate);
  }
  throw new Error(`Missing production dependency ${name} from ${owner}`);
}
function visit(directory) {
  directory = realpathSync(directory);
  if (packages.has(directory)) return packages.get(directory);
  const manifest = JSON.parse(readFileSync(join(directory, 'package.json'), 'utf8'));
  const item = { id: createHash('sha256').update(directory).digest('hex').slice(0, 16), name: manifest.name, version: manifest.version, source: directory,
    workspace: !directory.includes('node_modules'), dependencies: {} };
  packages.set(directory, item);
  for (const name of new Set([...Object.keys(manifest.dependencies ?? {}), ...Object.keys(manifest.peerDependencies ?? {}), ...Object.keys(manifest.optionalDependencies ?? {})])) {
    const optional = Object.hasOwn(manifest.optionalDependencies ?? {}, name) || manifest.peerDependenciesMeta?.[name]?.optional === true;
    let child;
    try { child = resolveDirectory(name, directory); } catch (error) {
      if (optional) {
        if (name.includes(targetSuffix) && !name.includes('musl')) {
          const candidates = Object.entries(lock.packages).filter(([key]) => key.startsWith(name + '@'));
          const exact = candidates.find(([key]) => key === name + '@' + manifest.optionalDependencies?.[name]) ?? (candidates.length === 1 ? candidates[0] : undefined);
          if (!exact) throw new Error(`No unambiguous locked Linux dependency for ${name}`);
          platformDownloads.push({ owner: item.id, name, version: exact[0].slice(name.length + 1), integrity: exact[1].resolution.integrity });
        }
        continue;
      }
      throw error;
    }
    const target = JSON.parse(readFileSync(join(child, 'package.json'), 'utf8'));
    if (target.os && !target.os.includes(selectedPlatform) && !target.os.includes('any') && !target.os.every(os => os.startsWith('!'))) {
      if (optional) continue;
    }
    if (target.cpu && !target.cpu.includes('x64') && !target.cpu.includes('any') && optional) continue;
    item.dependencies[name] = visit(child).id;
  }
  return item;
}
const root = visit(app);
const system = [...packages.values()].find(item => item.name === '@deepseek-ai/node-addon-system');
if (selectedPlatform === 'linux') {
  const platform = visit(resolve(repository, 'native/system/packages/linux-x64'));
  if (system) system.dependencies[platform.name] = platform.id;
}
const graph = { root: root.id, platform: selectedPlatform, packages: [...packages.values()], platformDownloads, node: '22.22.1', upstream: JSON.parse(readFileSync(join(repository, 'package.json'), 'utf8')).version };
const output = baseline ? resolve(repository, '.artifacts/rainy-baseline') : resolve(rainyApp, 'runtime');
mkdirSync(output, { recursive: true });
writeFileSync(resolve(output, selectedPlatform === 'win32' ? 'graph-windows.json' : 'graph.json'), JSON.stringify(graph, null, 2) + '\n');
console.log(JSON.stringify({ packages: graph.packages.length, workspacePackages: graph.packages.filter(p => p.workspace).length,
  omittedProductPackages: ['office', 'browser-use', 'computer-use', 'schedule', 'subagent', 'plugin-manager'].filter(fragment => !graph.packages.some(p => p.name.includes(fragment))) }));
