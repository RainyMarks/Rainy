/** Atomic JSON files under the Host home. */
import { randomUUID } from 'node:crypto'
import { mkdir, open, readFile, rename, rm } from 'node:fs/promises'
import { dirname } from 'node:path'
import { setTimeout as delay } from 'node:timers/promises'

/** Codes Windows gives a rename onto a file another program holds open for a moment, such as a virus scanner. */
const BUSY_CODES: ReadonlySet<string | undefined> = new Set(['EPERM', 'EACCES', 'EBUSY'])

/**
 * Read and parse a JSON file.
 * @param path File path.
 * @returns The parsed value, or `undefined` when the file does not exist.
 */
export async function readJson(path: string): Promise<unknown> {
  let text: string
  try { text = await readFile(path, 'utf8') } catch (error) {
    if ((error as NodeJS.ErrnoException).code === 'ENOENT') return undefined
    throw error
  }
  return JSON.parse(text)
}

/**
 * Replace a file's content atomically: write a sibling temp file, flush it, then rename.
 * @param path Destination path; parent directories are created.
 * @param content Bytes or text to store.
 * @param mode File mode for a newly created file.
 */
export async function writeFileAtomic(path: string, content: string | Uint8Array, mode = 0o600): Promise<void> {
  await mkdir(dirname(path), { recursive: true, mode: 0o700 })
  const temporary = `${path}.${randomUUID()}.tmp`
  const handle = await open(temporary, 'wx', mode)
  try {
    await handle.writeFile(content)
    await handle.sync()
  } catch (error) {
    await handle.close().catch(() => undefined)
    await rm(temporary, { force: true })
    throw error
  }
  await handle.close()
  for (let attempt = 1; ; attempt++) {
    try {
      await rename(temporary, path)
      return
    } catch (error) {
      if (process.platform === 'win32' && attempt < 6 && BUSY_CODES.has((error as NodeJS.ErrnoException).code)) { await delay(50 * attempt); continue }
      await rm(temporary, { force: true })
      throw error
    }
  }
}

/**
 * Serialize a value as indented JSON and store it atomically.
 * @param path Destination path.
 * @param value JSON-serializable value.
 */
export async function writeJson(path: string, value: unknown): Promise<void> {
  await writeFileAtomic(path, `${JSON.stringify(value, null, 2)}\n`)
}

/** Runs async tasks one at a time in submission order. */
export class SerialQueue {
  private tail: Promise<unknown> = Promise.resolve()

  /**
   * Queue a task after every task submitted before it.
   * @param task Work to run.
   * @returns The task's result.
   */
  run<T>(task: () => Promise<T>): Promise<T> {
    const result = this.tail.then(task, task)
    this.tail = result.catch(() => undefined)
    return result
  }
}
