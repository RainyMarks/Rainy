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

/** Reads one version of a file; every response must carry the same ETag as the first. */
export class ByteSource {
  private readonly pending = new Map<number, Promise<Uint8Array>>()
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
   * @returns The bytes.
   */
  async read(offset: number, length: number): Promise<Uint8Array> {
    const end = Math.min(this.size, offset + length)
    if (offset >= end) return new Uint8Array(0)
    const first = Math.floor(offset / CHUNK_BYTES)
    const last = Math.floor((end - 1) / CHUNK_BYTES)
    const chunks = await Promise.all(Array.from({ length: last - first + 1 }, (_unused, index) => this.chunk(first + index)))
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
   * @returns Completion once every chunk is loaded.
   */
  async load(offset: number, length: number): Promise<void> {
    const end = Math.min(this.size, offset + length)
    const tasks: Promise<Uint8Array>[] = []
    for (let index = Math.floor(offset / CHUNK_BYTES); index * CHUNK_BYTES < end; index++) tasks.push(this.chunk(index))
    await Promise.all(tasks)
  }

  private chunk(index: number): Promise<Uint8Array> {
    const ready = this.loaded.get(index)
    if (ready !== undefined) {
      this.loaded.delete(index)
      this.loaded.set(index, ready)
      return Promise.resolve(ready)
    }
    const running = this.pending.get(index)
    if (running !== undefined) return running
    const start = index * CHUNK_BYTES
    const end = Math.min(this.size, start + CHUNK_BYTES) - 1
    const task = this.fetcher(this.url, { headers: { Range: `bytes=${start}-${end}` }, cache: 'no-store' }).then(async (response) => {
      if (response.status !== 206 && response.status !== 200) throw response.status === 409 || response.status === 416 ? new FileChangedError() : new Error(`HTTP ${response.status}`)
      const etag = response.headers.get('etag') ?? ''
      this.etag ??= etag
      if (etag !== this.etag) throw new FileChangedError()
      const bytes = new Uint8Array(await response.arrayBuffer())
      const chunk = response.status === 200 ? bytes.slice(start, end + 1) : bytes
      if (chunk.length !== end - start + 1) throw new FileChangedError()
      this.loaded.set(index, chunk)
      while (this.loaded.size > this.maxChunks) this.loaded.delete(this.loaded.keys().next().value!)
      return chunk
    }).finally(() => { this.pending.delete(index) })
    this.pending.set(index, task)
    return task
  }
}
