/** Materialize Rainy's explicit composition from the pinned upstream rows. */
import { readFileSync, writeFileSync, mkdirSync } from 'node:fs';
import { resolve, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import yaml from 'js-yaml';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../../..');
const app = resolve(root, 'apps/rainy-desktop');
const manifestPath = resolve(app, 'package.json');
const currentManifest = JSON.parse(readFileSync(manifestPath, 'utf8'));
class Expression { constructor(value) { this.value = value; } }
const expression = new yaml.Type('tag:yaml.org,2002:js', {
  kind: 'scalar', instanceOf: Expression,
  construct: value => new Expression(value), represent: value => value.value,
});
const schema = yaml.DEFAULT_SCHEMA.extend([expression]);
const readRows = path => yaml.load(readFileSync(resolve(root, path), 'utf8'), { schema }).flatMap(row => row.insert ?? []);
const base = new Map(readRows('packages/bundle/base/cordis.patch.yml').map(row => [row.id, row]));
const web = new Map(readRows('packages/bundle/web-app/cordis.patch.yml').map(row => [row.id, row]));
const coreIds = 'timer hmr llm session typert typert-loader typert-gateway session-title agent agent-default-model jobs llm-retry config-editor settings authorization credentials llm-pi-ai session-persistence-jsonl attachment-local session-query-sqlite session-projection storage storage-json storage-domain session-projection-cache subprocess sandbox sandbox-policy bash-sandbox approval permission shell-env fs-observation-policy tool-fs agent-instructions commands token-meter compaction-basic command-compact timeout-policy spill-local spill-policy session-checkpoint-policy tool-result-pruner tools system-prompt agent-loop fs-sandbox tool-bash'.split(' ');
const uiIds = 'workspace session-reference file-reference-local session-stats session-turn-outline session-controller job-controller terminal-controller workspace-files settings-controller workspace-controller modules connection file-upload api-remotes ui-theme locale shortcuts ui-shortcuts ui-layout ui-renderer ui-session resources ui-sidebar ui-sidebar-right ui-sidebar-terminal ui-sidebar-files ui-settings ui-settings-general ui-settings-models ui-conversation ui-approval ui-chat ui-workspace ui-input-trigger ui-commands ui-reference ui-model-selection ui-permission ui-trajectory'.split(' ');
const rows = [...coreIds.map(id => base.get(id)), ...uiIds.map(id => web.get(id))];
if (rows.some(row => !row)) throw new Error('Pinned upstream composition changed: missing selected row');
for (const row of rows) delete row.disabled;
const set = (id, config) => { rows.find(row => row.id === id).config = config; };
set('agent-default-model', { provider: '', model: '' });
set('system-prompt', {
  includeHarnessIdentity: false, includeRuntimeContext: false,
  personaPrefix: 'You are RainyAgent, a concise coding assistant. Follow the user and applicable project instructions. Inspect relevant files before editing. Use read, write, edit and bash to complete the task. Check command results and report what was actually verified. Keep tool output bounded; use rg and targeted line ranges. Continue from compacted checkpoints without repeating finished work.',
  personaSuffix: 'Working directory: {{cwd}}. Commands execute in Bash on Linux (WSL).',
});
set('agent-loop', { agents: [], maxParallelToolCalls: 1 });
set('tool-bash', { enableRunInBackground: false, promoteOnTimeout: false });
set('compaction-basic', { auto: false, thresholdRatio: 0.7, headroomTokens: 8000, retainTokens: 20000, maxTokens: 4000, compactionRetries: 0, maxOverflowRetries: 0 });
set('spill-policy', {});
set('connection', { trustedHosts: [] });
delete rows.find(row => row.id === 'connection').inject;
set('session-persistence-jsonl', { root: new Expression("dshHomePath('sessions')"), compression: 'none' });
set('agent-instructions', { maxBytes: 1048576 });
set('locale', { preference: 'zh' });
rows.splice(rows.findIndex(row => row.id === 'ui-settings-models'), 1);
rows.find(row => row.id === 'compaction-basic').name = '@deepseek-ai/dsh-rainy-desktop/compaction';
rows.push(
  { id: 'webserver', name: '@deepseek-ai/dsh-host-webserver', config: { host: '127.0.0.1', port: new Expression('Number(process.env.RAINY_PORT ?? 0)') } },
  { id: 'directory-picker', name: '@deepseek-ai/dsh-host-directory-picker-browse' },
  { id: 'directory-picker-client', name: '@deepseek-ai/dsh-client-ui-directory-picker-browse' },
  { id: 'pty', name: '@deepseek-ai/dsh-terminal' },
  { id: 'terminal-bash', name: '@deepseek-ai/dsh-terminal-bash' },
  { id: 'rainy-web', name: '@deepseek-ai/dsh-rainy-desktop/web' },
  { id: 'rainy-ide', name: '@deepseek-ai/dsh-rainy-desktop/ide' },
  { id: 'rainy-policy', name: '@deepseek-ai/dsh-rainy-desktop/policy' },
  { id: 'rainy-ui', name: '@deepseek-ai/dsh-client-ui-rainy' },
  { id: 'rainy-extensions', name: '@deepseek-ai/dsh-rainy-desktop/extensions' },
);
const dependencies = Object.fromEntries([...new Set(rows.map(row => {
  const parts = row.name.split('/'); return parts[0].startsWith('@') ? parts.slice(0, 2).join('/') : parts[0];
}).filter(name => name !== '@deepseek-ai/dsh-rainy-desktop'))].sort().map(name => [name, name.includes('cordis') && !name.includes('dsh') ? 'workspace:~' : 'workspace:*']));
Object.assign(dependencies, {
  '@deepseek-ai/cordis': 'workspace:~', '@deepseek-ai/schemastery': 'workspace:~',
  '@deepseek-ai/dsh-app-boot': 'workspace:*', '@deepseek-ai/dsh-cmdline': 'workspace:*',
  '@deepseek-ai/dsh-home-paths': 'workspace:*', '@deepseek-ai/dsh-http-proxy': 'workspace:*',
  '@deepseek-ai/dsh-launch-environment': 'workspace:*', '@deepseek-ai/dsh-host-frontend-static': 'workspace:*',
  '@deepseek-ai/dsh-web-frontend': 'workspace:*', '@deepseek-ai/dsh-compaction': 'workspace:*',
  '@deepseek-ai/dsh-compaction-basic': 'workspace:*',
  '@deepseek-ai/dsh-credentials': 'workspace:*', '@deepseek-ai/dsh-util-values': 'workspace:*',
  '@deepseek-ai/dsh-spill': 'workspace:*', '@deepseek-ai/dsh-skill': 'workspace:*',
  '@deepseek-ai/dsh-skill-filesystem': 'workspace:*', '@deepseek-ai/dsh-tool-skill': 'workspace:*',
  '@deepseek-ai/dsh-mcp-client': 'workspace:*', '@deepseek-ai/dsh-fs': 'workspace:*',
  '@deepseek-ai/cordis-plugin-include': 'workspace:~', 'js-yaml': '^4.2.0', '@vscode/ripgrep': '1.18.0',
});
mkdirSync(resolve(app, 'src'), { recursive: true });
writeFileSync(resolve(app, 'cordis.patch.yml'), '# RainyAgent: explicit production plugin roster.\n' + yaml.dump([{ insert: rows }], { schema, lineWidth: -1 }));
writeFileSync(manifestPath, JSON.stringify({
  ...currentManifest,
  name: '@deepseek-ai/dsh-rainy-desktop', version: currentManifest.version, private: true, type: 'module',
  description: 'RainyAgent desktop with a WSL-hosted minimal DSH profile', license: 'SEE LICENSE IN LICENSE',
  main: 'lib/main.cjs',
  exports: { ...currentManifest.exports, '.': './lib/policy.js', './policy': './lib/policy.js', './web': './lib/web.js', './compaction': './lib/compaction.js', './extensions': './lib/extensions.js', './ide': './lib/ide.js', './package.json': './package.json' },
  dsh: { bundle: { patch: './cordis.patch.yml' } },
  scripts: { ...currentManifest.scripts, build: 'tsx scripts/build.ts', test: 'vitest run --config vitest.config.ts', start: 'electron .', package: 'electron-builder --config electron-builder.config.cjs --win nsis' },
  dependencies: { ...currentManifest.dependencies, ...dependencies },
  devDependencies: currentManifest.devDependencies,
}, null, 2) + '\n');
console.log(JSON.stringify({ entries: rows.length, coreTools: ['read', 'write', 'edit', 'bash'] }));
