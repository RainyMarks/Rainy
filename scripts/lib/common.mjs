/** Shared filesystem, hashing, path-boundary and CLI helpers for build and release scripts. */
import { createHash } from 'node:crypto'
import { createReadStream, realpathSync } from 'node:fs'
import { lstat, realpath } from 'node:fs/promises'
import { dirname, isAbsolute, relative, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

/**
 * Compute a streaming cryptographic digest (and byte count) of a file.
 * @param {string} path File path.
 * @param {string} [algorithm='sha256'] Hash algorithm supported by `node:crypto`.
 * @param {import('node:crypto').BinaryToTextEncoding} [encoding='hex'] Digest encoding.
 * @returns {Promise<{bytes: number, digest: string}>} Observed size and digest.
 */
export async function fileDigest(path, algorithm = 'sha256', encoding = 'hex') {
  const hash = createHash(algorithm)
  let bytes = 0
  for await (const chunk of createReadStream(path)) {
    hash.update(chunk)
    bytes += chunk.length
  }
  return { bytes, digest: hash.digest(encoding) }
}

/**
 * Compute a SHA-256 hex digest of an in-memory buffer or string.
 * @param {string | Uint8Array} data Bytes or UTF-8 string.
 * @returns {string} Lowercase hex SHA-256 digest.
 */
export function sha256Hex(data) {
  return createHash('sha256').update(data).digest('hex')
}

/**
 * Resolve `target` through any existing symlink/junction ancestors (works even when the leaf does not exist yet).
 * @param {string} target Path to resolve.
 * @returns {Promise<string>} Canonical path with real ancestors resolved.
 */
export async function resolveWithRealAncestors(target) {
  const full = resolve(target)
  const missing = []
  let current = full
  while (true) {
    try {
      const real = await realpath(current)
      return missing.length ? resolve(real, ...missing.reverse()) : real
    } catch (error) {
      if (error.code !== 'ENOENT' && error.code !== 'ENOTDIR') throw error
      const parent = dirname(current)
      if (parent === current) return full
      missing.push(relative(parent, current))
      current = parent
    }
  }
}

/**
 * Whether `candidate` lies strictly beneath `folder`, after resolving symlinks/junctions on existing ancestors.
 * @param {string} folder Owning directory.
 * @param {string} candidate Path to check.
 * @returns {Promise<boolean>} True only when `candidate` is strictly inside `folder`.
 */
export async function isInsidePath(folder, candidate) {
  const base = await resolveWithRealAncestors(folder)
  const target = await resolveWithRealAncestors(candidate)
  const child = relative(base, target)
  return child !== '' && !isAbsolute(child) && child !== '..' && !child.startsWith('..\\') && !child.startsWith('../')
}

/**
 * Whether `candidate` lies strictly outside `folder` (neither equal to it nor inside it), resolving symlinks/junctions.
 * @param {string} folder Protected directory.
 * @param {string} candidate Path to check.
 * @returns {Promise<boolean>} True only when `candidate` is neither `folder` nor inside `folder`.
 */
export async function isOutsidePath(folder, candidate) {
  const base = await resolveWithRealAncestors(folder)
  const target = await resolveWithRealAncestors(candidate)
  const child = relative(base, target)
  return child !== '' && (isAbsolute(child) || child === '..' || child.startsWith('..\\') || child.startsWith('../'))
}

/**
 * Whether `path` exists (without following a trailing symlink).
 * @param {string} path Path to test.
 * @returns {Promise<boolean>} True if `lstat` succeeds.
 */
export async function pathExists(path) {
  try {
    await lstat(path)
    return true
  } catch (error) {
    if (error.code === 'ENOENT') return false
    throw error
  }
}

function normalizeForEntryComparison(filePath) {
  let canonical = resolve(filePath)
  try {
    canonical = realpathSync(canonical)
  } catch {
    // Keep resolved path when file cannot be realpath'd.
  }
  return process.platform === 'win32' ? canonical.toLowerCase() : canonical
}

/**
 * Case-insensitive (on Windows) and symlink-aware check for whether the current module is the Node CLI entry point.
 * @param {string} metaUrl Caller's `import.meta.url`.
 * @param {string} [argv1=process.argv[1]] CLI script path (`process.argv[1]`).
 * @returns {boolean} True when executed directly as the main script.
 */
export function isMainEntry(metaUrl, argv1 = process.argv[1]) {
  if (!argv1) return false
  return normalizeForEntryComparison(fileURLToPath(metaUrl)) === normalizeForEntryComparison(argv1)
}

/**
 * Parse `--flag value` pairs and reject unknown flags or missing values.
 * @param {string[]} argv Arguments after the script name.
 * @param {string[]} allowed Permitted flag names.
 * @param {string} usage Usage message shown on argument-count mismatch.
 * @returns {Record<string, string>} Parsed flag values.
 */
export function parseFlagPairs(argv, allowed, usage) {
  if (argv.length % 2 !== 0) throw new Error(usage)
  const allowedSet = new Set(allowed)
  const values = {}
  for (let index = 0; index < argv.length; index += 2) {
    const name = argv[index]
    const value = argv[index + 1]
    if (!allowedSet.has(name)) throw new Error(`Unknown option: ${name}`)
    values[name] = value
  }
  return values
}
