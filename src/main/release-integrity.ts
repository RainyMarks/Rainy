/** Authenticated inventory for Rainy-owned release resources; user data is never inventoried. */
import { createHash, verify } from 'node:crypto'
import { createReadStream } from 'node:fs'
import { lstat, readFile, realpath, rm } from 'node:fs/promises'
import { dirname, isAbsolute, relative, resolve } from 'node:path'
import { z } from 'zod'
import type { ReleaseKeyring } from './release-trust.ts'
import { writePrivateRecord } from './protected-json.ts'

/** Domain separation restricts signatures to RainyAgent resource inventories. */
export const RELEASE_SIGNATURE_DOMAIN = 'RainyAgent/release-manifest/v1\0'
/** Paths are relative to the signed resource root and cannot escape it. */
export const releaseManifestSchema = z.object({ version: z.literal(1), product: z.literal('RainyAgent'),
  buildVersion: z.string().min(1).max(100), createdAt: z.iso.datetime(), keyId: z.string().regex(/^[a-f0-9]{32}$/),
  files: z.array(z.object({ path: z.string().min(1).max(4096).refine(value => !value.includes('\\') && !value.includes('\0')
    && !value.includes(':') && !value.startsWith('/') && !value.split('/').some(part => !part || part === '.' || part === '..')),
  sha256: z.string().regex(/^[a-f0-9]{64}$/), bytes: z.number().int().nonnegative() }).strict()).min(1).max(100000),
}).strict().refine(value => new Set(value.files.map(file => file.path.toLowerCase())).size === value.files.length, '资源清单包含重复路径')

/** A release resource is missing, replaced, outside its root, or has different bytes than its signed inventory. */
export class ReleaseIntegrityError extends Error {
  override name = 'ReleaseIntegrityError'
}

/** Canonical payload signed by the build's release key. */
export type ReleaseManifest = z.infer<typeof releaseManifestSchema>

const signedManifestSchema = z.object({ version: z.literal(1), payload: z.string().max(32 * 1024 * 1024), signature: z.string().max(128) })
  .strict()

/**
 * Authenticate an inventory against keys embedded in the client executable bundle.
 * @param input - untrusted signed JSON read from release resources.
 * @param keys - release-owned public keys, independent of the manifest.
 * @returns the authenticated resource list.
 */
export function authenticateReleaseManifest(input: unknown, keys: ReleaseKeyring): ReleaseManifest {
  const signed = signedManifestSchema.parse(input)
  const bytes = Buffer.from(signed.payload, 'base64url')
  const signature = Buffer.from(signed.signature, 'base64url')
  if (bytes.toString('base64url') !== signed.payload || signature.toString('base64url') !== signed.signature || signature.length !== 64) throw new Error('发行清单编码无效')
  const manifest = releaseManifestSchema.parse(JSON.parse(bytes.toString('utf8')))
  const key = keys.keys[manifest.keyId]
  if (!key || !verify(null, Buffer.concat([Buffer.from(RELEASE_SIGNATURE_DOMAIN), bytes]), key, signature)) throw new Error('发行清单签名无效')
  return manifest
}

/** Parallel file work; the first failure stops the remaining items. */
async function forEachConcurrently<T>(items: readonly T[], concurrency: number,
  work: (item: T, index: number) => Promise<void>): Promise<void> {
  // Workers share one iterator, so each item is taken exactly once.
  const pending = items.entries()
  let failed = false
  const worker = async (): Promise<void> => {
    for (const [index, item] of pending) {
      if (failed) return
      try { await work(item, index) }
      catch (error) { failed = true; throw error }
    }
  }
  await Promise.all(Array.from({ length: Math.max(1, Math.min(concurrency, items.length)) }, worker))
}

interface InventoryWalk {
  readonly canonicalRoot: string
  /** Resolve one inventory path and refuse escapes, links, and replaced parent directories. */
  locate(expected: ReleaseManifest['files'][number]): Promise<{ path: string; signature: string }>
}

async function walkInventory(root: string): Promise<InventoryWalk> {
  const canonicalRoot = await realpath(root)
  // Parent directories are shared by thousands of files, so each one is resolved once.
  const directories = new Map<string, Promise<string>>()
  return {
    canonicalRoot,
    async locate(expected) {
      const path = resolve(canonicalRoot, ...expected.path.split('/'))
      const local = relative(canonicalRoot, path)
      if (!local || isAbsolute(local) || local === '..' || local.startsWith('../') || local.startsWith('..\\')) throw new ReleaseIntegrityError('发行资源路径越界')
      const info = await lstat(path, { bigint: true }).catch((error: unknown) => {
        if (error instanceof Error && 'code' in error && (error.code === 'ENOENT' || error.code === 'ENOTDIR'))
          throw new ReleaseIntegrityError(`发行资源缺失：${expected.path}`)
        throw error
      })
      if (!info.isFile() || info.isSymbolicLink() || info.size !== BigInt(expected.bytes)) throw new ReleaseIntegrityError(`发行资源损坏：${expected.path}`)
      const parent = dirname(path)
      let canonicalParent = directories.get(parent)
      if (canonicalParent === undefined) { canonicalParent = realpath(parent); directories.set(parent, canonicalParent) }
      if (await canonicalParent !== parent) throw new ReleaseIntegrityError(`发行资源路径被替换：${expected.path}`)
      return { path, signature: `${expected.path}\0${info.size}\0${info.mtimeNs}\0${info.ino}` }
    },
  }
}

function fingerprintOf(signatures: readonly string[]): string {
  return createHash('sha256').update(signatures.join('\n')).digest('hex')
}

/** Options for one complete verification pass. */
export interface ReleaseVerificationOptions {
  /** Files hashed at the same time. */
  readonly concurrency?: number
  /** Observe progress. @param completed - verified files. @param total - inventoried files. */
  readonly onProgress?: (completed: number, total: number) => void
}

/**
 * Check signed resources before starting their processes; symbolic links and path escapes are refused.
 * @param root - release-owned resource directory.
 * @param signedPath - signed inventory path.
 * @param keys - public keys embedded in the carrier bundle.
 * @param options - parallelism and progress reporting.
 * @returns inventory metadata after every declared resource matches.
 */
export async function verifyReleaseResources(root: string, signedPath: string, keys: ReleaseKeyring,
  options: ReleaseVerificationOptions = {}): Promise<ReleaseManifest> {
  return (await verifyInventory(root, authenticateReleaseManifest(JSON.parse(await readFile(signedPath, 'utf8')), keys), options)).manifest
}

async function verifyInventory(root: string, manifest: ReleaseManifest, options: ReleaseVerificationOptions):
Promise<{ manifest: ReleaseManifest; fingerprint: string }> {
  const walk = await walkInventory(root)
  const signatures = new Array<string>(manifest.files.length)
  let completed = 0
  await forEachConcurrently(manifest.files, options.concurrency ?? 8, async (expected, index) => {
    const { path, signature } = await walk.locate(expected)
    const hash = createHash('sha256')
    for await (const chunk of createReadStream(path, { highWaterMark: 1024 * 1024 })) {
      if (!Buffer.isBuffer(chunk)) throw new Error('发行资源读取格式无效')
      hash.update(chunk)
    }
    if (hash.digest('hex') !== expected.sha256) throw new ReleaseIntegrityError(`发行资源校验失败：${expected.path}`)
    signatures[index] = signature
    options.onProgress?.(++completed, manifest.files.length)
  })
  return { manifest, fingerprint: fingerprintOf(signatures) }
}

const stampSchema = z.object({ version: z.literal(2), manifestSha256: z.string().regex(/^[a-f0-9]{64}$/), root: z.string().min(1),
  fingerprint: z.string().regex(/^[a-f0-9]{64}$/), checkedAt: z.number().int().nonnegative() }).strict()
/** A completed full verification of one signed inventory at one installation root. */
export type ReleaseVerificationStamp = z.infer<typeof stampSchema>

async function readStamp(path: string): Promise<ReleaseVerificationStamp | undefined> {
  try { return stampSchema.parse(JSON.parse(await readFile(path, 'utf8'))) }
  catch (_unusableStamp) { return undefined } // Missing, older or damaged stamps only cause one full verification.
}

async function writeStamp(path: string, stamp: ReleaseVerificationStamp): Promise<void> {
  await writePrivateRecord(path, Buffer.from(JSON.stringify(stamp) + '\n'))
}

/** Release resources checked once per signed inventory, then confirmed cheaply in the background. */
export interface ReleaseIntegrityOptions extends ReleaseVerificationOptions {
  readonly root: string
  readonly signedPath: string
  readonly keys: ReleaseKeyring
  /** Private record of the last completed verification. */
  readonly stampPath: string
  /** Minimum time between background consistency checks. */
  readonly recheckIntervalMs: number
  readonly now?: () => number
}

/** Outcome of startup admission. */
export interface ReleaseIntegrity {
  /** Whether startup hashed every resource or reused a stamp for this exact inventory. */
  readonly verified: 'full' | 'stamp'
  readonly manifest: ReleaseManifest
  /**
   * Compare file metadata with the stamp when it is due; changed metadata triggers a full re-verification.
   * @returns 'skipped' when not due, 'unchanged', or 'reverified'; a failed re-verification rejects and removes the stamp.
   */
  recheck(): Promise<'skipped' | 'unchanged' | 'reverified'>
}

/**
 * Admit release resources at startup. A full verification runs only for an inventory without a matching stamp,
 * so an installed or updated release is hashed once instead of on every launch.
 * @param options - resource paths, trust anchors, stamp location and pacing.
 * @returns the authenticated inventory and its deferred consistency check.
 */
export async function ensureReleaseIntegrity(options: ReleaseIntegrityOptions): Promise<ReleaseIntegrity> {
  const now = options.now ?? Date.now
  const envelope: unknown = JSON.parse(await readFile(options.signedPath, 'utf8'))
  const manifest = authenticateReleaseManifest(envelope, options.keys)
  const manifestSha256 = createHash('sha256').update(signedManifestSchema.parse(envelope).payload).digest('hex')
  const root = await realpath(options.root)
  let stamp = await readStamp(options.stampPath)
  let verified: ReleaseIntegrity['verified'] = 'stamp'
  if (stamp === undefined || stamp.manifestSha256 !== manifestSha256 || stamp.root !== root) {
    const { fingerprint } = await verifyInventory(root, manifest, options)
    stamp = { version: 2, manifestSha256, root, fingerprint, checkedAt: now() }
    await writeStamp(options.stampPath, stamp)
    verified = 'full'
  }
  let current = stamp
  return {
    verified,
    manifest,
    async recheck() {
      if (now() - current.checkedAt < options.recheckIntervalMs) return 'skipped'
      try {
        const walk = await walkInventory(root)
        const signatures = new Array<string>(manifest.files.length)
        await forEachConcurrently(manifest.files, options.concurrency ?? 8, async (expected, index) => {
          signatures[index] = (await walk.locate(expected)).signature
        })
        let outcome: 'unchanged' | 'reverified' = 'unchanged'
        let fingerprint = fingerprintOf(signatures)
        if (fingerprint !== current.fingerprint) {
          fingerprint = (await verifyInventory(root, manifest, options)).fingerprint
          outcome = 'reverified'
        }
        current = { ...current, fingerprint, checkedAt: now() }
        await writeStamp(options.stampPath, current)
        return outcome
      } catch (error) {
        // The next launch verifies every resource again before starting any process.
        await rm(options.stampPath, { force: true })
        throw error
      }
    },
  }
}
