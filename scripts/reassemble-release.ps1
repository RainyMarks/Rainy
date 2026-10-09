<# Reassemble original release paths and verify both transport pieces and complete files. #>
param(
    [Parameter(Mandatory = $true)][string]$Manifest,
    [Parameter(Mandatory = $true)][string]$PartsDirectory,
    [Parameter(Mandatory = $true)][string]$OutputDirectory
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

function Assert-ReleasePath([string]$Name) {
    if (-not $Name -or $Name.Length -gt 220) { throw "Unsafe release path: $Name" }
    foreach ($part in $Name.Split('/')) {
        if (-not $part -or $part -eq '.' -or $part -eq '..' -or $part -match '[\\:<>"|?*\x00-\x1f]' -or
            $part -match '[. ]$' -or $part -match '^(?i:con|prn|aux|nul|com[1-9]|lpt[1-9])(?:\.|$)') {
            throw "Unsafe release path: $Name"
        }
    }
}

function Resolve-ReleasePath([string]$Root, [string]$Name) {
    Assert-ReleasePath $Name
    $current = [IO.Path]::GetFullPath($Root)
    foreach ($part in @('') + $Name.Split('/')) {
        if ($part) { $current = Join-Path $current $part }
        if (Test-Path -LiteralPath $current) {
            $item = Get-Item -LiteralPath $current -Force
            if ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) { throw "Release path contains a link: $current" }
        }
    }
    return $current
}

function Assert-Digest([string]$Path, $Expected) {
    $item = Get-Item -LiteralPath $Path -Force
    if ($item.PSIsContainer -or ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -or $item.Length -ne $Expected.bytes) {
        throw "Release file size or type differs: $Path"
    }
    $algorithm = [Security.Cryptography.SHA256]::Create()
    $stream = [IO.File]::OpenRead($Path)
    try { $digest = [BitConverter]::ToString($algorithm.ComputeHash($stream)).Replace('-', '').ToLowerInvariant() }
    finally { $stream.Dispose(); $algorithm.Dispose() }
    if ($digest -cne $Expected.sha256) {
        throw "Release checksum differs: $Path"
    }
}

$data = Get-Content -LiteralPath $Manifest -Raw -Encoding UTF8 | ConvertFrom-Json
if ($data.version -ne 1 -or $data.releaseVersion -notmatch '^\d+\.\d+\.\d+(?:[-+][A-Za-z0-9.-]+)?$' -or
    $data.partBytes -lt 1 -or $data.partBytes -gt 1073741824 -or @($data.files).Count -eq 0) { throw 'Invalid release manifest' }
$paths = @{}
$pieces = @{ 'release-assets.json' = $true }
foreach ($entry in $data.files) {
    Assert-ReleasePath $entry.path
    $key = $entry.path.ToLowerInvariant()
    if ($paths.ContainsKey($key)) { throw "Release path collision: $($entry.path)" }
    foreach ($existing in $paths.Keys) {
        if ($existing.StartsWith($key + '/') -or $key.StartsWith($existing + '/')) { throw "Release path collision: $($entry.path)" }
    }
    $paths[$key] = $true
    if ($entry.category -notin @('installer', 'offline', 'build-input') -or $entry.bytes -lt 0 -or
        [math]::Truncate($entry.bytes) -ne $entry.bytes -or $entry.sha256 -cnotmatch '^[a-f0-9]{64}$' -or @($entry.pieces).Count -eq 0) {
        throw "Invalid release file: $($entry.path)"
    }
    [long]$total = 0
    foreach ($piece in $entry.pieces) {
        Assert-ReleasePath $piece.file
        if ($piece.file.Contains('/') -or $pieces.ContainsKey($piece.file.ToLowerInvariant()) -or $piece.bytes -lt 0 -or
            [math]::Truncate($piece.bytes) -ne $piece.bytes -or $piece.bytes -ge 2147483648 -or
            ($entry.category -ne 'installer' -and $piece.bytes -gt $data.partBytes) -or $piece.sha256 -cnotmatch '^[a-f0-9]{64}$') {
            throw "Invalid release piece: $($piece.file)"
        }
        $pieces[$piece.file.ToLowerInvariant()] = $true
        $total += $piece.bytes
    }
    if ($total -ne $entry.bytes) { throw "Release piece sizes differ: $($entry.path)" }
    if ($entry.category -eq 'installer' -and (@($entry.pieces).Count -ne 1 -or
        $entry.pieces[0].file -cne [IO.Path]::GetFileName($entry.path) -or $entry.pieces[0].sha256 -cne $entry.sha256)) {
        throw 'The installer must remain one raw asset'
    }
}
$partsRoot = [IO.Path]::GetFullPath($PartsDirectory)
$outputRoot = [IO.Path]::GetFullPath($OutputDirectory)
if ($partsRoot.TrimEnd([char]92, [char]47) -ieq $outputRoot.TrimEnd([char]92, [char]47)) { throw 'Parts and output directories must differ' }
[void][IO.Directory]::CreateDirectory($outputRoot)
foreach ($entry in $data.files) {
    $target = Resolve-ReleasePath $outputRoot $entry.path
    if (Test-Path -LiteralPath $target) { Assert-Digest $target $entry; continue }
    foreach ($piece in $entry.pieces) { Assert-Digest (Resolve-ReleasePath $partsRoot $piece.file) $piece }
    [void][IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($target))
    $temporary = $target + '.' + [guid]::NewGuid().ToString('N') + '.pending'
    try {
        $output = [IO.File]::Open($temporary, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
        try {
            foreach ($piece in $entry.pieces) {
                $inputFile = [IO.File]::OpenRead((Resolve-ReleasePath $partsRoot $piece.file))
                try { $inputFile.CopyTo($output, 1048576) } finally { $inputFile.Dispose() }
            }
        } finally { $output.Dispose() }
        Assert-Digest $temporary $entry
        [IO.File]::Move($temporary, $target)
    } finally {
        if (Test-Path -LiteralPath $temporary) { Remove-Item -LiteralPath $temporary -Force }
    }
    Write-Output "Verified: $($entry.path)"
}
Write-Output "Release $($data.releaseVersion) restored: $outputRoot"
