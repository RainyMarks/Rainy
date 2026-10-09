<# Build the Windows installer and the offline release directory from a checkout with restored release inputs. #>
param([string]$Distribution = 'Ubuntu', [string]$NativeToolsSource, [string]$NativeToolsStage,
    [string]$ReuseNativeToolsRelease, [string]$ComponentSource, [string]$SevenZip, [string]$StrataArchive)
$ErrorActionPreference = 'Stop'
$rainyApp = Split-Path $PSScriptRoot -Parent
$rainyVersion = (Get-Content -LiteralPath (Join-Path $rainyApp 'package.json') -Raw | ConvertFrom-Json).version
$rainyStage = if ($NativeToolsStage) { [IO.Path]::GetFullPath($NativeToolsStage) } else { Join-Path $rainyApp "toolpacks/stage-$rainyVersion" }
$rainyOffline = Join-Path $rainyApp "release/offline-$rainyVersion"
function Check-Exit { if ($LASTEXITCODE -ne 0) { throw "Build step failed with exit code $LASTEXITCODE" } }
Push-Location $rainyApp
try {
    if ($ReuseNativeToolsRelease -and $NativeToolsSource) { throw 'Choose a verified prior tool release or a new native-tool source.' }
    if ($NativeToolsSource) {
        if (-not $SevenZip) { throw 'Pass -SevenZip with a 7-Zip executable for the pinned application archives.' }
        & node (Join-Path $PSScriptRoot 'prepare-native-tools.mjs') --source $NativeToolsSource --stage $rainyStage --seven-zip $SevenZip; Check-Exit
    }
    if (-not $ReuseNativeToolsRelease) {
        & node (Join-Path $PSScriptRoot 'prepare-native-tools.mjs') --stage $rainyStage --verify; Check-Exit
        & node (Join-Path $PSScriptRoot 'package-native-tools.mjs') --stage $rainyStage --output $rainyOffline; Check-Exit
    }
    & node --import tsx (Join-Path $PSScriptRoot 'prepare-environment-media.mjs'); Check-Exit
    $rainyInputs = @('--output', $rainyOffline)
    if ($ReuseNativeToolsRelease) { $rainyInputs += @('--tools', [IO.Path]::GetFullPath($ReuseNativeToolsRelease)) }
    if ($ComponentSource) { $rainyInputs += @('--components', [IO.Path]::GetFullPath($ComponentSource)) }
    & node --import tsx (Join-Path $PSScriptRoot 'prepare-release-inputs.ts') @rainyInputs; Check-Exit
    & node --import tsx (Join-Path $PSScriptRoot 'build.ts') --release; Check-Exit
    & node (Join-Path $PSScriptRoot 'runtime-graph.mjs') --windows; Check-Exit
    & node (Join-Path $PSScriptRoot 'stage-windows.mjs'); Check-Exit
    & node (Join-Path $PSScriptRoot 'runtime-graph.mjs'); Check-Exit
    $stageScript = (& wsl.exe -d $Distribution --exec wslpath -u (Join-Path $PSScriptRoot 'stage-linux.py')).Trim(); Check-Exit
    $runtimeDir = (& wsl.exe -d $Distribution --exec wslpath -u (Join-Path $rainyApp 'runtime')).Trim(); Check-Exit
    & wsl.exe -d $Distribution --exec python3 $stageScript --graph "$runtimeDir/graph.json" --output $runtimeDir; Check-Exit
    # Strata, PHP and the Linux runtime ship as downloadable components; their pieces go to the resources release.
    $rainyPieces = Join-Path $rainyApp "release/resources-$rainyVersion"
    foreach ($rainyModule in @('php', 'linux-runtime')) { Remove-Item -LiteralPath (Join-Path $rainyPieces $rainyModule) -Recurse -Force -ErrorAction SilentlyContinue }
    $rainyStrata = if ($StrataArchive) { [IO.Path]::GetFullPath($StrataArchive) } else { Join-Path $rainyOffline 'build-inputs/strata-runtime.tar.gz' }
    & node (Join-Path $PSScriptRoot 'prepare-optional-modules.mjs') --version $rainyVersion --pieces $rainyPieces --strata-archive $rainyStrata; Check-Exit
    & node (Join-Path $PSScriptRoot 'prepare-shell.mjs'); Check-Exit
    & node (Join-Path $rainyApp 'node_modules/electron-builder/out/cli/cli.js') --config electron-builder.config.cjs --win nsis --publish never; Check-Exit
    $rainyInstaller = Join-Path $rainyApp "release/RainyAgent-$rainyVersion-windows-x64-setup.exe"
    Copy-Item -LiteralPath $rainyInstaller -Destination $rainyOffline -Force
    Copy-Item -LiteralPath (Join-Path $rainyApp 'resources/offline-readme.txt') -Destination (Join-Path $rainyOffline '安装说明.txt') -Force
    if (-not $ReuseNativeToolsRelease) { Copy-Item -LiteralPath (Join-Path $rainyStage 'tools/manifest.json') -Destination (Join-Path $rainyOffline '工具清单.json') -Force }
    $rainyChecksums = Get-ChildItem -LiteralPath $rainyOffline -File -Recurse | Where-Object { $_.Name -ne 'SHA256SUMS.txt' } | Sort-Object FullName | ForEach-Object {
        $rainyHash = (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
        $rainyRelative = $_.FullName.Substring($rainyOffline.Length + 1).Replace('\', '/')
        "$rainyHash  $rainyRelative"
    }
    [IO.File]::WriteAllText((Join-Path $rainyOffline 'SHA256SUMS.txt'), ($rainyChecksums -join "`n") + "`n", [Text.UTF8Encoding]::new($false))
    Write-Host "Offline release: $rainyOffline"
} finally { Pop-Location }
