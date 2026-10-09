/** Pinned official installation media; packaging must verify these exact bytes. */
export const environmentMedia = {
  wsl: {
    version: '3.0.1.0',
    file: 'wsl.3.0.1.0.x64.msi',
    url: 'https://github.com/microsoft/WSL/releases/download/3.0.1/wsl.3.0.1.0.x64.msi',
    sha256: '28b1a0d013640a2ac95898ea705fa186e5b4ff767a1c1b49257161bc106599c6',
    bytes: 367669248,
  },
  ubuntu: {
    version: '26.04.1',
    file: 'ubuntu-26.04.1-wsl-amd64.wsl',
    url: 'https://releases.ubuntu.com/26.04.1/ubuntu-26.04.1-wsl-amd64.wsl',
    sha256: '48d56724b5c8e60f24893e83e73bbb58c60b3ca22fba3da977075420acd54104',
    bytes: 418495746,
  },
} as const
