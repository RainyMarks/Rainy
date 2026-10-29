/**
 * Build the viewer page (`dist/renderer`) the Host serves to the native workbench's panes: documents, media, Markdown
 * and the project map. With `--release`, first write the release public keys (`runtime/release-public-keys.json`, which
 * the native build embeds) and refuse missing or changed release resources (the Linux IDE tools).
 */
import { spawn } from 'node:child_process'
import { existsSync } from 'node:fs'
import { readFile } from 'node:fs/promises'
import { resolve } from 'node:path'

const root = resolve(import.meta.dirname, '..')
const release = process.argv.includes('--release')

/**
 * Run one Node script with inherited output.
 * @param script Script path relative to the repository root.
 * @param args Script arguments.
 * @returns Resolves when the script exits with code 0.
 */
function node(script: string, ...args: string[]): Promise<void> {
  return new Promise((accept, reject) => {
    const child = spawn(process.execPath, [resolve(root, script), ...args], { cwd: root, stdio: 'inherit' })
    child.once('error', reject)
    child.once('exit', (code, signal) => {
      if (code === 0) accept()
      else reject(new Error(`${script} failed with ${signal ?? `exit code ${String(code)}`}`))
    })
  })
}

async function main(): Promise<void> {
  const manifest: { version?: unknown } = JSON.parse(await readFile(resolve(root, 'package.json'), 'utf8'))
  if (typeof manifest.version !== 'string' || !/^\d+\.\d+\.\d+(?:-[\w.-]+)?$/.test(manifest.version)) {
    throw new Error('package.json must declare a valid application version')
  }
  if (release) await node('scripts/release-signing-key.mjs')
  // Release inputs are restored by bootstrap-release-inputs.mjs; a development checkout may not have them.
  for (const [directory, script] of [['resources/ide', 'scripts/verify-ide-resources.mjs']] as const) {
    if (release || existsSync(resolve(root, directory))) await node(script)
    else console.log(`Skipped ${script}: ${directory} is not prepared.`)
  }
  await node('node_modules/vite/bin/vite.js', 'build', '--config', resolve(root, 'vite.config.ts'))
  console.log('Built the viewer page in dist/renderer.')
}

try { await main() }
catch (error) {
  // The child scripts have already printed their diagnostics.
  console.error(error instanceof Error ? error.message : String(error))
  process.exitCode = 1
}
