// Export the interface strings of src/renderer/**/messages.ts to native/crates/app/assets/messages.json, so the GPUI
// interface shows the same Chinese and English text. Run from the repository root:
//   npx tsx native/scripts/export-messages.mts
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'
import { pathToFileURL } from 'node:url'

const files: Record<string, string> = {
  app: 'src/renderer/app/messages.ts', chat: 'src/renderer/chat/messages.ts', git: 'src/renderer/git/messages.ts',
  ide: 'src/renderer/ide/messages.ts', map: 'src/renderer/map/messages.ts', review: 'src/renderer/review/messages.ts',
  search: 'src/renderer/search/messages.ts', settings: 'src/renderer/settings/messages.ts', viewers: 'src/renderer/ide/viewers/messages.ts',
}
const stub = `const defineMessages = (zh: any, en: any) => { (globalThis as any).__captured = { zh, en }; return { useT: () => undefined, t: () => undefined } as any }\n`
const scratch = mkdtempSync(join(tmpdir(), 'rainy-messages-'))
const out: Record<string, { zh: Record<string, string>; en: Record<string, string> }> = {}
try {
  for (const [name, file] of Object.entries(files)) {
    const text = readFileSync(resolve(file), 'utf8').replace(/^import \{ defineMessages \} from .*$/mu, stub)
    const copy = join(scratch, `${name}.ts`)
    writeFileSync(copy, text)
    await import(pathToFileURL(copy).href)
    const captured = (globalThis as any).__captured
    const zhKeys = Object.keys(captured.zh).sort(), enKeys = Object.keys(captured.en).sort()
    if (JSON.stringify(zhKeys) !== JSON.stringify(enKeys)) throw new Error(`${file}: Chinese and English keys differ`)
    out[name] = captured
  }
} finally { rmSync(scratch, { recursive: true, force: true }) }
writeFileSync(resolve('native/crates/app/assets/messages.json'), JSON.stringify(out, null, 1) + '\n')
console.log(Object.entries(out).map(([name, { zh }]) => `${name}: ${Object.keys(zh).length}`).join(', '))
