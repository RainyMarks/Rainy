/** Convert the PNG application icon into a multi-size Windows ICO without native image libraries. */
import { crc32, deflateSync, inflateSync } from 'node:zlib'

/** Icon sizes the Windows shell picks from. */
export const ICO_SIZES = [16, 24, 32, 48, 64, 128, 256]
const SIGNATURE = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a])

/**
 * Decode a non-interlaced 8-bit PNG.
 * @param {Buffer} bytes PNG file.
 * @returns {{width: number, height: number, pixels: Uint8Array}} Straight-alpha RGBA pixels, row-major.
 */
export function decodePng(bytes) {
  if (!bytes.subarray(0, 8).equals(SIGNATURE)) throw new Error('Not a PNG file')
  let header
  let palette
  let transparency
  const data = []
  for (let offset = 8; offset < bytes.length;) {
    const length = bytes.readUInt32BE(offset)
    const type = bytes.toString('latin1', offset + 4, offset + 8)
    const body = bytes.subarray(offset + 8, offset + 8 + length)
    if (crc32(bytes.subarray(offset + 4, offset + 8 + length)) !== bytes.readUInt32BE(offset + 8 + length)) throw new Error(`PNG ${type} chunk checksum differs`)
    if (type === 'IHDR') header = { width: body.readUInt32BE(0), height: body.readUInt32BE(4), depth: body[8], color: body[9], interlace: body[12] }
    else if (type === 'PLTE') palette = body
    else if (type === 'tRNS') transparency = body
    else if (type === 'IDAT') data.push(body)
    else if (type === 'IEND') break
    offset += 12 + length
  }
  if (!header || header.depth !== 8 || header.interlace !== 0) throw new Error('Only non-interlaced 8-bit PNG icons are supported')
  const channels = { 0: 1, 2: 3, 3: 1, 4: 2, 6: 4 }[header.color]
  if (channels === undefined || (header.color === 3 && !palette)) throw new Error(`Unsupported PNG color type ${header.color}`)
  const { width, height } = header
  const stride = width * channels
  const raw = inflateSync(Buffer.concat(data))
  if (raw.length !== height * (stride + 1)) throw new Error('PNG image data has an unexpected length')
  const rows = new Uint8Array(height * stride)
  for (let y = 0; y < height; y++) {
    const filter = raw[y * (stride + 1)]
    const source = y * (stride + 1) + 1
    const target = y * stride
    for (let x = 0; x < stride; x++) {
      const left = x >= channels ? rows[target + x - channels] : 0
      const up = y > 0 ? rows[target - stride + x] : 0
      const corner = y > 0 && x >= channels ? rows[target - stride + x - channels] : 0
      let predictor
      if (filter === 0) predictor = 0
      else if (filter === 1) predictor = left
      else if (filter === 2) predictor = up
      else if (filter === 3) predictor = (left + up) >> 1
      else if (filter === 4) {
        const estimate = left + up - corner
        const [a, b, c] = [Math.abs(estimate - left), Math.abs(estimate - up), Math.abs(estimate - corner)]
        predictor = a <= b && a <= c ? left : b <= c ? up : corner
      } else throw new Error(`Unknown PNG row filter ${filter}`)
      rows[target + x] = (raw[source + x] + predictor) & 0xff
    }
  }
  const pixels = new Uint8Array(width * height * 4)
  for (let index = 0; index < width * height; index++) {
    const value = rows.subarray(index * channels, index * channels + channels)
    let rgba
    if (header.color === 0) rgba = [value[0], value[0], value[0], 255]
    else if (header.color === 2) rgba = [value[0], value[1], value[2], 255]
    else if (header.color === 3) rgba = [palette[value[0] * 3], palette[value[0] * 3 + 1], palette[value[0] * 3 + 2], transparency?.[value[0]] ?? 255]
    else if (header.color === 4) rgba = [value[0], value[0], value[0], value[1]]
    else rgba = [value[0], value[1], value[2], value[3]]
    pixels.set(rgba, index * 4)
  }
  return { width, height, pixels }
}

/**
 * Scale an image into a transparent square by area averaging, keeping its aspect ratio and centering it.
 * @param {{width: number, height: number, pixels: Uint8Array}} image Straight-alpha RGBA source.
 * @param {number} size Output edge length.
 * @returns {Uint8Array} Straight-alpha RGBA pixels of a `size`×`size` image.
 */
export function containSquare(image, size) {
  const scale = Math.min(size / image.width, size / image.height)
  const width = Math.max(1, Math.round(image.width * scale))
  const height = Math.max(1, Math.round(image.height * scale))
  const left = Math.floor((size - width) / 2)
  const top = Math.floor((size - height) / 2)
  const output = new Uint8Array(size * size * 4)
  const spanX = image.width / width
  const spanY = image.height / height
  for (let y = 0; y < height; y++) {
    const y0 = y * spanY
    const y1 = y0 + spanY
    for (let x = 0; x < width; x++) {
      const x0 = x * spanX
      const x1 = x0 + spanX
      // Alpha-weighted sums keep transparent edges from darkening the result.
      let red = 0, green = 0, blue = 0, alpha = 0, area = 0
      for (let sy = Math.floor(y0); sy < Math.ceil(y1); sy++) {
        const coverY = Math.min(y1, sy + 1) - Math.max(y0, sy)
        for (let sx = Math.floor(x0); sx < Math.ceil(x1); sx++) {
          const weight = coverY * (Math.min(x1, sx + 1) - Math.max(x0, sx))
          const at = (sy * image.width + sx) * 4
          const opacity = image.pixels[at + 3] * weight
          red += image.pixels[at] * opacity
          green += image.pixels[at + 1] * opacity
          blue += image.pixels[at + 2] * opacity
          alpha += opacity
          area += weight
        }
      }
      const at = ((top + y) * size + left + x) * 4
      if (alpha > 0) output.set([Math.round(red / alpha), Math.round(green / alpha), Math.round(blue / alpha), Math.round(alpha / area)], at)
    }
  }
  return output
}

/**
 * Encode straight-alpha RGBA pixels as a PNG.
 * @param {number} width Image width.
 * @param {number} height Image height.
 * @param {Uint8Array} pixels Row-major RGBA bytes.
 * @returns {Buffer} PNG file.
 */
export function encodePng(width, height, pixels) {
  const chunk = (type, body) => {
    const head = Buffer.alloc(8)
    head.writeUInt32BE(body.length, 0)
    head.write(type, 4, 'latin1')
    const tail = Buffer.alloc(4)
    tail.writeUInt32BE(crc32(Buffer.concat([head.subarray(4), body])), 0)
    return Buffer.concat([head, body, tail])
  }
  const header = Buffer.alloc(13)
  header.writeUInt32BE(width, 0)
  header.writeUInt32BE(height, 4)
  header.set([8, 6, 0, 0, 0], 8)
  const raw = Buffer.alloc(height * (width * 4 + 1))
  for (let y = 0; y < height; y++) raw.set(pixels.subarray(y * width * 4, (y + 1) * width * 4), y * (width * 4 + 1) + 1)
  return Buffer.concat([SIGNATURE, chunk('IHDR', header), chunk('IDAT', deflateSync(raw, { level: 9 })), chunk('IEND', Buffer.alloc(0))])
}

/**
 * Build a Windows icon whose entries are PNG images.
 * @param {Buffer} png Source PNG, ideally square and at least 256 pixels wide.
 * @param {number[]} [sizes] Entry sizes, each at most 256.
 * @returns {Buffer} ICO file.
 */
export function pngToIco(png, sizes = ICO_SIZES) {
  const image = decodePng(png)
  const images = sizes.map(size => encodePng(size, size, containSquare(image, size)))
  const header = Buffer.alloc(6 + 16 * sizes.length)
  header.writeUInt16LE(1, 2)
  header.writeUInt16LE(sizes.length, 4)
  let offset = header.length
  images.forEach((entry, index) => {
    const at = 6 + index * 16
    header[at] = header[at + 1] = sizes[index] === 256 ? 0 : sizes[index]
    header.writeUInt16LE(1, at + 4)
    header.writeUInt16LE(32, at + 6)
    header.writeUInt32LE(entry.length, at + 8)
    header.writeUInt32LE(offset, at + 12)
    offset += entry.length
  })
  return Buffer.concat([header, ...images])
}
