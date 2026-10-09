/** Small matched real-model tasks, through the original Web preset or the Rainy profile. */
import { performance } from 'node:perf_hooks';
import { mkdtemp, mkdir, writeFile, readFile } from 'node:fs/promises';
import { homedir, tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import yaml from 'js-yaml';
import { initProfile, loadProfileDirectory, loadLayeredEnv } from '@deepseek-ai/dsh-app-boot';
import { runProfile } from '../../cli/src/profile-boot.ts';
import { createUserMessage } from '@deepseek-ai/dsh-llm';
import { SessionId } from '@deepseek-ai/dsh-session';
import { configureModel, DEEPSEEK_FLASH } from '../src/models.ts';

const variant = process.env.RAINY_BENCHMARK_VARIANT ?? 'rainy';
if (!['baseline', 'rainy'].includes(variant)) throw new Error('Unknown benchmark variant');
const secretDocument = yaml.load(await readFile(join(homedir(), '.rainy-agent/.credentials.yaml'), 'utf8'));
const apiKey = secretDocument.refs.DEEPSEEK_API_KEY;
const home = await mkdtemp(join(tmpdir(), `rainy-benchmark-${variant}-`));
process.env.DSH_HOME = home; process.env.RAINY_HOME = home; process.env.DSH_TELEMETRY_DISABLED = '1';
const app = resolve(import.meta.dirname, '..');
const profileDir = join(home, 'profiles', variant);
const bundles = variant === 'baseline' ? ['@deepseek-ai/dsh-base', '@deepseek-ai/dsh-web-app'] : ['@deepseek-ai/dsh-rainy-desktop'];
initProfile(profileDir, bundles);
const profile = loadProfileDirectory('dsh', profileDir, join(app, 'package.json'));
if (profile.skippedBundles.length) throw new Error(JSON.stringify(profile.skippedBundles));
const bootStart = performance.now();
const { ctx, shutdown } = await runProfile({ environment: loadLayeredEnv('dsh'), profile: variant, args: ['--no-open', '--port', '0'], patchFiles: [], resolvedProfile: { profile, installAnchor: join(app, 'package.json') } });
const bootMs = performance.now() - bootStart;
const baselineRss = process.memoryUsage().rss;
const report = { kind: 'real-api-matched-programming-tasks', variant, upstream: JSON.parse(await readFile(resolve(app, '../../package.json'), 'utf8')).version, model: 'deepseek-flash', contextWindow: 1048576, maxOutputTokens: 393216, effort: 'max', bootMs, idleHostRssBytes: baselineRss, tasks: [] };
const active = new Set();
let current;
ctx.on('llm/stream', async function* (request, next) {
  const run = current;
  if (run) {
    run.requests++;
    if (request.purpose) run.auxiliaryRequests++;
    else {
      run.primaryCaps.push({ maxTokens: request.maxTokens, effort: request.reasoningEffort });
      if (!run.toolNames.length) run.toolNames = (request.tools ?? []).map(tool => tool.name);
    }
  }
  const marker = {}; active.add(marker);
  try {
    for await (const chunk of next()) {
      if (run && chunk.type === 'usage') {
        run.inputTokens += chunk.usage.inputTokens + (chunk.usage.cacheReadTokens ?? 0) + (chunk.usage.cacheWriteTokens ?? 0);
        run.outputTokens += chunk.usage.outputTokens;
      }
      yield chunk;
    }
  } finally { active.delete(marker); }
});
const tasks = [
  { id: 'create', files: {}, prompt: 'Create numbers_util.py with parse_numbers(text): split on ASCII or Chinese commas, trim and ignore empty pieces, convert to floats, invalid numbers raise ValueError. Create stats.py importing parse_numbers and defining average(text), returning None for empty input. Create a unittest file covering mixed delimiters, empty text, negative/decimal numbers and invalid input. Run python3 -m unittest -v.', verify: "from numbers_util import parse_numbers; from stats import average; assert parse_numbers('1,2，3') == [1.,2.,3.]; assert average(' ,， ') is None; assert average('-1.5,2.5') == .5\ntry: parse_numbers('1,bad')\nexcept ValueError: pass\nelse: raise AssertionError('invalid input accepted')" },
  { id: 'edit', files: { 'normalize.py': "def normalize_names(values):\n    return sorted(set(values))\n", 'app.py': "from normalize import normalize_names\n\ndef label(values):\n    return ','.join(normalize_names(values))\n" }, prompt: 'Inspect and fix normalize.py so normalize_names strips whitespace, removes empty items, deduplicates case-insensitively while preserving the first cleaned spelling, and preserves encounter order. Keep app.py importing the same function; label must join the normalized names with a comma. Write and run unittest tests. Do not change either public function signature.', verify: "from normalize import normalize_names; from app import label; assert normalize_names([' Bob ', '', 'alice', 'BOB', ' Alice ']) == ['Bob','alice']; assert label([' 张三 ', '李四', '张三']) == '张三,李四'; assert normalize_names([]) == []" },
  { id: 'jsonl', files: {}, prompt: 'Create records.py with load_records(path), reading UTF-8 JSONL, skipping blank lines and lines whose first non-space character is #, returning a list of dictionaries. Reject non-object JSON values and malformed JSON with ValueError containing the 1-based line number. Create tests using a temporary path with spaces and Chinese characters, run python3 -m unittest -v.', verify: "import tempfile,pathlib; from records import load_records; p=pathlib.Path(tempfile.mkdtemp())/'中文 data.jsonl'; p.write_text('# comment\\n\\n{\"n\":1}\\n',encoding='utf-8'); assert load_records(p)==[{'n':1}]; p.write_text('{}\\n[]\\n',encoding='utf-8')\ntry: load_records(p)\nexcept ValueError as e: assert '2' in str(e)\nelse: raise AssertionError('non-object accepted')" },
];
try {
  await configureModel(ctx, { ...DEEPSEEK_FLASH, apiKey });
  for (const task of tasks) {
    const cwd = join(home, 'projects', task.id); await mkdir(cwd, { recursive: true });
    for (const [name, content] of Object.entries(task.files)) await writeFile(join(cwd, name), content);
    const presets = ctx.get('agentPresets');
    const preset = presets ? (await presets.resolve()).id : undefined;
    const handle = await ctx.agents.create({ sessionId: SessionId(`benchmark-${variant}-${task.id}`), meta: { cwd, ...(preset ? { agentPreset: preset } : {}) }, agentOptions: { provider: DEEPSEEK_FLASH.provider, model: DEEPSEEK_FLASH.model, reasoningEffort: 'max', maxTokens: DEEPSEEK_FLASH.maxTokens }, ...(preset ? { setup: async scope => { await presets.mount(scope, preset); } } : {}) });
    current = { id: task.id, requests: 0, auxiliaryRequests: 0, inputTokens: 0, outputTokens: 0, primaryCaps: [], toolNames: [], toolCalls: 0, compactions: 0, errors: [] };
    const run = current;
    const off = ctx.on('session/event', (session, event) => {
      if (session.id !== handle.agent.id) return;
      if (event.type === 'tool/call') run.toolCalls++;
      if (event.type === 'compaction/end') run.compactions++;
      if (event.type === 'tool/result' && event.data.error) run.errors.push(event.data.error.code);
    });
    const started = performance.now();
    const timeout = setTimeout(() => handle.agent.cancel({ kind: 'user' }), 180000);
    handle.agent.followup(createUserMessage({ content: [{ type: 'text', text: 'Work only in this workspace. Do not use network access, install packages, or delegate to other agents. Complete the task and report actual verification.\n\n' + task.prompt }], source: { kind: 'user' } }));
    await handle.agent.whenIdle(); clearTimeout(timeout);
    run.elapsedMs = performance.now() - started;
    const deadline = Date.now() + 10000;
    while (active.size && Date.now() < deadline) await new Promise(resolve => setTimeout(resolve, 50));
    try {
      const result = await promisify(execFile)('python3', ['-c', task.verify], { cwd, timeout: 15000 });
      run.independentVerification = 'passed'; run.verifierOutput = result.stdout + result.stderr;
    } catch (error) { run.independentVerification = 'failed'; run.verifierOutput = String(error.stderr ?? error.message); }
    run.hostRssBytes = process.memoryUsage().rss;
    report.tasks.push(run); current = undefined; off(); await handle.dispose();
    console.log(JSON.stringify({ variant, task: task.id, result: run.independentVerification, elapsedMs: run.elapsedMs, inputTokens: run.inputTokens, outputTokens: run.outputTokens, tools: run.toolNames.length }));
  }
  const output = process.env.RAINY_BENCHMARK_OUTPUT ?? join(home, 'benchmark.json');
  await writeFile(output, JSON.stringify(report, null, 2) + '\n');
  console.log(JSON.stringify({ variant, reportPath: output }));
} finally { await shutdown.shutdown(0); }
