/** Installed Windows/Electron acceptance. Run manually with --installed after installation is verified. */
import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { randomUUID } from 'node:crypto';
import { access, mkdir, readFile, unlink, writeFile } from 'node:fs/promises';
import { createRequire } from 'node:module';
import { join, resolve } from 'node:path';
import { promisify } from 'node:util';

const requireWeb = createRequire(new URL('../../web/package.json', import.meta.url));
const { _electron } = requireWeb('playwright');
const run = promisify(execFile);
const executablePath = 'C:/Users/ROG/AppData/Local/Programs/RainyAgent/RainyAgent.exe';
const statePattern = state => `**/rainy/icesky/state?scope=${encodeURIComponent(state.scope)}`;
const startupTimeout = 240000;
const lifecycleTimeout = 90000;
const deferred = () => Promise.withResolvers();

async function bounded(promise, message, milliseconds = 30000) {
    let timer;
    try { return await Promise.race([promise, new Promise((_, reject) => { timer = setTimeout(() => reject(new Error(message)), milliseconds); })]); }
    finally { clearTimeout(timer); }
}

function option(name) { const index = process.argv.indexOf(name); return index < 0 ? undefined : process.argv[index + 1]; }

async function rainyProcesses() {
    const script = '$items = @(Get-CimInstance Win32_Process -Filter "Name = \'RainyAgent.exe\'" | Select-Object ProcessId, ExecutablePath); ConvertTo-Json -InputObject $items -Compress';
    const result = await run('powershell.exe', ['-NoProfile', '-NonInteractive', '-Command', script], { windowsHide: true, timeout: 15000 });
    return JSON.parse(result.stdout.trim() || '[]');
}

function privateEnvironment(home) {
    const env = Object.fromEntries(Object.entries(process.env).filter(([, value]) => typeof value === 'string'));
    const entries = (env.WSLENV || '').split(':').filter(Boolean).filter(value => !['RAINY_HOME', 'RAINY_CONFIGURE_DEEPSEEK'].includes(value.split('/')[0]));
    env.WSLENV = [...entries, 'RAINY_HOME/u', 'RAINY_CONFIGURE_DEEPSEEK/u'].join(':');
    env.RAINY_HOME = home;
    env.RAINY_CONFIGURE_DEEPSEEK = '0';
    delete env.ELECTRON_RUN_AS_NODE;
    return env;
}

async function saveCarrierFiles(directory) {
    const entries = [];
    for (const name of ['host.json', 'desktop.json']) {
        const path = join(directory, name);
        try { entries.push({ path, bytes: await readFile(path) }); }
        catch (error) { if (error.code !== 'ENOENT') throw error; entries.push({ path, bytes: null }); }
    }
    return async () => {
        assert.equal((await rainyProcesses()).length, 0, 'Do not restore carrier metadata while another RainyAgent owns it.');
        for (const entry of entries) {
            if (entry.bytes !== null) await writeFile(entry.path, entry.bytes);
            else await unlink(entry.path).catch(error => { if (error.code !== 'ENOENT') throw error; });
        }
    };
}

async function installDialogRecorder(application) {
    await application.evaluate(({ dialog }) => {
        globalThis.__rainyNativeAcceptance = { dialogs: [], waiters: [], cancelExit: true };
        dialog.showMessageBox = async (...args) => {
            const options = args.at(-1), state = globalThis.__rainyNativeAcceptance;
            const record = { index: state.dialogs.length, message: options.message || '', detail: options.detail || '', buttons: options.buttons || [] };
            state.dialogs.push(record);
            for (const waiter of [...state.waiters]) if (record.index >= waiter.after && record.message === waiter.message) {
                state.waiters.splice(state.waiters.indexOf(waiter), 1); clearTimeout(waiter.timer); waiter.resolve(record);
            }
            return { response: state.cancelExit && record.buttons.includes('取消退出') ? record.buttons.indexOf('取消退出') : 0, checkboxChecked: false };
        };
    });
}

async function dialogCount(application) {
    return application.evaluate(() => globalThis.__rainyNativeAcceptance.dialogs.length);
}

function nextDialog(application, message, after) {
    const pending = application.evaluate((_electronModule, expected) => {
        const state = globalThis.__rainyNativeAcceptance;
        const existing = state.dialogs.find(item => item.index >= expected.after && item.message === expected.message);
        if (existing) return existing;
        return new Promise((resolveDialog, reject) => {
            const waiter = { ...expected, resolve: resolveDialog };
            waiter.timer = setTimeout(() => {
                state.waiters.splice(state.waiters.indexOf(waiter), 1);
                reject(new Error('Native dialog did not appear: ' + expected.message));
            }, 30000);
            state.waiters.push(waiter);
        });
    }, { message, after });
    void pending.catch(() => {});
    return pending;
}

async function trigger(application, kind) {
    if (kind === 'menu') {
        await application.evaluate(({ Menu, BrowserWindow }) => {
            const item = Menu.getApplicationMenu()?.items.find(item => item.label === '视图')?.submenu?.items.find(item => item.label === '重新加载');
            if (!item) throw new Error('The installed reload menu item is missing.');
            item.click(item, BrowserWindow.getAllWindows()[0], {});
        });
    } else if (kind === 'f5') {
        await application.evaluate(({ BrowserWindow }) => {
            const contents = BrowserWindow.getAllWindows()[0].webContents;
            contents.sendInputEvent({ type: 'keyDown', keyCode: 'F5' });
            contents.sendInputEvent({ type: 'keyUp', keyCode: 'F5' });
        });
    } else if (kind === 'close') {
        await application.evaluate(({ BrowserWindow }) => { BrowserWindow.getAllWindows()[0].close(); });
    } else throw new Error('Unknown native trigger');
}

async function windowAlive(application) {
    return application.evaluate(({ BrowserWindow }) => BrowserWindow.getAllWindows().some(window => !window.isDestroyed()));
}

async function openWorkbench(page) {
    await page.getByRole('button', { name: /^(CTF 工具|CTF tools)$/ }).click({ timeout: 60000 });
    const iframe = page.locator('[data-rainy-ctf-workbench] iframe');
    await iframe.waitFor({ state: 'visible', timeout: 60000 });
    assert.equal(await iframe.count(), 1);
    const frame = await (await iframe.elementHandle()).contentFrame();
    assert.ok(frame);
    await frame.waitForFunction(() => window.app && !window.app.toolLoading && !window.app.toolError, null, { timeout: 60000 });
    await frame.evaluate(async () => { await window.app.openTool('tokenizer'); });
    await frame.locator('#tokenizer-input').waitFor();
    return frame;
}

async function launch(home, report) {
    assert.equal((await rainyProcesses()).length, 0, 'An existing RainyAgent must be closed before installed validation.');
    const application = await _electron.launch({ executablePath, env: privateEnvironment(home), timeout: startupTimeout });
    let startedHost;
    try {
        await installDialogRecorder(application);
        const page = await application.firstWindow({ timeout: startupTimeout });
        page.setDefaultTimeout(30000);
        await page.waitForURL(url => url.protocol === 'http:' && url.hostname === '127.0.0.1', { timeout: startupTimeout });
        const userData = await application.evaluate(({ app }) => app.getPath('userData'));
        const host = JSON.parse(await readFile(join(userData, 'host.json'), 'utf8'));
        startedHost = host;
        assert.equal(host.home, home, 'Refuse to edit drafts outside the private acceptance Host.');
        assert.equal(new URL(page.url()).origin, host.origin);
        const identity = await application.evaluate(({ app }) => ({ version: app.getVersion(), packaged: app.isPackaged, executable: process.execPath, pid: process.pid }));
        assert.equal(identity.packaged, true, 'Acceptance requires the installed packaged app.');
        assert.equal(resolve(identity.executable).toLowerCase(), resolve(executablePath).toLowerCase());
        report.launches.push({ ...identity, host, userData });
        const errors = [];
        page.on('pageerror', error => errors.push(error.message));
        const frame = await openWorkbench(page);
        const draftContext = await frame.evaluate(() => window.IceSkyRuntime.context);
        assert.equal(draftContext.kind, 'standalone', 'A private Host with only its initial blank chat must open the independent workbench draft.');
        const scope = draftContext.kind === 'standalone' ? 'standalone' : `session:${draftContext.id}`;
        report.launches.at(-1).workbenchAssetPath = new URL(frame.url()).pathname;
        report.launches.at(-1).draftContext = draftContext;
        return { application, page, frame, host, scope, errors, gates: new Set() };
    } catch (error) {
        report.dialogs.push(...await application.evaluate(() => globalThis.__rainyNativeAcceptance?.dialogs || []).catch(() => []));
        await application.close().catch(() => {});
        if (startedHost) await assertHostStopped(startedHost);
        throw error;
    }
}

async function readSaved(state) {
    const response = await state.application.context().request.get(`${state.host.origin}/rainy/icesky/state?scope=${encodeURIComponent(state.scope)}`);
    assert.equal(response.ok(), true);
    return response.json();
}

async function holdWrite(state, expectedText) {
    const entered = deferred(), release = deferred(); let captured = false;
    const handler = async route => {
        const request = route.request();
        if (request.method() === 'PUT' && !captured && request.postDataJSON()?.data?.tools?.tokenizer?.fields?.tokenizerInput === expectedText) {
            captured = true; entered.resolve(); await release.promise;
        }
        await route.continue();
    };
    await state.application.context().route(statePattern(state), handler);
    const gate = { entered: entered.promise, release: () => release.resolve(), async remove() { release.resolve(); state.gates.delete(gate); await state.application.context().unroute(statePattern(state), handler); } };
    state.gates.add(gate);
    return gate;
}

async function verifyNativeReload(state, kind, text, report) {
    const marker = randomUUID();
    await state.page.evaluate(marker => { window.__nativeAcceptanceDocument = marker; }, marker);
    const gate = await holdWrite(state, text);
    try {
        await state.frame.locator('#tokenizer-input').fill(text);
        const navigated = state.page.waitForEvent('domcontentloaded', { timeout: lifecycleTimeout });
        void navigated.catch(() => {});
        await trigger(state.application, kind);
        await bounded(gate.entered, 'Native reload did not request a save.');
        assert.equal(await state.page.evaluate(() => window.__nativeAcceptanceDocument), marker, 'The document must remain while its save is pending.');
        assert.equal(await windowAlive(state.application), true);
        gate.release(); await navigated;
        state.frame = await openWorkbench(state.page);
        const context = await state.frame.evaluate(() => window.IceSkyRuntime.context);
        assert.equal(context.kind === 'standalone' ? 'standalone' : `session:${context.id}`, state.scope, 'Native reload must return to the original draft context.');
        assert.equal(await state.frame.locator('#tokenizer-input').inputValue(), text);
        assert.equal(await state.page.evaluate(() => window.__nativeAcceptanceDocument), undefined);
        assert.equal((await readSaved(state)).data.tools.tokenizer.fields.tokenizerInput, text);
        report.cases.push({ name: kind === 'menu' ? 'native menu reload' : 'native F5 reload', waitedForSave: true, restoredFinalDraft: true });
    } finally { await gate.remove().catch(() => {}); }
}

async function assertHostStopped(host) {
    const code = 'import json,os,sys; p="/proc/"+sys.argv[1]+"/cmdline"; a=open(p,"rb").read().split(b"\\0") if os.path.exists(p) else []; print(json.dumps({"sameRuntimeAlive":sys.argv[2].encode() in a}))';
    const result = await run('wsl.exe', ['-d', host.distro, '--exec', 'python3', '-c', code, String(host.pid), host.runtime], { windowsHide: true, timeout: 15000 });
    assert.equal(JSON.parse(result.stdout).sameRuntimeAlive, false, 'The private WSL Host must exit with the installed window.');
}

async function orderlyClose(state) {
    for (const gate of state.gates) gate.release();
    const process = state.application.process();
    if (process.exitCode !== null || process.signalCode !== null) { await assertHostStopped(state.host); return; }
    await state.application.context().unrouteAll({ behavior: 'wait' });
    await state.application.evaluate(() => { globalThis.__rainyNativeAcceptance.cancelExit = false; });
    const exited = state.application.waitForEvent('close', { timeout: lifecycleTimeout });
    void exited.catch(() => {});
    await trigger(state.application, 'close');
    await exited;
    await assertHostStopped(state.host);
}

async function main() {
    if (!process.argv.includes('--installed')) throw new Error('Pass --installed only after the new RainyAgent installation has been verified.');
    assert.equal(process.platform, 'win32', 'This acceptance driver targets the installed Windows app.');
    await access(executablePath);
    assert.equal((await rainyProcesses()).length, 0, 'Close existing RainyAgent windows before starting acceptance.');
    const id = randomUUID();
    const home = `/tmp/rainy-icesky-native-${id}`;
    const output = resolve(option('--output') || `apps/rainy-desktop/validation/icesky-desktop-${id}`);
    await mkdir(output, { recursive: true });
    const restoreCarrier = await saveCarrierFiles(join(process.env.APPDATA, 'RainyAgent'));
    const report = { executablePath, home, output, launches: [], cases: [], screenshots: [], errors: [], dialogs: [], cleanup: {} };
    let state;
    try {
        state = await launch(home, report);
        await state.frame.evaluate(() => window.IceSkyRuntime.flush());
        await verifyNativeReload(state, 'menu', 'Native menu final draft 示例🙂', report);
        await verifyNativeReload(state, 'f5', 'Native F5 final draft 示例🙂', report);

        const savedBeforeFailure = await readSaved(state);
        const blockedText = 'Unsaved native close recovery 示例🙂';
        const blockSave = route => route.request().method() === 'PUT'
            ? route.fulfill({ status: 503, contentType: 'application/json', body: JSON.stringify({ error: 'Native acceptance storage unavailable' }) })
            : route.continue();
        await state.application.context().route(statePattern(state), blockSave);
        await state.frame.locator('#tokenizer-input').fill(blockedText);
        const marker = randomUUID(); await state.page.evaluate(marker => { window.__nativeAcceptanceDocument = marker; }, marker);
        const reloadFailure = nextDialog(state.application, '刷新前草稿尚未保存', await dialogCount(state.application));
        await trigger(state.application, 'menu'); await reloadFailure;
        assert.equal(await state.page.evaluate(() => window.__nativeAcceptanceDocument), marker);
        assert.equal(await state.frame.locator('#tokenizer-input').inputValue(), blockedText);
        assert.equal(await state.frame.evaluate(() => window.app.saveState), 'error');
        assert.equal((await readSaved(state)).data.tools.tokenizer.fields.tokenizerInput, savedBeforeFailure.data.tools.tokenizer.fields.tokenizerInput);
        report.cases.push({ name: '503 blocks native reload', windowPreserved: await windowAlive(state.application), memoryPreserved: true, durableDraftPreserved: true });

        const closeFailure = nextDialog(state.application, '草稿尚未保存', await dialogCount(state.application));
        await trigger(state.application, 'close'); const closeDialog = await closeFailure;
        assert(closeDialog.buttons.includes('取消退出'));
        assert.equal(await windowAlive(state.application), true);
        assert.equal(await state.frame.locator('#tokenizer-input').inputValue(), blockedText);
        report.cases.push({ name: '503 native close cancelled', windowPreserved: true, memoryPreserved: true });
        const failureScreenshot = join(output, 'save-failure-cancelled.png'); await state.page.screenshot({ path: failureScreenshot }); report.screenshots.push(failureScreenshot);
        await state.application.context().unroute(statePattern(state), blockSave);

        const exitText = 'Final native exit draft 示例🙂';
        const gate = await holdWrite(state, exitText);
        await state.frame.locator('#tokenizer-input').fill(exitText);
        const exited = state.application.waitForEvent('close', { timeout: lifecycleTimeout });
        void exited.catch(() => {});
        await trigger(state.application, 'close');
        await bounded(gate.entered, 'Native exit did not request a save.');
        assert.equal(await windowAlive(state.application), true);
        const exitScreenshot = join(output, 'exit-waits-for-save.png'); await state.page.screenshot({ path: exitScreenshot }); report.screenshots.push(exitScreenshot);
        report.dialogs.push(...await state.application.evaluate(() => globalThis.__rainyNativeAcceptance.dialogs));
        gate.release(); await exited; await assertHostStopped(state.host);
        const previousOrigin = state.host.origin, previousScope = state.scope; report.errors.push(...state.errors); state = null;
        report.cases.push({ name: 'native exit waits for save', hostStopped: true });

        state = await launch(home, report);
        assert.notEqual(state.host.origin, previousOrigin, 'This run must demonstrate recovery after a Host port change.');
        assert.equal(state.scope, previousScope, 'The installed relaunch must restore the original draft context.');
        assert.equal(await state.frame.locator('#tokenizer-input').inputValue(), exitText);
        assert.equal((await readSaved(state)).data.tools.tokenizer.fields.tokenizerInput, exitText);
        report.cases.push({ name: 'installed relaunch restores across Host ports', previousOrigin, nextOrigin: state.host.origin, restoredFinalDraft: true });
        const restoredScreenshot = join(output, 'relaunch-restored.png'); await state.page.screenshot({ path: restoredScreenshot }); report.screenshots.push(restoredScreenshot);
        report.dialogs.push(...await state.application.evaluate(() => globalThis.__rainyNativeAcceptance.dialogs));
        report.errors.push(...state.errors);
        assert.deepEqual(report.errors, [], 'Installed renderer emitted unexpected exceptions.');
        await orderlyClose(state); state = null;
        report.cleanup.ownedHostStopped = true;
        report.passed = true;
    } catch (error) {
        report.passed = false; report.failure = error.stack;
        if (state) {
            report.errors.push(...state.errors);
            const path = join(output, 'failure.png');
            await state.page.screenshot({ path }).then(() => report.screenshots.push(path), () => {});
        }
        process.exitCode = 1;
    } finally {
        if (state) {
            try { await orderlyClose(state); report.cleanup.ownedHostStopped = true; }
            catch (error) { report.cleanup.closeError = error.message; process.exitCode = 1; }
        }
        try { await restoreCarrier(); report.cleanup.carrierMetadataRestored = true; }
        catch (error) { report.cleanup.restoreError = error.message; process.exitCode = 1; }
        try {
            report.cleanup.remainingRainyProcesses = await rainyProcesses();
            if (report.cleanup.remainingRainyProcesses.length) process.exitCode = 1;
        } catch (error) { report.cleanup.processCheckError = error.message; process.exitCode = 1; }
        report.passed = report.passed === true && process.exitCode !== 1;
        await writeFile(join(output, 'report.json'), JSON.stringify(report, null, 2) + '\n');
        console.log(JSON.stringify({ passed: report.passed && process.exitCode !== 1, output, cases: report.cases, cleanup: report.cleanup, failure: report.failure }));
    }
}

await main();
