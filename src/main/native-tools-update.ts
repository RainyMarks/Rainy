/** Authenticate a tool-channel revision before accepting new launch entries and archive hashes. */
import { createHash, verify } from 'node:crypto'
import { z } from 'zod'
import type { ReleaseKeyring } from './release-trust.ts'
import { TOOL_UNIT_ARCHIVE, toolPackMetadataSchema } from './toolpack-format.ts'
import type { ToolPackMetadataV2 } from './toolpack-format.ts'
import { parseNativeToolCatalog } from './native-tools.ts'

/** Tool-channel signatures cannot be reused as application resource signatures or as earlier channel formats. */
export const TOOL_CHANNEL_SIGNATURE_DOMAIN = 'RainyAgent/tool-channel/v2\0'
/** Publisher location of the newest signed per-tool channel. */
export const TOOL_CHANNEL_URL = 'https://raw.githubusercontent.com/RainyMarks/RainyAgent/main/apps/rainy-desktop/toolpacks/native-tools-channel.v2.signed.json'
const hash = z.string().regex(/^[a-f0-9]{64}$/)
const keyId = z.string().regex(/^[a-f0-9]{32}$/)
const bytes = z.number().int().nonnegative().max(Number.MAX_SAFE_INTEGER)
/** Download location of each unit archive; unchanged archives stay in the release that first published them. */
export const toolDownloadSourceSchema = z.object({ version: z.literal(2), packId: hash,
  archives: z.array(z.object({ file: z.string().regex(TOOL_UNIT_ARCHIVE), bytes, sha256: hash,
    baseUrl: z.string().regex(/^https:\/\/github\.com\/RainyMarks\/RainyAgent\/releases\/download\/v\d+\.\d+\.\d+-resources\/$/),
    pieces: z.array(z.object({ file: z.string().regex(/^rainy-[a-f0-9]{20}\.\d{3}$/), bytes, sha256: hash }).strict()).min(1),
  }).strict()).min(1),
}).strict()
/** Validated downloadable archive selection. */
export type NativeToolsDownloadSource = z.infer<typeof toolDownloadSourceSchema>
const channelSchema = z.object({ version: z.literal(2), revision: z.number().int().positive(),
  releaseVersion: z.string().regex(/^\d+\.\d+\.\d+$/), keyId,
  source: toolDownloadSourceSchema, metadata: toolPackMetadataSchema, catalog: z.string().min(1),
}).strict()
/** Authenticated channel revision, including exact catalog bytes and per-unit installation metadata. */
export type NativeToolsChannel = Omit<z.infer<typeof channelSchema>, 'metadata'> & { readonly metadata: ToolPackMetadataV2 }

/** Check that every unit archive has one download entry with the same size and digest.
 * @param source - validated transport selection.
 * @param metadata - validated installation inventory.
 * @returns the per-unit inventory.
 */
export function validateToolDownloadInputs(
  source: NativeToolsDownloadSource, metadata: z.infer<typeof toolPackMetadataSchema>,
): ToolPackMetadataV2 {
  if (metadata.version !== 2 || source.packId !== metadata.id || source.archives.length !== metadata.archives.length) throw new Error('工具下载清单与当前工具包不匹配')
  const names = new Set<string>()
  for (const archive of metadata.archives) {
    const download = source.archives.find(entry => entry.file === archive.file)
    if (!download || download.bytes !== archive.bytes || download.sha256 !== archive.sha256
      || download.pieces.reduce((sum, piece) => sum + piece.bytes, 0) !== download.bytes) throw new Error('工具下载分卷校验信息不匹配')
    for (const piece of download.pieces) {
      if (names.has(piece.file)) throw new Error('工具下载分片重复')
      names.add(piece.file)
    }
  }
  return metadata
}

/** Verify a revision using only the carrier's trust anchors, then validate every installation input.
 * @param input - untrusted channel envelope.
 * @param keys - public keys embedded in the executable.
 * @returns authenticated tool channel; unsigned, modified or mismatched inputs reject.
 */
export function authenticateToolChannel(input: unknown, keys: ReleaseKeyring): NativeToolsChannel {
  const envelope = z.object({ version: z.literal(2), payload: z.string().max(14 * 1024 ** 2),
    signature: z.string().max(200) }).strict().parse(input)
  const payload = Buffer.from(envelope.payload, 'base64')
  const raw: unknown = JSON.parse(payload.toString('utf8'))
  const identity = z.object({ keyId }).parse(raw)
  const key = keys.keys[identity.keyId]
  if (!key || !verify(null, Buffer.concat([Buffer.from(TOOL_CHANNEL_SIGNATURE_DOMAIN), payload]), key, Buffer.from(envelope.signature, 'base64'))) {
    throw new Error('工具更新签名无效，请重试或更新 RainyAgent')
  }
  const channel = channelSchema.parse(raw)
  const metadata = validateToolDownloadInputs(channel.source, channel.metadata)
  const file = metadata.files.find(entry => entry.path === 'tools/manifest.json')
  if (!file || Buffer.byteLength(channel.catalog) !== file.bytes || createHash('sha256').update(channel.catalog).digest('hex') !== file.sha256) throw new Error('工具更新目录摘要不匹配')
  parseNativeToolCatalog(channel.catalog)
  return { ...channel, metadata }
}
