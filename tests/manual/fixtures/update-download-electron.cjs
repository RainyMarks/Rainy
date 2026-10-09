/** Exercise the public Windows updater API in a private Electron carrier without installing an update. */
const assert = require('node:assert/strict')
const { writeFile } = require('node:fs/promises')
const { relative, isAbsolute, join } = require('node:path')
const { app } = require('electron')

const config = require(join(app.getAppPath(), 'fixture.json'))
app.disableHardwareAcceleration()
app.setPath('appData', join(config.root, 'app-data'))
app.setPath('userData', join(config.root, 'user-data'))
app.setPath('sessionData', join(config.root, 'session-data'))
app.setPath('logs', join(config.root, 'logs'))

const report = { scenario: config.scenario, currentVersion: app.getVersion(), events: [], errors: [], logs: [] }
const logger = {}
for (const level of ['info', 'warn', 'error', 'debug']) {
  logger[level] = (...values) => {
    report.logs.push({ level, message: values.map(String).join(' ') })
    if (report.logs.length > 100) report.logs.shift()
  }
}

async function run() {
  await app.whenReady()
  assert.equal(app.getVersion(), config.currentVersion)
  const { NsisUpdater } = require(config.updaterEntry)
  const updater = new NsisUpdater()
  updater.autoInstallOnAppQuit = false
  updater.autoRunAppAfterInstall = false
  updater.autoDownload = true
  updater.allowPrerelease = false
  updater.allowDowngrade = false
  updater.disableWebInstaller = true
  updater.disableDifferentialDownload = true
  updater.forceDevUpdateConfig = true
  updater.logger = logger
  updater.setFeedURL({ provider: 'generic', url: config.feedUrl })
  await updater.netSession.setProxy({ mode: 'direct' })
  for (const event of ['checking-for-update', 'update-available', 'update-not-available', 'update-downloaded']) {
    updater.on(event, () => report.events.push(event))
  }
  updater.on('error', error => report.errors.push({ code: error.code, message: error.message }))
  const check = await updater.checkForUpdates()
  assert(check, 'The development carrier must perform a real update check')
  report.isUpdateAvailable = check.isUpdateAvailable
  if (config.scenario === 'current') {
    assert.equal(check.isUpdateAvailable, false)
    assert.equal(check.downloadPromise == null, true)
    assert(report.events.includes('update-not-available'))
  } else {
    assert.equal(check.isUpdateAvailable, true)
    assert(check.downloadPromise, 'The available update must start downloading automatically')
    if (config.scenario === 'corrupt') {
      let failure
      try { await check.downloadPromise } catch (error) { failure = error }
      assert(failure, 'An invalid SHA-512 must reject the download')
      assert.match(failure.message, /sha512 checksum mismatch/i)
      assert(!report.events.includes('update-downloaded'))
      report.rejectedChecksum = true
    } else {
      const downloads = await check.downloadPromise
      assert.equal(downloads.length, 1)
      const downloaded = relative(config.root, downloads[0])
      assert(downloaded && downloaded !== '..' && !downloaded.startsWith(`..${require('node:path').sep}`) && !isAbsolute(downloaded))
      assert(report.events.includes('update-downloaded'))
      report.downloaded = downloaded
    }
  }
  assert.equal(updater.autoInstallOnAppQuit, false)
  report.passed = true
}

run().catch(error => {
  report.passed = false
  report.error = error.stack ?? String(error)
}).finally(async () => {
  await writeFile(config.reportPath, JSON.stringify(report, null, 2) + '\n')
  app.exit(report.passed ? 0 : 1)
})
