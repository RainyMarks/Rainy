// Copy the libraries and fonts interactive answers may use into resources/widget, from the pinned dev dependencies.
// Run after changing their versions: node scripts/prepare-widget-libs.mjs
import { build } from 'esbuild'
import { copyFile, mkdir, readFile, rm, writeFile } from 'node:fs/promises'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

const root = join(dirname(fileURLToPath(import.meta.url)), '..')
const modules = join(root, 'node_modules')
const out = join(root, 'resources', 'widget')

await rm(join(out, 'lib'), { recursive: true, force: true })
await rm(join(out, 'fonts'), { recursive: true, force: true })
await rm(join(out, 'licenses'), { recursive: true, force: true })
for (const folder of ['lib', 'fonts', 'licenses']) await mkdir(join(out, folder), { recursive: true })

const copies = [
  ['chart.js/dist/chart.umd.min.js', 'lib/chart.umd.min.js'],
  ['d3/dist/d3.min.js', 'lib/d3.min.js'],
  ['idiomorph/dist/idiomorph.min.js', 'lib/idiomorph.min.js'],
  ['@fontsource-variable/hanken-grotesk/files/hanken-grotesk-latin-wght-normal.woff2', 'fonts/hanken-grotesk-latin.woff2'],
  ['@fontsource-variable/hanken-grotesk/files/hanken-grotesk-latin-ext-wght-normal.woff2', 'fonts/hanken-grotesk-latin-ext.woff2'],
  ['@fontsource-variable/geist-mono/files/geist-mono-latin-wght-normal.woff2', 'fonts/geist-mono-latin.woff2'],
  ['chart.js/LICENSE.md', 'licenses/chart.js.txt'],
  ['d3/LICENSE', 'licenses/d3.txt'],
  ['three/LICENSE', 'licenses/three.txt'],
  ['idiomorph/LICENSE', 'licenses/idiomorph.txt'],
  ['@fontsource-variable/hanken-grotesk/LICENSE', 'licenses/hanken-grotesk.txt'],
  ['@fontsource-variable/geist-mono/LICENSE', 'licenses/geist-mono.txt'],
]
for (const [from, to] of copies) await copyFile(join(modules, ...from.split('/')), join(out, ...to.split('/')))

// three.module.js imports three.core.js; one minified module is what the import map points `three` at.
await build({ entryPoints: [join(modules, 'three/build/three.module.js')], bundle: true, format: 'esm', minify: true, legalComments: 'inline',
  outfile: join(out, 'lib/three.module.min.js'), logLevel: 'warning' })
await build({ entryPoints: [join(modules, 'three/examples/jsm/controls/OrbitControls.js')], bundle: true, format: 'esm', minify: true,
  external: ['three'], outfile: join(out, 'lib/three-addons/controls/OrbitControls.js'), logLevel: 'warning' })

const versions = {}
for (const name of ['chart.js', 'd3', 'three', 'idiomorph', '@fontsource-variable/hanken-grotesk', '@fontsource-variable/geist-mono']) {
  versions[name] = JSON.parse(await readFile(join(modules, ...name.split('/'), 'package.json'), 'utf8')).version
}
await writeFile(join(out, 'versions.json'), `${JSON.stringify(versions, null, 2)}\n`)
console.log('resources/widget ready', versions)
