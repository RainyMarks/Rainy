/** Observe the real manager's existing signing command without adding a renderer or a dialog. */
import assert from 'node:assert/strict'
import { writeFileSync } from 'node:fs'
import { pathToFileURL } from 'node:url'
import { app } from 'electron'

const [managerMain, reportPath] = process.argv.slice(2)
let windows = 0
let webContents = 0
app.disableHardwareAcceleration()
app.on('browser-window-created', () => { windows++ })
app.on('web-contents-created', () => { webContents++ })
app.once('will-quit', () => {
  writeFileSync(reportPath, JSON.stringify({ windows, webContents,
    issuerOverride: app.commandLine.hasSwitch('issuer-data-dir'), signingCommand: app.commandLine.hasSwitch('sign-release-manifest') }) + '\n')
})
try {
  assert(managerMain && reportPath, 'Pass the built manager entry and observer report')
  assert(app.commandLine.hasSwitch('issuer-data-dir') && app.commandLine.hasSwitch('sign-release-manifest'), 'Only isolated signing is permitted')
  await import(pathToFileURL(managerMain).href)
} catch (error) {
  console.error(JSON.stringify({ passed: false, errorType: error instanceof Error ? error.name : typeof error }))
  app.exit(1)
}
