/** File types from their leading bytes, in the spirit of `file(1)`. */

/** A recognized format. */
export interface FileSignature {
  /** Technical name such as `PNG image` or `ELF 64-bit LSB executable, x86-64`. */
  readonly label: string
  readonly family: 'image' | 'archive' | 'executable' | 'document' | 'audio' | 'video' | 'capture' | 'database' | 'text' | 'other'
}

const u16le = (bytes: Uint8Array, offset: number): number => (bytes[offset] ?? 0) | ((bytes[offset + 1] ?? 0) << 8)
const u16be = (bytes: Uint8Array, offset: number): number => ((bytes[offset] ?? 0) << 8) | (bytes[offset + 1] ?? 0)
const u32le = (bytes: Uint8Array, offset: number): number => (u16le(bytes, offset) + u16le(bytes, offset + 2) * 0x10000)
const u32be = (bytes: Uint8Array, offset: number): number => (u16be(bytes, offset) * 0x10000 + u16be(bytes, offset + 2))

function starts(bytes: Uint8Array, offset: number, ...values: readonly number[]): boolean {
  return values.every((value, index) => bytes[offset + index] === value)
}

function ascii(bytes: Uint8Array, offset: number, text: string): boolean {
  for (let index = 0; index < text.length; index++) if (bytes[offset + index] !== text.charCodeAt(index)) return false
  return true
}

function latin1(bytes: Uint8Array, offset: number, length: number): string {
  return String.fromCharCode(...bytes.subarray(offset, Math.min(bytes.length, offset + length)))
}

const ELF_MACHINES: Readonly<Record<number, string>> = {
  0x02: 'SPARC', 0x03: 'Intel 80386', 0x08: 'MIPS', 0x14: 'PowerPC', 0x15: 'PowerPC64', 0x16: 'IBM S/390', 0x28: 'ARM',
  0x2b: 'SPARC V9', 0x32: 'IA-64', 0x3e: 'x86-64', 0xb7: 'ARM aarch64', 0xf3: 'RISC-V', 0x102: 'LoongArch',
}
const PE_MACHINES: Readonly<Record<number, string>> = {
  0x14c: 'Intel 80386', 0x8664: 'x86-64', 0xaa64: 'ARM64', 0x1c0: 'ARM', 0x1c4: 'ARMv7 Thumb', 0x200: 'Intel Itanium',
}

function elf(head: Uint8Array): FileSignature {
  const bits = head[4] === 2 ? '64-bit' : head[4] === 1 ? '32-bit' : 'invalid class'
  const little = head[5] !== 2
  const read16 = little ? u16le : u16be
  const kinds: Readonly<Record<number, string>> = { 1: 'relocatable', 2: 'executable', 3: 'shared object', 4: 'core file' }
  const kind = kinds[read16(head, 16)] ?? 'file'
  const machine = ELF_MACHINES[read16(head, 18)]
  return { label: `ELF ${bits} ${little ? 'LSB' : 'MSB'} ${kind}${machine === undefined ? '' : `, ${machine}`}`, family: 'executable' }
}

function pe(head: Uint8Array): FileSignature {
  const header = u32le(head, 0x3c)
  if (header < 0x40 || header + 26 > head.length || !ascii(head, header, 'PE\0\0')) return { label: 'MS-DOS executable', family: 'executable' }
  const machine = PE_MACHINES[u16le(head, header + 4)]
  const library = (u16le(head, header + 22) & 0x2000) !== 0
  const magic = u16le(head, header + 24)
  const format = magic === 0x20b ? 'PE32+' : 'PE32'
  // Data directory 14 describes the CLR header of a .NET assembly.
  const directories = header + 24 + (magic === 0x20b ? 112 : 96)
  const clr = directories + 14 * 8 + 8 <= head.length && u32le(head, directories + 14 * 8) !== 0
  return {
    label: `${format} ${library ? 'DLL' : 'executable'}${machine === undefined ? '' : `, ${machine}`}${clr ? ', .NET assembly' : ''}`,
    family: 'executable',
  }
}

function zip(head: Uint8Array): FileSignature {
  const nameLength = u16le(head, 26)
  const name = latin1(head, 30, nameLength)
  if (name === 'mimetype' && ascii(head, 30 + nameLength + u16le(head, 28), 'application/epub+zip')) return { label: 'EPUB document', family: 'document' }
  if (name.startsWith('[Content_Types].xml') || name.startsWith('_rels/') || name.startsWith('docProps/')) {
    return { label: 'Office Open XML document (docx, xlsx, pptx)', family: 'document' }
  }
  if (name === 'AndroidManifest.xml' || name === 'classes.dex' || name === 'resources.arsc') return { label: 'Android package (APK)', family: 'archive' }
  if (name.startsWith('META-INF/')) return { label: 'Java archive (JAR)', family: 'archive' }
  return { label: 'Zip archive', family: 'archive' }
}

function isoMedia(head: Uint8Array): FileSignature {
  const brand = latin1(head, 8, 4)
  if (brand === 'avif' || brand === 'avis') return { label: 'AVIF image', family: 'image' }
  if (['heic', 'heix', 'mif1', 'msf1'].includes(brand)) return { label: 'HEIF image', family: 'image' }
  if (brand === 'qt  ') return { label: 'QuickTime video', family: 'video' }
  if (brand === 'M4A ' || brand === 'M4B ') return { label: 'MPEG-4 audio', family: 'audio' }
  return { label: `MPEG-4 media (${brand.trim() || 'unknown brand'})`, family: 'video' }
}

/**
 * Recognize a file from its first bytes; 4 KiB is enough for every format here.
 * @param head Leading bytes.
 * @returns The format, or `undefined` when nothing matches.
 */
export function detectSignature(head: Uint8Array): FileSignature | undefined {
  if (starts(head, 0, 0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a)) return { label: 'PNG image', family: 'image' }
  if (starts(head, 0, 0xff, 0xd8, 0xff)) return { label: 'JPEG image', family: 'image' }
  if (ascii(head, 0, 'GIF87a') || ascii(head, 0, 'GIF89a')) return { label: `GIF image (${latin1(head, 3, 3)})`, family: 'image' }
  if (ascii(head, 0, 'RIFF') && ascii(head, 8, 'WEBP')) return { label: 'WebP image', family: 'image' }
  if (ascii(head, 0, 'RIFF') && ascii(head, 8, 'WAVE')) return { label: 'WAVE audio', family: 'audio' }
  if (ascii(head, 0, 'RIFF') && ascii(head, 8, 'AVI ')) return { label: 'AVI video', family: 'video' }
  if (ascii(head, 0, 'BM') && [12, 40, 52, 56, 64, 108, 124].includes(u32le(head, 14))) return { label: 'BMP image', family: 'image' }
  if (starts(head, 0, 0, 0, 1, 0) && u16le(head, 4) > 0 && u16le(head, 4) < 256 && head[9] === 0) return { label: 'ICO icon', family: 'image' }
  if (starts(head, 0, 0x49, 0x49, 0x2a, 0) || starts(head, 0, 0x4d, 0x4d, 0, 0x2a)) return { label: 'TIFF image', family: 'image' }
  if (ascii(head, 0, '8BPS')) return { label: 'Photoshop image', family: 'image' }
  if (ascii(head, 4, 'ftyp')) return isoMedia(head)
  if (ascii(head, 0, '%PDF-')) return { label: `PDF document, version ${latin1(head, 5, 3)}`, family: 'document' }
  if (starts(head, 0, 0x50, 0x4b, 0x03, 0x04)) return zip(head)
  if (starts(head, 0, 0x50, 0x4b, 0x05, 0x06) || starts(head, 0, 0x50, 0x4b, 0x07, 0x08)) return { label: 'Zip archive', family: 'archive' }
  if (ascii(head, 0, 'Rar!\x1a\x07')) return { label: head[6] === 1 ? 'RAR archive v5' : 'RAR archive', family: 'archive' }
  if (starts(head, 0, 0x37, 0x7a, 0xbc, 0xaf, 0x27, 0x1c)) return { label: '7-Zip archive', family: 'archive' }
  if (starts(head, 0, 0x1f, 0x8b)) return { label: 'gzip compressed data', family: 'archive' }
  if (ascii(head, 0, 'BZh') && (head[3] ?? 0) >= 0x31 && (head[3] ?? 0) <= 0x39) return { label: 'bzip2 compressed data', family: 'archive' }
  if (starts(head, 0, 0xfd, 0x37, 0x7a, 0x58, 0x5a, 0)) return { label: 'XZ compressed data', family: 'archive' }
  if (starts(head, 0, 0x28, 0xb5, 0x2f, 0xfd)) return { label: 'Zstandard compressed data', family: 'archive' }
  if (starts(head, 0, 0x04, 0x22, 0x4d, 0x18)) return { label: 'LZ4 compressed data', family: 'archive' }
  if (ascii(head, 257, 'ustar')) return { label: 'POSIX tar archive', family: 'archive' }
  if (ascii(head, 0, 'MSCF')) return { label: 'Microsoft Cabinet archive', family: 'archive' }
  if (starts(head, 0, 0xd0, 0xcf, 0x11, 0xe0, 0xa1, 0xb1, 0x1a, 0xe1)) return { label: 'Compound document (legacy Office, MSI)', family: 'document' }
  if (starts(head, 0, 0x7f, 0x45, 0x4c, 0x46)) return elf(head)
  if (ascii(head, 0, 'MZ')) return pe(head)
  const machO = u32be(head, 0)
  if ([0xfeedface, 0xfeedfacf, 0xcefaedfe, 0xcffaedfe].includes(machO)) {
    return { label: `Mach-O ${machO === 0xfeedfacf || machO === 0xcffaedfe ? '64-bit' : '32-bit'} binary`, family: 'executable' }
  }
  // Java class files and Mach-O universal binaries share CAFEBABE; class files store a major version of 45 or more there.
  if (machO === 0xcafebabe) return u16be(head, 6) >= 45 ? { label: 'Java class file', family: 'executable' } : { label: 'Mach-O universal binary', family: 'executable' }
  if (ascii(head, 0, 'dex\n')) return { label: 'Android Dalvik executable (DEX)', family: 'executable' }
  if (starts(head, 0, 0, 0x61, 0x73, 0x6d)) return { label: 'WebAssembly module', family: 'executable' }
  if (ascii(head, 0, 'SQLite format 3\0')) return { label: 'SQLite 3 database', family: 'database' }
  if ([0xd4c3b2a1, 0xa1b2c3d4, 0x4d3cb2a1, 0xa1b23c4d].includes(machO)) return { label: 'pcap capture file', family: 'capture' }
  if (starts(head, 0, 0x0a, 0x0d, 0x0d, 0x0a)) return { label: 'pcapng capture file', family: 'capture' }
  if (ascii(head, 0, 'ID3') || (head[0] === 0xff && ((head[1] ?? 0) & 0xe6) === 0xe2)) return { label: 'MP3 audio', family: 'audio' }
  if (ascii(head, 0, 'fLaC')) return { label: 'FLAC audio', family: 'audio' }
  if (ascii(head, 0, 'OggS')) return { label: 'Ogg media', family: 'audio' }
  if (starts(head, 0, 0x1a, 0x45, 0xdf, 0xa3)) return { label: 'Matroska or WebM video', family: 'video' }
  if (ascii(head, 0, '-----BEGIN ')) {
    const end = latin1(head, 11, 64).indexOf('-----')
    return { label: `PEM ${end > 0 ? latin1(head, 11, end) : 'data'}`, family: 'text' }
  }
  if (ascii(head, 0, '#!')) {
    const line = latin1(head, 2, 120).split(/[\r\n]/u)[0]?.trim() ?? ''
    return { label: `Script (${line === '' ? 'shebang' : line})`, family: 'text' }
  }
  if (starts(head, 0, 0xef, 0xbb, 0xbf)) return { label: 'UTF-8 text with BOM', family: 'text' }
  if (starts(head, 0, 0xff, 0xfe)) return { label: 'UTF-16 little-endian text', family: 'text' }
  if (starts(head, 0, 0xfe, 0xff)) return { label: 'UTF-16 big-endian text', family: 'text' }
  return undefined
}
