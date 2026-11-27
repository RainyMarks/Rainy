#!/usr/bin/env bash
# Restart the debug interface with the scratch profile in tmp/ui-data and record its pid in tmp/ui.pid.
set -euo pipefail
root="$(cd "$(dirname "$0")/../.." && pwd)"
exe="$root/native/target/debug/Rainy.exe"
if [ ! -f "$exe" ]; then
  echo "Rainy.exe is missing ($exe); run: cd native && cargo build -p rainy -p rainy-host" >&2
  exit 1
fi
ui_data_win="$(cygpath -w "$root/tmp/ui-data")"
if [ -f "$root/tmp/ui.pid" ]; then
  old_pid="$(tr -d '[:space:]' < "$root/tmp/ui.pid")"
  if [[ "$old_pid" =~ ^[0-9]+$ ]]; then
    # Verify image name and command line before stopping so a reused PID is never killed by mistake.
    powershell -NoProfile -NonInteractive -Command "
      \$p = Get-CimInstance Win32_Process -Filter \"ProcessId=$old_pid AND Name='Rainy.exe'\" -ErrorAction SilentlyContinue
      if (\$p -and \$p.CommandLine -and \$p.CommandLine.IndexOf(\$args[0], [StringComparison]::OrdinalIgnoreCase) -ge 0) {
        Stop-Process -Id $old_pid -Force -ErrorAction SilentlyContinue
      }
    " "$ui_data_win" > /dev/null 2>&1 || true
  fi
fi
mkdir -p "$root/tmp/ui-data"
# Tests never touch the real ~/.rainy-agent inside WSL.
export RAINY_DEV_WSL_HOME=/tmp/rainy-gpui-home MSYS2_ENV_CONV_EXCL=RAINY_DEV_WSL_HOME
# The window opens behind the others and leaves the focus where it is (RAINY_QA_FOREGROUND=1 to bring it up).
[ -n "${RAINY_QA_FOREGROUND:-}" ] || export RAINY_QA_BACKGROUND=1
"$exe" --user-data-dir="$ui_data_win" > "$root/tmp/ui.log" 2>&1 &
# Find the Windows PID matching this worktree's exact ui-data directory in a single PowerShell loop.
pid="$(powershell -NoProfile -NonInteractive -Command '
  $target = $args[0]
  for ($i = 0; $i -lt 40; $i++) {
    $proc = Get-CimInstance Win32_Process -Filter "Name=''Rainy.exe''" -ErrorAction SilentlyContinue |
      Where-Object { $_.CommandLine -and $_.CommandLine.IndexOf($target, [StringComparison]::OrdinalIgnoreCase) -ge 0 } |
      Select-Object -First 1
    if ($proc) { Write-Output $proc.ProcessId; exit 0 }
    Start-Sleep -Milliseconds 500
  }
' "$ui_data_win" | tr -d '\r\n')"
if [ -n "$pid" ]; then
  echo "$pid" > "$root/tmp/ui.pid"
  echo "pid $pid"
  exit 0
fi
echo "Rainy did not start" >&2
if [ -s "$root/tmp/ui.log" ]; then
  echo "--- tmp/ui.log (tail) ---" >&2
  tail -n 20 "$root/tmp/ui.log" >&2
fi
exit 1
