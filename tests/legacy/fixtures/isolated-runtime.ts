/** Repository-independent working directories and package-resolution checks for shipped Host tests. */
import assert from 'node:assert/strict'
import { execFile } from 'node:child_process'
import { cp } from 'node:fs/promises'
import { promisify } from 'node:util'
import { isAbsolute, join, relative, resolve } from 'node:path'
import { z } from 'zod'

const execute = promisify(execFile)
const repository = resolve(import.meta.dirname, '../../../..')
/** @param directory - owned temporary directory. @returns the previous working directory for teardown. */
export function useExternalWorkingDirectory(directory: string): string {
  const location = relative(repository, directory)
  assert(location === '..' || location.startsWith('..\\') || location.startsWith('../') || isAbsolute(location))
  const previous = process.cwd()
  for (const key of Object.keys(process.env)) {
    if (/^(?:NODE_PATH$|NODE_OPTIONS$|DSH_|RAINY_LICENSE_)/iu.test(key)) Reflect.deleteProperty(process.env, key)
  }
  process.chdir(directory)
  return previous
}

/** @param source - shipped Windows Host tree. @param directory - owned external test directory. @returns its physical copy. */
export async function copyExternalRuntime(source: string, directory: string): Promise<string> {
  const runtime = join(directory, 'runtime')
  await cp(source, runtime, { recursive: true, errorOnExist: true, force: false })
  return runtime
}

/** Plain Node probe used with both shipped platform binaries, without the source test driver's loader. */
export const runtimeResolutionProbe = `
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { dirname, isAbsolute, join, relative, resolve } from 'node:path';
import { realpathSync } from 'node:fs';
import { pathToFileURL } from 'node:url';
const runtime = realpathSync(process.argv[1]);
const anchor = join(runtime, 'app/package.json');
const within = path => { const p = relative(runtime, realpathSync(path)); return !isAbsolute(p) && p !== '..' && !p.startsWith('../') && !p.startsWith('..\\\\'); };
const require = createRequire(anchor);
const bootPath = require.resolve('@deepseek-ai/dsh-app-boot');
assert(within(bootPath));
const boot = await import(pathToFileURL(bootPath).href);
const bundleDirectory = realpathSync(boot.resolveBundleDir('external smoke', '@deepseek-ai/dsh-rainy-desktop', anchor, join(process.cwd(), 'profile')));
assert.equal(bundleDirectory, realpathSync(join(runtime, 'app')));
const resolution = await boot.createRuntimeResolution({ installAnchor: anchor, home: process.cwd() });
const escaped = resolution.entries.filter(entry => !within(entry.packageDir)).map(entry => entry.name);
assert.deepEqual(escaped, []);
console.log(JSON.stringify({ cwd: process.cwd(), bundleDirectory, installationPackages: resolution.entries.length, allPackagesInsideRuntime: true }));
`

/** Parsed observations from the copied runtime's own Node process. */
export const runtimeResolutionSchema = z.object({
  cwd: z.string(), bundleDirectory: z.string(), installationPackages: z.number(), allPackagesInsideRuntime: z.literal(true),
}).strict()

/**
 * @param runtime - physical Windows Host outside the repository.
 * @param cwd - external working directory.
 * @returns the copied Node's resolution observations.
 */
export async function inspectWindowsRuntime(runtime: string, cwd: string): Promise<z.infer<typeof runtimeResolutionSchema>> {
  const result = await execute(join(runtime, 'node/node.exe'), ['--input-type=module', '-e', runtimeResolutionProbe, runtime],
    { cwd, env: process.env, windowsHide: true, timeout: 120000, maxBuffer: 1024 * 1024 })
  return runtimeResolutionSchema.parse(JSON.parse(result.stdout))
}
