/** Exercise desktop clipboard permissions against the real operating-system clipboard. */
import { app, BrowserWindow, clipboard } from 'electron'
import { createServer } from 'node:http'
import assert from 'node:assert/strict'
import { allowsClipboardWrite } from '../../../src/main/clipboard-policy.ts'

async function main(): Promise<void> {
  await app.whenReady()
  const previous = await clipboard.readText()
  const server = createServer((_request, response) => response.end('<button id="copy">Copy</button><script>document.querySelector("button").onclick=async()=>{try{await navigator.clipboard.writeText("rainy-conversation-copy");document.body.dataset.result="copied"}catch(e){document.body.dataset.result=String(e)}}</script>'))
  await new Promise<void>(resolve => server.listen(0, '127.0.0.1', resolve))
  const address = server.address()
  assert.ok(address && typeof address !== 'string')
  const origin = `http://127.0.0.1:${address.port}`
  const window = new BrowserWindow({ show: false, webPreferences: { contextIsolation: true, sandbox: true } })
  window.webContents.session.setPermissionRequestHandler((_contents, permission, callback, details) => {
    callback(allowsClipboardWrite(permission, details.requestingUrl, origin))
  })
  try {
    await window.loadURL(`${origin}/sessions/clipboard`)
    window.show()
    window.focus()
    await window.webContents.executeJavaScript('document.querySelector("button").click()', true)
    for (let attempt = 0; attempt < 100; attempt++) {
      if (await window.webContents.executeJavaScript('document.body.dataset.result')) break
      await new Promise(resolve => setTimeout(resolve, 20))
    }
    assert.equal(await window.webContents.executeJavaScript('document.body.dataset.result'), 'copied')
    assert.equal(await clipboard.readText(), 'rainy-conversation-copy')
    console.log('Real Electron conversation clipboard write passed')
  } finally {
    if (typeof previous === 'string') await clipboard.writeText(previous)
    window.destroy()
    await new Promise<void>((resolve, reject) => server.close((error) => { if (error) reject(error); else resolve() }))
    app.quit()
  }
}
void main().catch((error: unknown) => { console.error(error); app.exit(1) })
