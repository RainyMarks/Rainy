<# Build the Windows installer and the offline release directory from a checkout with restored release inputs. #>
# Saved as UTF-8 with a BOM so Windows PowerShell 5.1 reads the Chinese offline file names below correctly.
param([string]$Distribution = 'Ubuntu', [string]$ComponentSource)
$ErrorActionPreference = 'Stop'
$rainyApp = Split-Path $PSScriptRoot -Parent
# The native application (native/Cargo.toml) gives the release its version; the viewer page (package.json) carries the same one.
$rainyVersion = [regex]::Match((Get-Content -LiteralPath (Join-Path $rainyApp 'native/Cargo.toml') -Raw), '(?ms)^\[workspace\.package\].*?^version\s*=\s*"([^"]+)"').Groups[1].Value
$rainyHostVersion = (Get-Content -LiteralPath (Join-Path $rainyApp 'package.json') -Raw | ConvertFrom-Json).version
if (-not $rainyVersion) { throw 'native/Cargo.toml has no [workspace.package] version' }
if ($rainyHostVersion -ne $rainyVersion) { throw "package.json is version $rainyHostVersion, native/Cargo.toml $rainyVersion; release them together" }
# Windows Python prepares the Windows IDE tools; WSL's python3 the Linux ones.
function Find-Python {
    if ($env:RAINY_PYTHON) { return @($env:RAINY_PYTHON) }
    if (Get-Command py.exe -ErrorAction SilentlyContinue) { return @('py.exe', '-3') }
    if (Get-Command python.exe -ErrorAction SilentlyContinue) { return @('python.exe') }
    throw 'Install Python 3 for Windows, or set RAINY_PYTHON to its python.exe'
}
$rainyOffline = Join-Path $rainyApp "release/offline-$rainyVersion"
function Check-Exit { if ($LASTEXITCODE -ne 0) { throw "Build step failed with exit code $LASTEXITCODE" } }
# Git for Windows' bash (or MSYS2's) runs the native build script; System32\bash.exe would be WSL.
function Find-Bash {
    if ($env:RAINY_BASH) { return $env:RAINY_BASH }
    $git = Get-Command git.exe -ErrorAction SilentlyContinue
    if ($git) {
        $candidate = Join-Path (Split-Path (Split-Path $git.Source -Parent) -Parent) 'bin\bash.exe'
        if (Test-Path -LiteralPath $candidate) { return $candidate }
    }
    if (Test-Path -LiteralPath 'C:\msys64\usr\bin\bash.exe') { return 'C:\msys64\usr\bin\bash.exe' }
    throw 'Install Git for Windows or MSYS2, or set RAINY_BASH to its bash.exe'
}
Push-Location $rainyApp
try {
    & node --import tsx (Join-Path $PSScriptRoot 'prepare-environment-media.mjs'); Check-Exit
    $rainyInputs = @('--output', $rainyOffline)
    if ($ComponentSource) { $rainyInputs += @('--components', [IO.Path]::GetFullPath($ComponentSource)) }
    & node --import tsx (Join-Path $PSScriptRoot 'prepare-release-inputs.ts') @rainyInputs; Check-Exit
    # The IDE tools of both platforms (Ruff, ty, TypeScript 7, debugpy, js-debug), pinned in toolpacks/ide-tools.sources.json.
    $rainyPython = @(Find-Python)
    $rainyPythonArgs = @($rainyPython | Select-Object -Skip 1) + @((Join-Path $PSScriptRoot 'prepare-ide-tools.py'), '--platform', 'win32-x64')
    & $rainyPython[0] @rainyPythonArgs; Check-Exit
    $ideScript = (& wsl.exe -d $Distribution --exec wslpath -u (Join-Path $PSScriptRoot 'prepare-ide-tools.py')).Trim(); Check-Exit
    & wsl.exe -d $Distribution --exec python3 $ideScript --platform linux-x64; Check-Exit
    & node --import tsx (Join-Path $PSScriptRoot 'build.ts') --release; Check-Exit
    # The Rust Host for Windows and, through cargo-zigbuild, for Linux.
    & (Find-Bash) ((Join-Path $rainyApp 'native/scripts/build-hosts.sh').Replace('\', '/')); Check-Exit
    & node (Join-Path $PSScriptRoot 'stage-windows.mjs'); Check-Exit
    $stageScript = (& wsl.exe -d $Distribution --exec wslpath -u (Join-Path $PSScriptRoot 'stage-linux.py')).Trim(); Check-Exit
    $runtimeDir = (& wsl.exe -d $Distribution --exec wslpath -u (Join-Path $rainyApp 'runtime')).Trim(); Check-Exit
    & wsl.exe -d $Distribution --exec python3 $stageScript --output $runtimeDir; Check-Exit
    # The editor's Windows tools, PHP, the Linux runtime base and the offline Ubuntu packages ship as downloadable components; their pieces go to the resources release.
    $rainyPieces = Join-Path $rainyApp "release/resources-$rainyVersion"
    foreach ($rainyModule in @('ide-python', 'ide-web', 'php', 'linux-runtime', 'linux-dev-packages')) { Remove-Item -LiteralPath (Join-Path $rainyPieces $rainyModule) -Recurse -Force -ErrorAction SilentlyContinue }
    & node (Join-Path $PSScriptRoot 'prepare-optional-modules.mjs') --version $rainyVersion --pieces $rainyPieces; Check-Exit
    # RainyAgent.exe carries the public release keys the resources are signed with; package-native.mjs stages, signs and
    # builds the NSIS installer, latest.yml and latest.yml.sig in release/.
    $env:RAINY_RELEASE_PUBLIC_KEYS = Join-Path $rainyApp 'runtime\release-public-keys.json'
    try {
        & (Find-Bash) ((Join-Path $rainyApp 'native/scripts/build-release.sh').Replace('\', '/')); Check-Exit
    } finally { Remove-Item Env:\RAINY_RELEASE_PUBLIC_KEYS -ErrorAction SilentlyContinue }
    & node (Join-Path $PSScriptRoot 'package-native.mjs'); Check-Exit
    $rainyInstaller = Join-Path $rainyApp "release/Rainy-$rainyVersion-windows-x64-setup.exe"
    Copy-Item -LiteralPath $rainyInstaller -Destination $rainyOffline -Force
    Copy-Item -LiteralPath (Join-Path $rainyApp 'native/installer/offline-readme.txt') -Destination (Join-Path $rainyOffline '安装说明.txt') -Force
    $rainyChecksums = Get-ChildItem -LiteralPath $rainyOffline -File -Recurse | Where-Object { $_.Name -ne 'SHA256SUMS.txt' } | Sort-Object FullName | ForEach-Object {
        $rainyHash = (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
        $rainyRelative = $_.FullName.Substring($rainyOffline.Length + 1).Replace('\', '/')
        "$rainyHash  $rainyRelative"
    }
    [IO.File]::WriteAllText((Join-Path $rainyOffline 'SHA256SUMS.txt'), ($rainyChecksums -join "`n") + "`n", [Text.UTF8Encoding]::new($false))
    Write-Host "Offline release: $rainyOffline"
} finally { Pop-Location }
