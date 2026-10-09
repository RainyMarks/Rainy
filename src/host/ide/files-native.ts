/** No-replace OS rename for files, directories and links; unsupported filesystems fail without a copy fallback. */
import { toNamespacedPath } from 'node:path'
import { getSystemErrorName } from 'node:util'

type NativeRename = (source: string, destination: string) => void
type RenameAt2 = (oldDirectory: number, source: string, newDirectory: number, destination: string, flags: number) => number
type RenameExclusive = (source: string, destination: string, flags: number) => number
type LastError = () => number

// Linux UAPI fs.h and Darwin sys/stdio.h assign these flags; they are OS constants.
const RENAME_NOREPLACE = 1
const RENAME_EXCL = 4
const AT_FDCWD = -100
const MOVEFILE_WRITE_THROUGH = 8
let operation: Promise<NativeRename> | undefined

function renameError(code: string, source: string, destination: string): NodeJS.ErrnoException {
  return Object.assign(new Error(`Cannot rename "${source}" to "${destination}": ${code}.`), { code, path: source, dest: destination })
}

function win32Code(error: number): string {
  switch (error) {
    case 2:
    case 3: return 'ENOENT'
    case 5: return 'EACCES'
    case 17: return 'EXDEV'
    case 32: return 'EBUSY'
    case 80:
    case 183: return 'EEXIST'
    case 87: return 'EINVAL'
    case 145: return 'ENOTEMPTY'
    default: return 'EIO'
  }
}

async function loadRename(): Promise<NativeRename> {
  const koffi = (await import('koffi')).default
  if (process.platform === 'win32') {
    const kernel = koffi.load('kernel32.dll')
    const move = kernel.func('int __stdcall MoveFileExW(const char16_t *source, const char16_t *destination, uint32_t flags)') as RenameExclusive
    const lastError = kernel.func('uint32_t __stdcall GetLastError()') as LastError
    return (source, destination) => {
      if (move(toNamespacedPath(source), toNamespacedPath(destination), MOVEFILE_WRITE_THROUGH) === 0) {
        throw renameError(win32Code(lastError()), source, destination)
      }
    }
  }
  if (process.platform === 'linux') {
    const libc = koffi.load(null)
    const move = libc.func('int renameat2(int olddir, const char *source, int newdir, const char *destination, unsigned int flags)') as RenameAt2
    return (source, destination) => {
      if (move(AT_FDCWD, source, AT_FDCWD, destination, RENAME_NOREPLACE) !== 0) {
        throw renameError(getSystemErrorName(-koffi.errno()), source, destination)
      }
    }
  }
  if (process.platform === 'darwin') {
    const libc = koffi.load(null)
    const move = libc.func('int renamex_np(const char *source, const char *destination, unsigned int flags)') as RenameExclusive
    return (source, destination) => {
      if (move(source, destination, RENAME_EXCL) !== 0) throw renameError(getSystemErrorName(-koffi.errno()), source, destination)
    }
  }
  throw new Error(`Exclusive IDE rename is unavailable on ${process.platform}.`)
}

/**
 * Rename on the same filesystem, refusing an existing destination at the OS commit.
 * @param source - validated absolute source path; a final link itself is moved.
 * @param destination - validated absolute destination whose parent already exists.
 * @returns resolution after the OS rename, or rejection without overwriting a competitor.
 */
export async function renameIdePathNoReplace(source: string, destination: string): Promise<void> {
  operation ??= loadRename()
  const move = await operation
  move(source, destination)
}
