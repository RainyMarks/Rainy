# Whether a process still runs from the Rainy installation (3.x customCheckAppRunning, with a wait): its program lies in
# one of the roots, or its command line, with -EncodedCommand decoded, names a path in one. Nothing is ever ended.
# Exit 0 when none does, 2 when one still does after WaitSeconds (printed as "Busy: <pid> <name>"), 1 when the check failed.
param([string]$InstallRoot, [string]$PreviousUserRoot, [string]$PreviousMachineRoot, [int]$InstallerPid, [int]$WaitSeconds = 0)
$ErrorActionPreference = 'Stop'
$env:PSModulePath = [IO.Path]::Combine($PSHOME, 'Modules')
function Normalize([string]$value) { return $value.Replace('/', '\').ToLowerInvariant() }
function Find-Busy([string[]]$roots) {
  foreach ($row in Get-CimInstance Win32_Process) {
    if ($row.ProcessId -eq $PID -or $row.ProcessId -eq $InstallerPid) { continue }
    $raw = [string]$row.CommandLine; $decoded = ''
    if ($raw -match '-(?i:EncodedCommand|enc|ec)\s+([A-Za-z0-9+/=]+)') { try { $decoded = [Text.Encoding]::Unicode.GetString([Convert]::FromBase64String($Matches[1])) } catch { $decoded = '' } }
    $command = Normalize ($raw + $decoded.Replace(([string][char]39 + [char]39), [string][char]39)); $program = Normalize ([string]$row.ExecutablePath)
    foreach ($root in $roots) { if ($program.StartsWith($root + '\') -or $command.Contains($root + '\')) { return $row } }
  }
  return $null
}
try {
  $roots = @($InstallRoot, $PreviousUserRoot, $PreviousMachineRoot) | Where-Object { $_ } |
    ForEach-Object { Normalize ([IO.Path]::GetFullPath($_).TrimEnd([char]92)) } | Select-Object -Unique
  $deadline = [DateTime]::UtcNow.AddSeconds($WaitSeconds)
  while ($true) {
    $busy = Find-Busy $roots
    if ($null -eq $busy) { exit 0 }
    if ([DateTime]::UtcNow -ge $deadline) { Write-Output ('Busy: ' + $busy.ProcessId + ' ' + $busy.Name); exit 2 }
    Start-Sleep -Milliseconds 500
  }
} catch { Write-Output ('Process check failed: ' + $_.Exception.Message); exit 1 }
