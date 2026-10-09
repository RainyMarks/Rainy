/** Sign Rainy-owned resource bytes after copying and before producing the installer. */
module.exports = async (context) => {
  if (context.electronPlatformName !== 'win32') throw new Error('RainyAgent release currently targets Windows x64')
  const { join } = require('node:path')
  const { pathToFileURL } = require('node:url')
  const { cp, lstat } = require('node:fs/promises')
  const resources = join(context.appOutDir, 'resources')
  const destination = join(resources, 'windows-host')
  try {
    await lstat(destination)
    throw new Error('The packaged Windows Host destination must be empty before copying the verified runtime')
  } catch (error) {
    if (error.code !== 'ENOENT') throw error
  }
  // Electron Builder's dependency pruning also affects extraResources containing node_modules.
  await cp(join(__dirname, '../runtime/windows-host'), destination, { recursive: true, force: false, errorOnExist: true })
  const { signReleaseResources } = await import(pathToFileURL(join(__dirname, 'sign-release.mjs')).href)
  const result = await signReleaseResources(resources)
  console.log(`Signed ${result.files} RainyAgent release resources`)
}
