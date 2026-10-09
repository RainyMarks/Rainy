/** Stage `build/shell`, the Electron app directory packaged into app.asar, and generate `build/icon.ico`. */
import { copyFileSync, cpSync, existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { pngToIco } from './icon.mjs'

const root = resolve(import.meta.dirname, '..')
const manifest = JSON.parse(readFileSync(resolve(root, 'package.json'), 'utf8'))
const target = resolve(root, 'build/shell')
for (const name of ['dist/main.cjs', 'dist/preload.cjs', 'dist/setup/index.html']) {
  if (!existsSync(resolve(root, name))) throw new Error(`Build the application first (pnpm run build:release): ${name} is missing`)
}
rmSync(target, { recursive: true, force: true })
mkdirSync(target, { recursive: true })
copyFileSync(resolve(root, 'dist/main.cjs'), resolve(target, 'main.cjs'))
copyFileSync(resolve(root, 'dist/preload.cjs'), resolve(target, 'preload.cjs'))
cpSync(resolve(root, 'dist/setup'), resolve(target, 'setup'), { recursive: true })
copyFileSync(resolve(root, 'LICENSE'), resolve(target, 'LICENSE'))
copyFileSync(resolve(root, 'THIRD_PARTY_NOTICES.md'), resolve(target, 'THIRD_PARTY_NOTICES.md'))
writeFileSync(resolve(target, 'package.json'), JSON.stringify({ name: manifest.name, productName: manifest.productName, version: manifest.version,
  main: 'main.cjs', description: manifest.description, author: 'NCUCyberBase', license: manifest.license }, null, 2) + '\n')
writeFileSync(resolve(root, 'build/icon.ico'), pngToIco(readFileSync(resolve(root, 'resources/icon.png'))))
console.log(`Staged ${target} for RainyAgent ${manifest.version}`)
