# Whether a process still runs from the Rainy installation (3.x customCheckAppRunning, with a wait): its executable lies
# under one of the normalized install roots, or its command line names a script/binary under one (also inside a quoted
# command string or -EncodedCommand).
# Strict directory-prefix matching avoids false positives on external editors or terminals merely mentioning the folder.
# Nothing is ever ended.
# Exit 0 when none does, 2 when one still does after WaitSeconds (printed as "Busy: <pid> <name>"), 1 when the check failed.
param([string]$InstallRoot, [string]$PreviousUserRoot, [string]$PreviousMachineRoot, [int]$InstallerPid, [int]$WaitSeconds = 0)
$ErrorActionPreference = 'Stop'
$env:PSModulePath = [IO.Path]::Combine($PSHOME, 'Modules')
$script:ExecutableExts = @('.exe', '.com', '.dll', '.ps1', '.py', '.js', '.mjs', '.cjs', '.bat', '.cmd')
function Normalize-Dir([string]$value) {
  if ([string]::IsNullOrWhiteSpace($value)) { return '' }
  $trimmed = $value.Trim().Trim('"', "'")
  if ($trimmed.StartsWith('\\?\')) { $trimmed = $trimmed.Substring(4) }
  try { $full = [IO.Path]::GetFullPath($trimmed) } catch { $full = $trimmed }
  return $full.Replace('/', '\').TrimEnd([char]92).ToLowerInvariant()
}
function Normalize-Candidate([string]$value) {
  if ([string]::IsNullOrWhiteSpace($value)) { return '' }
  $trimmed = $value.Trim().Trim('"', "'", ';', ',', ')', '(')
  if ($trimmed.StartsWith('\\?\')) { $trimmed = $trimmed.Substring(4) }
  if ($trimmed -notmatch '^[A-Za-z]:[\\/]' -and -not $trimmed.StartsWith('\\')) { return '' }
  try { $full = [IO.Path]::GetFullPath($trimmed) } catch { $full = $trimmed }
  return $full.Replace('/', '\').ToLowerInvariant()
}
function Extract-PathTokens([string]$text) {
  $tokens = New-Object System.Collections.Generic.List[string]
  if ([string]::IsNullOrWhiteSpace($text)) { return $tokens }
  foreach ($m in [regex]::Matches($text, '"([^"]+)"|''([^'']+)''|(\S+)')) {
    $val = if ($m.Groups[1].Success) { $m.Groups[1].Value } elseif ($m.Groups[2].Success) { $m.Groups[2].Value } else { $m.Groups[3].Value }
    if ($val) { $tokens.Add($val) }
  }
  return $tokens
}
function Matches-RootPrefix([string]$candidate, [string[]]$roots, [bool]$requireExecutableExt) {
  $norm = Normalize-Candidate $candidate
  if (-not $norm) { return $false }
  foreach ($root in $roots) {
    if ($norm.StartsWith($root + '\')) {
      if (-not $requireExecutableExt) { return $true }
      $ext = [IO.Path]::GetExtension($norm)
      if ($ext -and $script:ExecutableExts -contains $ext) { return $true }
    }
  }
  return $false
}
# A path under one of the roots ending in an executable extension anywhere in the text, also inside a quoted command
# string (powershell -Command "... C:\Rainy\x.ps1", cmd /c "..."), where whole-token matching misses it.
function Contains-RootExecutable([string]$text, [string[]]$roots) {
  if ([string]::IsNullOrWhiteSpace($text)) { return $false }
  $lower = $text.Replace('/', '\').ToLowerInvariant()
  $exts = ($script:ExecutableExts | ForEach-Object { [regex]::Escape($_) }) -join '|'
  foreach ($root in $roots) {
    if ([regex]::IsMatch($lower, [regex]::Escape($root + '\') + '[^"''<>|\s]*?(' + $exts + ')(?![\w.])')) { return $true }
  }
  return $false
}
function Find-Busy([string[]]$roots) {
  foreach ($row in Get-CimInstance Win32_Process) {
    if ($row.ProcessId -eq $PID -or $row.ProcessId -eq $InstallerPid) { continue }
    $program = Normalize-Candidate ([string]$row.ExecutablePath)
    if ($program) {
      foreach ($root in $roots) { if ($program.StartsWith($root + '\')) { return $row } }
    }
    $raw = [string]$row.CommandLine
    $tokens = Extract-PathTokens $raw
    if ($tokens.Count -gt 0 -and (Matches-RootPrefix $tokens[0] $roots $false)) { return $row }
    if (Contains-RootExecutable $raw $roots) { return $row }
    if ($raw -match '-(?i:EncodedCommand|enc|ec)\s+([A-Za-z0-9+/=]+)') {
      try { $decoded = [Text.Encoding]::Unicode.GetString([Convert]::FromBase64String($Matches[1])) } catch { $decoded = '' }
      $decoded = $decoded.Replace(([string][char]39 + [char]39), [string][char]39)
      if (Contains-RootExecutable $decoded $roots) { return $row }
    }
  }
  return $null
}
try {
  $roots = @($InstallRoot, $PreviousUserRoot, $PreviousMachineRoot) | Where-Object { $_ } |
    ForEach-Object { Normalize-Dir $_ } | Where-Object { $_ } | Select-Object -Unique
  $deadline = [DateTime]::UtcNow.AddSeconds($WaitSeconds)
  while ($true) {
    $busy = Find-Busy $roots
    if ($null -eq $busy) { exit 0 }
    if ([DateTime]::UtcNow -ge $deadline) { Write-Output ('Busy: ' + $busy.ProcessId + ' ' + $busy.Name); exit 2 }
    Start-Sleep -Milliseconds 500
  }
} catch { Write-Output ('Process check failed: ' + $_.Exception.Message); exit 1 }
