#!/usr/bin/env bash
# Restart the debug interface with the scratch profile in tmp/ui-data and record its pid in tmp/ui.pid.
set -euo pipefail
root="$(cd "$(dirname "$0")/../.." && pwd)"
if [ -f "$root/tmp/ui.pid" ]; then taskkill //PID "$(cat "$root/tmp/ui.pid")" //F > /dev/null 2>&1 || true; fi
mkdir -p "$root/tmp/ui-data"
# Tests never touch the real ~/.rainy-agent inside WSL.
export RAINY_DEV_WSL_HOME=/tmp/rainy-gpui-home MSYS2_ENV_CONV_EXCL=RAINY_DEV_WSL_HOME
# The window opens behind the others and leaves the focus where it is (RAINY_QA_FOREGROUND=1 to bring it up).
[ -n "${RAINY_QA_FOREGROUND:-}" ] || export RAINY_QA_BACKGROUND=1
"$root/native/target/debug/Rainy.exe" --user-data-dir="$(cygpath -w "$root/tmp/ui-data")" > "$root/tmp/ui.log" 2>&1 &
echo $! > "$root/tmp/ui.pid.bash"
# The Windows pid of the started process, for the PowerShell drivers.
for _ in $(seq 1 40); do
  pid=$(powershell -NoProfile -Command "(Get-CimInstance Win32_Process -Filter \"Name='Rainy.exe'\" | Where-Object { \$_.CommandLine -like '*ui-data*' } | Select-Object -First 1).ProcessId")
  if [ -n "$pid" ]; then echo "$pid" > "$root/tmp/ui.pid"; echo "pid $pid"; exit 0; fi
  sleep 0.5
done
echo "Rainy did not start"; exit 1
