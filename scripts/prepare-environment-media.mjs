/** Download and authenticate the fixed offline WSL/Ubuntu installation inputs. */
import { createHash } from 'node:crypto'
import { createReadStream, createWriteStream } from 'node:fs'
import { mkdir, readFile, rename, stat, writeFile } from 'node:fs/promises'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { pipeline } from 'node:stream/promises'
import { Readable } from 'node:stream'
import { execFile } from 'node:child_process'
import { promisify } from 'node:util'
import { environmentMedia } from '../src/main/environment-media.ts'

const exec = promisify(execFile)
const root = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const destination = resolve(process.argv[2] ?? join(root, 'runtime', 'environment'))
const evidence = join(root, 'validation', 'environment')
const fingerprint = '843938DF228D22F7B3742BC0D94AA3F0EFE21092'
await mkdir(destination, { recursive: true })
await mkdir(evidence, { recursive: true })

async function sha256(path) {
  const hash = createHash('sha256')
  for await (const chunk of createReadStream(path)) hash.update(chunk)
  return hash.digest('hex')
}

async function present(path) {
  try { return await stat(path) } catch (error) { if (error.code === 'ENOENT') return undefined; throw error }
}

async function download(url, file, expected) {
  const target = join(destination, file)
  if (await present(target)) {
    if (!expected || ((await stat(target)).size === expected.bytes && await sha256(target) === expected.sha256)) return target
    throw new Error(`Existing media did not match the pinned checksum: ${target}`)
  }
  const temporary = `${target}.part`
  const partial = await present(temporary)
  const offset = partial?.size ?? 0
  const response = await fetch(url, { headers: offset ? { Range: `bytes=${offset}-` } : {} })
  if (!response.ok || !response.body) throw new Error(`${response.status} downloading ${url}`)
  const append = offset > 0 && response.status === 206
  if (append && !response.headers.get('content-range')?.startsWith(`bytes ${offset}-`)) throw new Error('Unexpected partial download range')
  console.log(`Downloading ${file}${append ? ` (resuming at ${offset} bytes)` : ''}`)
  await pipeline(Readable.fromWeb(response.body), createWriteStream(temporary, { flags: append ? 'a' : 'w' }))
  if (expected && ((await stat(temporary)).size !== expected.bytes || await sha256(temporary) !== expected.sha256)) throw new Error(`Downloaded media checksum mismatch: ${file}`)
  await rename(temporary, target)
  return target
}

const sums = await download('https://releases.ubuntu.com/26.04.1/SHA256SUMS', 'SHA256SUMS')
const signature = await download('https://releases.ubuntu.com/26.04.1/SHA256SUMS.gpg', 'SHA256SUMS.gpg')
const key = await download(`https://keyserver.ubuntu.com/pks/lookup?op=get&search=0x${fingerprint}`, 'ubuntu-signing-key.asc')
const gpgHome = join(evidence, 'gnupg')
await mkdir(gpgHome, { recursive: true, mode: 0o700 })
const gpg = process.env.RAINY_GPG ?? (process.platform === 'win32' ? 'C:/msys64/usr/bin/gpg.exe' : 'gpg')
const gpgPath = path => /msys/i.test(gpg) ? path.replaceAll('\\', '/').replace(/^([A-Za-z]):\//, (_, drive) => `/${drive.toLowerCase()}/`) : path
const gpgArgs = ['--no-options', '--homedir', gpgPath(gpgHome), '--batch', '--no-tty', '--no-autostart', '--no-auto-key-retrieve']
const keyDetails = await exec(gpg, [...gpgArgs, '--with-colons', '--import-options', 'show-only', '--import', gpgPath(key)], { windowsHide: true })
if (!keyDetails.stdout.includes(`fpr:::::::::${fingerprint}:`)) throw new Error('Ubuntu signing key fingerprint does not match the pinned release key')
// The inspected public-key file provides a dedicated keyring without requiring a secret-key agent.
const keyring = join(gpgHome, 'ubuntu-release.gpg')
await exec(gpg, [...gpgArgs, '--yes', '--dearmor', '--output', gpgPath(keyring), gpgPath(key)], { windowsHide: true })
const verified = await exec(gpg, [...gpgArgs, '--no-default-keyring', '--keyring', gpgPath(keyring),
  '--status-fd', '1', '--verify', gpgPath(signature), gpgPath(sums)], { windowsHide: true })
if (!verified.stdout.includes(`[GNUPG:] VALIDSIG ${fingerprint} `)) throw new Error('Ubuntu signed checksum verification did not confirm the expected key')
const checksumText = await readFile(sums, 'utf8')
if (!checksumText.split(/\r?\n/).some(line => line === `${environmentMedia.ubuntu.sha256} *${environmentMedia.ubuntu.file}` || line === `${environmentMedia.ubuntu.sha256}  ${environmentMedia.ubuntu.file}`)) throw new Error('The signed Ubuntu manifest does not contain the pinned WSL image checksum')

const paths = await Promise.all(Object.values(environmentMedia).map(item => download(item.url, item.file, item)))
if (process.platform !== 'win32') throw new Error('Authenticode validation requires Windows; media is downloaded but packaging is not approved')
const msi = join(destination, environmentMedia.wsl.file).replaceAll("'", "''")
const authScript = `$signature = Get-AuthenticodeSignature -LiteralPath '${msi}'; if ($signature.Status -ne 'Valid' -or $signature.SignerCertificate.Subject -notmatch 'O=Microsoft Corporation') { throw 'Microsoft Authenticode validation failed' }; @{ status = [string]$signature.Status; subject = $signature.SignerCertificate.Subject; thumbprint = $signature.SignerCertificate.Thumbprint } | ConvertTo-Json -Compress`
const authenticode = await exec('powershell.exe', ['-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-Command', authScript], { windowsHide: true, env: Object.fromEntries(Object.entries(process.env).filter(([name]) => !/^PSModulePath$/i.test(name))) })
const manifest = {
  formatVersion: 1,
  verifiedAt: new Date().toISOString(),
  ubuntuSignatureFingerprint: fingerprint,
  wslSignature: JSON.parse(authenticode.stdout),
  media: Object.values(environmentMedia),
  hostRequirements: { bash: true, minimumPython: '3.12' },
}
await writeFile(join(destination, 'media-verification.json'), JSON.stringify(manifest, null, 2) + '\n')
await writeFile(join(evidence, 'media-verification.json'), JSON.stringify(manifest, null, 2) + '\n')
console.log(JSON.stringify({ verified: true, destination, paths }, null, 2))
