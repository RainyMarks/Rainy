/** Offline model preparation and process ownership use isolated files and deterministic child barriers. */
import { mkdtemp, mkdir, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { dirname, join, sep } from 'node:path'
import { describe, expect, it, vi } from 'vitest'
import { z } from 'zod'
import { createStrataManager } from '../src/strata.ts'
import type { StrataManagerOptions } from '../src/strata.ts'
import { DEFAULT_STRATA_SETTINGS, discoverStrataProfiles, resolveStrataModel } from '../src/strata-model.ts'
import type { StrataProcess, StrataProcessExit, StrataProcessSpec } from '../src/strata-process.ts'
import type { StrataHealth } from '../src/strata-health.ts'

async function put(path: string, content = 'fixture'): Promise<void> {
  await mkdir(dirname(path), { recursive: true })
  await writeFile(path, content)
}

async function preparedPack(path: string): Promise<void> {
  for (const file of ['dense.bin', 'index.txt', 'native_experts.txt', 'tokenizer/vocab.json', 'tokenizer/merges.txt', 'tokenizer/token_type.json']) {
    await put(join(path, file))
  }
}

async function preparedMtp(path: string): Promise<void> {
  for (const file of ['dense.bin', 'dense.txt', 'experts.bin']) await put(join(path, file))
}

async function files(onTestFinished: (cleanup: () => Promise<void>) => void, prepared = true) {
  const root = await mkdtemp(join(tmpdir(), 'rainy-strata-'))
  onTestFinished(async () => { await rm(root, { recursive: true, force: true, maxRetries: 5, retryDelay: 20 }) })
  const runtimeRoot = join(root, 'runtime')
  for (const file of ['portablepython/python.exe', 'server/serve/server.py', 'server/tools/iq_pack.py',
    'server/tools/mtp_rt.py', 'server/tools/strata_tokenizer.py', 'server/data/expert-profile.bin', 'engine/strata.exe']) {
    await put(join(runtimeRoot, file))
  }
  await put(join(runtimeRoot, 'engine/BUILD.json'), JSON.stringify({ version: '0.1.39' }))
  const weights = join(root, 'external-weights')
  const gguf = join(weights, 'models', 'unsloth', 'Qwen3.8-Flash-Next-00001-of-00002.gguf')
  const second = gguf.replace('00001-of-', '00002-of-')
  await put(gguf, 'GGUF-fixture-shard-one')
  await put(second, 'GGUF-fixture-shard-two')
  const pack = join(weights, 'packs', 'unsloth')
  const mtp = join(weights, 'mtp', 'rt')
  if (prepared) { await preparedPack(pack); await preparedMtp(mtp) }
  const profile = join(root, 'original-strata', 'strata-unsloth.json')
  await put(profile, JSON.stringify({ exe: 'original-engine-must-not-run.exe', cwd: root,
    args: ['--native', gguf, '--pack', pack, '--mtp', mtp, '--max-context', '65536', '--kv', 'int8', '--resident-budget-gib', '39'],
    tokenizer: join(pack, 'tokenizer'), model_name: 'qwen3.8-flash-next', port: 31337,
    before_load: 'original-hook-must-not-run', api_key: 'fixture-key', log: 'original-private.log', mcp_servers: [{ name: 'not-imported' }],
  }))
  return { root, runtimeRoot, userData: join(root, 'user-data'), gguf, second, weights, pack, mtp, profile,
    profileSettingsPath: join(root, 'strata-installs.json') }
}

function processFixture() {
  let finished = false
  const completion = Promise.withResolvers<StrataProcessExit>()
  const finish = (code = 0) => { finished = true; completion.resolve({ code, signal: null }) }
  const stop = vi.fn(async () => { finish() })
  const child: StrataProcess = {
    pid: 12345, exited: completion.promise, get finished() { return finished },
    stop,
  }
  return { child, finish, stop }
}

function processProvider() {
  const children: ReturnType<typeof processFixture>[] = []
  const specs: StrataProcessSpec[] = []
  const barriers = new Map<number, ReturnType<typeof Promise.withResolvers<StrataProcessSpec>>>()
  const barrier = (index: number) => {
    let result = barriers.get(index)
    if (!result) { result = Promise.withResolvers<StrataProcessSpec>(); barriers.set(index, result) }
    return result
  }
  const launch = vi.fn((spec: StrataProcessSpec) => {
    specs.push(spec)
    const process = processFixture()
    children.push(process)
    barrier(children.length).resolve(spec)
    return process.child
  })
  return { launch, children, specs, spawned: (index: number) => barrier(index).promise }
}

function liveHealth(model = 'qwen3.8-flash-next'): StrataHealth {
  return { baseURL: 'http://127.0.0.1:31337/v1', model, contextWindow: 32768, loaded: true, authenticationRequired: false }
}

async function managerFixture(
  onTestFinished: (cleanup: () => Promise<void>) => void,
  options: Partial<StrataManagerOptions> = {}, prepared = true,
) {
  const fixture = await files(onTestFinished, prepared)
  const processes = processProvider()
  const readHealth = vi.fn(async () => {
    const serving = processes.specs.some((spec, index) => spec.args.includes('--engine') && !processes.children[index]?.child.finished)
    if (!serving) throw new Error('not listening')
    return liveHealth()
  })
  const unload = vi.fn(async () => {})
  const manager = createStrataManager({ ...fixture, platform: 'win32', launch: processes.launch,
    readHealth, unload, availableMemory: () => 64 * 2 ** 30, pollIntervalMs: 1, ...options })
  onTestFinished(async () => { await manager.close() })
  return { ...fixture, ...processes, manager, readHealth, unload }
}

describe('Strata external model files', () => {
  it('imports supported settings without copying original executable, hooks, keys, or logs', async ({ onTestFinished }) => {
    const test = await files(onTestFinished)
    const original = await readFile(test.profile)
    const result = await resolveStrataModel({ ...DEFAULT_STRATA_SETTINGS, modelPath: test.profile }, join(test.userData, 'strata'), true)
    expect(result?.settings).toMatchObject({ contextWindow: 65536, port: 31337, residentBudgetGiB: 39, mtpPath: test.mtp })
    expect(result?.model).toMatchObject({ ggufPath: test.gguf, packPath: test.pack, mtpPath: test.mtp, needsPreparation: false })
    expect(result?.preparations).toEqual([])
    expect(await readFile(test.profile)).toEqual(original)
  })

  it('requires every model shard and identifies missing MTP before resource allocation', async ({ onTestFinished }) => {
    const test = await files(onTestFinished, false)
    const input = { ...DEFAULT_STRATA_SETTINGS, modelPath: test.gguf }
    const result = await resolveStrataModel(input, join(test.userData, 'strata'))
    expect(result?.missing[0]).toContain('MTP')
    await rm(test.second)
    await expect(resolveStrataModel(input, join(test.userData, 'strata'))).rejects.toThrow('缺少模型分片')
  })

  it('prepares external main and MTP GGUFs only through bundled offline tools', async ({ onTestFinished }) => {
    const test = await files(onTestFinished, false)
    const mtp = join(test.weights, 'mtp-q2_0.gguf')
    await put(mtp, 'GGUF-fixture-mtp')
    const result = await resolveStrataModel({ ...DEFAULT_STRATA_SETTINGS, modelPath: test.gguf, mtpPath: mtp }, join(test.userData, 'strata'))
    expect(result?.missing).toEqual([])
    expect(result?.preparations.map(item => item.tool)).toEqual(['iq_pack.py', 'mtp_rt.py'])
    expect(result?.preparations.every(item => item.directory.startsWith(test.userData))).toBe(true)
    expect(result?.preparations.some(item => item.args.includes('--experts-bin'))).toBe(false)
  })

  it('discovers profile paths only from the installer record and ignores generated shared settings', async ({ onTestFinished }) => {
    const test = await files(onTestFinished)
    await put(test.profileSettingsPath, JSON.stringify({ installs: [dirname(test.profile)] }))
    await put(join(dirname(test.profile), 'unrelated.json'), '{}')
    await put(join(dirname(test.profile), 'strata-unsloth.shared-settings.json'), '{}')
    expect(await discoverStrataProfiles(test.profileSettingsPath)).toEqual([{ path: test.profile, label: 'unsloth' }])
    const result = await resolveStrataModel({ ...DEFAULT_STRATA_SETTINGS, modelPath: dirname(test.profile) }, join(test.userData, 'strata'))
    expect(result?.model.ggufPath).toBe(test.gguf)
  })
})

describe('Strata carrier ownership', () => {
  it('does not start on status/save and serves with bundled paths plus actual health context', async ({ onTestFinished }) => {
    const test = await managerFixture(onTestFinished)
    expect((await test.manager.status()).phase).toBe('unconfigured')
    await test.manager.save({ ...DEFAULT_STRATA_SETTINGS, modelPath: test.profile })
    expect(test.launch).not.toHaveBeenCalled()
    await test.manager.start()
    const spec = await test.spawned(1)
    expect(spec.executable).toBe(join(test.runtimeRoot, 'portablepython/python.exe'))
    expect(spec.args).toContain(join(test.runtimeRoot, 'server/serve/server.py'))
    expect(spec.args).toContain('31337')
    expect(spec.args).not.toContain('--open')
    expect(spec.environment.PYTHONDONTWRITEBYTECODE).toBe('1')
    expect(spec.environment.PYTHONNOUSERSITE).toBe('1')
    const config = z.object({ exe: z.string(), log: z.string() }).loose()
      .parse(JSON.parse(await readFile(join(test.userData, 'strata/run.json'), 'utf8')))
    expect(config.exe).toBe(join(test.runtimeRoot, 'engine/strata.exe'))
    expect(config.log.startsWith(join(test.userData, 'strata/logs') + sep)).toBe(true)
    expect(config).not.toHaveProperty('before_load')
    expect(config).not.toHaveProperty('api_key')
    expect(config).not.toHaveProperty('mcp_servers')
    expect(await test.manager.connection()).toEqual({ baseURL: 'http://127.0.0.1:31337/v1', model: 'qwen3.8-flash-next', contextWindow: 32768 })
    await test.manager.start()
    expect(test.launch).toHaveBeenCalledTimes(1)
    await test.manager.stop()
    expect(test.children[0]?.stop).toHaveBeenCalledTimes(1)
  })

  it('refuses a missing MTP before starting even a preparation child', async ({ onTestFinished }) => {
    const test = await managerFixture(onTestFinished, {}, false)
    await test.manager.save({ ...DEFAULT_STRATA_SETTINGS, modelPath: test.gguf })
    await expect(test.manager.start()).rejects.toThrow('MTP')
    expect(test.launch).not.toHaveBeenCalled()
  })

  it('completes both offline preparations before serving and reuses the private cache on restart', async ({ onTestFinished }) => {
    const test = await managerFixture(onTestFinished, {}, false)
    const mtp = join(test.weights, 'mtp-q2_0.gguf')
    await put(mtp, 'GGUF-fixture-mtp')
    await test.manager.save({ ...DEFAULT_STRATA_SETTINGS, modelPath: test.gguf, mtpPath: mtp })
    await test.manager.start()
    const main = await test.spawned(1)
    const mainOutput = main.args[main.args.indexOf('--out') + 1]
    expect(main.args).toContain(join(test.runtimeRoot, 'server/tools/iq_pack.py'))
    expect(mainOutput.startsWith(test.userData)).toBe(true)
    await preparedPack(mainOutput)
    test.children[0]?.finish()
    const draft = await test.spawned(2)
    const mtpOutput = draft.args[draft.args.indexOf('--out') + 1]
    expect(draft.args).toContain(join(test.runtimeRoot, 'server/tools/mtp_rt.py'))
    expect(mtpOutput.startsWith(test.userData)).toBe(true)
    await preparedMtp(mtpOutput)
    test.children[1]?.finish()
    const server = await test.spawned(3)
    expect(server.args).toContain('--engine')
    await vi.waitFor(async () => { expect((await test.manager.status()).phase).toBe('running') })
    expect((await test.manager.status()).model?.needsPreparation).toBe(false)
    await test.manager.stop()
    await test.manager.start()
    expect((await test.spawned(4)).args).toContain('--engine')
    expect(test.specs.filter(spec => spec.args.includes('--out'))).toHaveLength(2)
    expect(await readFile(test.gguf, 'utf8')).toBe('GGUF-fixture-shard-one')
    expect(await readFile(mtp, 'utf8')).toBe('GGUF-fixture-mtp')
  })

  it('retries failed preparation without accepting its partial files or launching a server', async ({ onTestFinished }) => {
    const test = await managerFixture(onTestFinished, {}, false)
    await preparedMtp(test.mtp)
    await test.manager.save({ ...DEFAULT_STRATA_SETTINGS, modelPath: test.gguf, mtpPath: test.mtp })
    await test.manager.start()
    const failed = await test.spawned(1)
    await preparedPack(failed.args[failed.args.indexOf('--out') + 1])
    test.children[0]?.finish(1)
    await vi.waitFor(async () => { expect((await test.manager.status()).phase).toBe('error') })
    expect(test.specs.some(spec => spec.args.includes('--engine'))).toBe(false)
    await test.manager.start()
    expect((await test.spawned(2)).args).toEqual(failed.args)
    await test.manager.stop()
    expect(test.children[1]?.stop).toHaveBeenCalledTimes(1)
  })

  for (const freeGiB of [18, 8]) {
    it(`refuses startup with only ${freeGiB} GiB free and leaves other processes untouched`, async ({ onTestFinished }) => {
      const test = await managerFixture(onTestFinished, { availableMemory: () => freeGiB * 2 ** 30 })
      await test.manager.save({ ...DEFAULT_STRATA_SETTINGS, modelPath: test.profile })
      await expect(test.manager.start()).rejects.toThrow('内存')
      expect(test.launch).not.toHaveBeenCalled()
      expect(test.unload).not.toHaveBeenCalled()
    })
  }

  it('leaves a running external Strata service untouched', async ({ onTestFinished }) => {
    const test = await managerFixture(onTestFinished, { readHealth: async () => liveHealth(), availableMemory: () => 8 * 2 ** 30 })
    await test.manager.save({ ...DEFAULT_STRATA_SETTINGS, modelPath: test.profile })
    expect((await test.manager.start()).phase).toBe('external')
    expect((await test.manager.stop()).server?.owned).toBe(false)
    await test.manager.close()
    expect(test.launch).not.toHaveBeenCalled()
    expect(test.unload).not.toHaveBeenCalled()
  })

  it('does not spawn after close overtakes a pending startup probe', async ({ onTestFinished }) => {
    const health = Promise.withResolvers<StrataHealth>()
    const probing = Promise.withResolvers<undefined>()
    let gated = false
    const test = await managerFixture(onTestFinished, { readHealth: async () => {
      if (!gated) throw new Error('not listening')
      probing.resolve(undefined)
      return health.promise
    } })
    await test.manager.save({ ...DEFAULT_STRATA_SETTINGS, modelPath: test.profile })
    gated = true
    const starting = test.manager.start()
    const rejected = expect(starting).rejects.toThrow('正在关闭')
    await probing.promise
    const closing = test.manager.close()
    health.reject(new Error('not listening'))
    await rejected
    await closing
    expect(test.launch).not.toHaveBeenCalled()
  })

  it('cancels owned GGUF preparation and waits for its child before returning', async ({ onTestFinished }) => {
    const test = await managerFixture(onTestFinished, {}, false)
    await preparedMtp(test.mtp)
    await test.manager.save({ ...DEFAULT_STRATA_SETTINGS, modelPath: test.gguf, mtpPath: test.mtp })
    await test.manager.start()
    const spec = await test.spawned(1)
    expect(spec.args).toContain(join(test.runtimeRoot, 'server/tools/iq_pack.py'))
    const child = test.children[0]
    expect(child).toBeDefined()
    const stopping = test.manager.stop()
    await stopping
    expect(child?.stop).toHaveBeenCalledTimes(1)
    expect(child?.child.finished).toBe(true)
    expect(test.specs.some(item => item.args.includes('--engine'))).toBe(false)
  })
})
