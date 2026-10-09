const { version } = require('./package.json');

// The bundled NSIS extractor supports BCJ, but not 7-Zip's automatic ARM64 filter.
process.env.ELECTRON_BUILDER_7Z_FILTER = 'BCJ';

module.exports = {
  appId: 'dev.rainy.agent', productName: 'RainyAgent',
  publish: { provider: 'github', owner: 'RainyMarks', repo: 'RainyAgent', releaseType: 'release' },
  // scripts/prepare-shell.mjs stages build/shell from dist/main.cjs, dist/preload.cjs and dist/setup.
  directories: { app: 'build/shell', output: 'release' },
  files: ['package.json', 'main.cjs', 'preload.cjs', 'setup/**', 'LICENSE', 'THIRD_PARTY_NOTICES.md', '!**/node_modules/**'],
  extraResources: [
    // The signed per-tool channel; tools themselves are downloaded one by one.
    { from: 'toolpacks/native-tools-channel.v2.signed.json', to: 'native-tools-channel.signed.json' },
    { from: 'resources/native-tools-public-keys.json', to: 'native-tools-public-keys.json' },
    // Strata, PHP and the Linux runtime are downloaded when first needed; this pins their archives.
    { from: 'runtime/optional-modules.json', to: 'optional-modules.json' },
    { from: 'build/icon.ico', to: 'icon.ico' },
    { from: 'runtime/linux-runtime.json', to: 'linux-runtime.json' },
    { from: 'scripts/install-runtime.py', to: 'install-runtime.py' },
    { from: 'runtime/release-public-keys.json', to: 'release-public-keys.json' },
    { from: 'runtime/environment-component-catalog.json', to: 'environment-component-catalog.json' },
    { from: 'scripts/install-environment-component.py', to: 'install-environment-component.py' },
  ],
  npmRebuild: false,
  afterPack: './scripts/after-pack.cjs',
  asar: true,
  electronFuses: { runAsNode: false, enableNodeOptionsEnvironmentVariable: false,
    enableNodeCliInspectArguments: false, enableEmbeddedAsarIntegrityValidation: true, onlyLoadAppFromAsar: true },
  win: { target: [{ target: 'nsis', arch: ['x64'] }], icon: 'build/icon.ico', signExecutable: Boolean(process.env.CSC_LINK) },
  nsis: { oneClick: false, perMachine: false, allowToChangeInstallationDirectory: true, createDesktopShortcut: true, shortcutName: 'RainyAgent', deleteAppDataOnUninstall: false, include: 'resources/native-tools-installer.nsh' },
  artifactName: 'RainyAgent-${version}-windows-x64-setup.${ext}',
};
