/** Range reads of one workspace file through the Host byte route, cached in fixed chunks. */

/** Bytes fetched per request. */
export const CHUNK_BYTES = 64 * 1024

/** The file changed on disk while it was being read; the viewer reloads from a fresh observation. */
export class FileChangedError extends Error {
  constructor() {
    super('The file changed on disk.')
    this.name = 'FileChangedError'
  }
}

/** The byte route answered with something other than the file. */
export class HttpStatusError extends Error {
  override name = 'HttpStatusError'
  constructor(readonly status: number) {
    super(`HTTP ${status}`)
  }
}

interface PendingChunk {
  readonly promise: Promise<Uint8Array>
  readonly controller: AbortController
  waiters: number
}

function abortError(): Error {
  const error = new Error('The request was aborted.')
  error.name = 'AbortError'
  return error
}

/** Reads one version of a file; every response must carry the same ETag as the first. */
export class ByteSource {
  private readonly pending = new Map<number, PendingChunk>()
  /** Loaded chunks in least-recently-used order. */
  private readonly loaded = new Map<number, Uint8Array>()
  private etag: string | undefined

  /**
   * @param url Byte route URL of the file.
   * @param size File size from the observation the viewer opened.
   * @param maxChunks Loaded chunks kept in memory.
   * @param fetcher Network access; tests pass a fake.
   */
  constructor(readonly url: string, readonly size: number, private readonly maxChunks = 96, private readonly fetcher: typeof fetch = (input, init) => fetch(input, init)) {}

  /**
   * @param offset First byte.
   * @param length Byte count; the result is shorter at the end of the file.
   * @param signal Optional cancellation signal.
   * @returns The bytes.
   */
  async read(offset: number, length: number, signal?: AbortSignal): Promise<Uint8Array> {
    if (signal?.aborted === true) throw abortError()
    const end = Math.min(this.size, offset + length)
    if (offset >= end) return new Uint8Array(0)
    const first = Math.floor(offset / CHUNK_BYTES)
    const last = Math.floor((end - 1) / CHUNK_BYTES)
    const chunks = await Promise.all(Array.from({ length: last - first + 1 }, (_unused, index) => this.chunk(first + index, signal)))
    const result = new Uint8Array(end - offset)
    let position = 0
    chunks.forEach((chunk, index) => {
      const start = (first + index) * CHUNK_BYTES
      const from = Math.max(offset, start) - start
      const to = Math.min(end, start + chunk.length) - start
      result.set(chunk.subarray(from, to), position)
      position += to - from
    })
    return result
  }

  /**
   * A loaded byte, for synchronous rendering.
   * @param offset File offset.
   * @returns The byte, or `undefined` while its chunk is not loaded.
   */
  byte(offset: number): number | undefined {
    return this.loaded.get(Math.floor(offset / CHUNK_BYTES))?.[offset % CHUNK_BYTES]
  }

  /** @param offset File offset. @returns Whether the chunk holding it is loaded. */
  has(offset: number): boolean {
    return this.loaded.has(Math.floor(offset / CHUNK_BYTES))
  }

  /**
   * Load the chunks covering a byte range.
   * @param offset First byte.
   * @param length Byte count.
   * @param signal Optional cancellation signal; aborts in-flight chunk fetches when no other caller awaits them.
   * @returns Completion once every chunk is loaded.
   */
  async load(offset: number, length: number, signal?: AbortSignal): Promise<void> {
    if (signal?.aborted === true) throw abortError()
    const end = Math.min(this.size, offset + length)
    const tasks: Promise<Uint8Array>[] = []
    for (let index = Math.floor(offset / CHUNK_BYTES); index * CHUNK_BYTES < end; index++) tasks.push(this.chunk(index, signal))
    await Promise.all(tasks)
  }

  private chunk(index: number, signal?: AbortSignal): Promise<Uint8Array> {
    if (signal?.aborted === true) return Promise.reject(abortError())
    const ready = this.loaded.get(index)
    if (ready !== undefined) {
      this.loaded.delete(index)
      this.loaded.set(index, ready)
      return Promise.resolve(ready)
    }
    let entry = this.pending.get(index)
    if (entry === undefined) {
      const start = index * CHUNK_BYTES
      const end = Math.min(this.size, start + CHUNK_BYTES) - 1
      const controller = new AbortController()
      const promise = this.fetcher(this.url, { headers: { Range: `bytes=${start}-${end}` }, cache: 'no-store', signal: controller.signal }).then(async (response) => {
        if (response.status !== 206 && response.status !== 200) throw response.status === 409 || response.status === 416 ? new FileChangedError() : new HttpStatusError(response.status)
        const etag = response.headers.get('etag') ?? ''
        this.etag ??= etag
        if (etag !== this.etag) throw new FileChangedError()
        const bytes = new Uint8Array(await response.arrayBuffer())
        const chunk = response.status === 200 ? bytes.slice(start, end + 1) : bytes
        if (chunk.length !== end - start + 1) throw new FileChangedError()
        this.loaded.set(index, chunk)
        while (this.loaded.size > this.maxChunks) this.loaded.delete(this.loaded.keys().next().value!)
        return chunk
      }).finally(() => {
        if (this.pending.get(index) === entry) this.pending.delete(index)
      })
      entry = { promise, controller, waiters: 0 }
      this.pending.set(index, entry)
    }
    const active = entry
    active.waiters++
    if (signal === undefined) {
      return active.promise.finally(() => { active.waiters-- })
    }
    return new Promise<Uint8Array>((resolve, reject) => {
      let settled = false
      const release = (): void => {
        if (settled) return
        settled = true
        signal.removeEventListener('abort', onAbort)
        active.waiters--
      }
      const onAbort = (): void => {
        release()
        if (active.waiters <= 0) active.controller.abort()
        reject(abortError())
      }
      signal.addEventListener('abort', onAbort, { once: true })
      active.promise.then((chunk) => { release(); resolve(chunk) }, (error: unknown) => { release(); reject(error) })
    })
  }
}
