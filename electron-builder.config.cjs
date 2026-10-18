const { version } = require('./package.json');

// The bundled NSIS extractor supports BCJ, but not 7-Zip's automatic ARM64 filter.
process.env.ELECTRON_BUILDER_7Z_FILTER = 'BCJ';

module.exports = {
  // Rainy is the product name; the executable keeps its earlier name so installed shortcuts, pinned icons and the
  // uninstaller steps in resources/installer.nsh keep working across the rename.
  appId: 'dev.rainy.agent', productName: 'Rainy', executableName: 'RainyAgent',
  publish: { provider: 'github', owner: 'RainyMarks', repo: 'Rainy', releaseType: 'release' },
  // scripts/prepare-shell.mjs stages build/shell from dist/main.cjs, dist/preload.cjs and dist/setup.
  directories: { app: 'build/shell', output: 'release' },
  files: ['package.json', 'main.cjs', 'preload.cjs', 'setup/**', 'LICENSE', 'THIRD_PARTY_NOTICES.md', '!**/node_modules/**'],
  extraResources: [
    // Strata, PHP and the Linux runtime are downloaded when first needed; this pins their archives.
    { from: 'runtime/optional-modules.json', to: 'optional-modules.json' },
    { from: 'build/icon.ico', to: 'icon.ico' },
    { from: 'runtime/linux-runtime.json', to: 'linux-runtime.json' },
    // The Linux Host's application layer: a version update with an unchanged base downloads nothing.
    { from: 'runtime/linux-app.tar.gz', to: 'linux-app.tar.gz' },
    { from: 'scripts/install-runtime.py', to: 'install-runtime.py' },
    { from: 'runtime/release-public-keys.json', to: 'release-public-keys.json' },
    { from: 'runtime/environment-component-catalog.json', to: 'environment-component-catalog.json' },
    { from: 'scripts/install-environment-component.py', to: 'install-environment-component.py' },
  ],
  npmRebuild: false,
  // Chromium's own strings (spellcheck, built-in dialogs) for the two interface languages; other locales fall back to en-US.
  electronLanguages: ['zh-CN', 'en-US'],
  afterPack: './scripts/after-pack.cjs',
  asar: true,
  electronFuses: { runAsNode: false, enableNodeOptionsEnvironmentVariable: false,
    enableNodeCliInspectArguments: false, enableEmbeddedAsarIntegrityValidation: true, onlyLoadAppFromAsar: true },
  win: { target: [{ target: 'nsis', arch: ['x64'] }], icon: 'build/icon.ico', signExecutable: Boolean(process.env.CSC_LINK) },
  nsis: { oneClick: false, perMachine: false, allowToChangeInstallationDirectory: true, createDesktopShortcut: true, shortcutName: 'Rainy', deleteAppDataOnUninstall: false, include: 'resources/installer.nsh' },
  artifactName: 'Rainy-${version}-windows-x64-setup.${ext}',
};
