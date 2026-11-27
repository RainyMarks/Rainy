#!/usr/bin/env bash
# Build the optimised Rainy.exe in native/target/release. GPUI precompiles its shaders with fxc.exe in release
# builds; without a Windows SDK, scripts/fxc.cs stands in for it, built with the C# compiler every Windows has.
# RAINY_PROFILE=qa builds the faster QA profile into native/target/qa instead (never for a published package).
set -euo pipefail
profile="${RAINY_PROFILE:-release}"
native="$(cd "$(dirname "$0")/.." && pwd)"
msys2="${RAINY_MSYS2:-/c/msys64/ucrt64/bin}"
export PATH="$HOME/.cargo/bin:$msys2:$PATH"
win_root="$(cygpath -u "${SYSTEMROOT:-${WINDIR:-C:\\Windows}}")"
csc="$win_root/Microsoft.NET/Framework64/v4.0.30319/csc.exe"
fxc="$native/target/tools/fxc.exe"
if [ ! -f "$fxc" ] || [ "$native/scripts/fxc.cs" -nt "$fxc" ]; then
  mkdir -p "$native/target/tools"
  MSYS2_ARG_CONV_EXCL='*' "$csc" -nologo -optimize -platform:x64 \
    "-out:$(cygpath -w "$fxc")" "$(cygpath -w "$native/scripts/fxc.cs")"
fi
export GPUI_FXC_PATH="$(cygpath -w "$fxc")"
cd "$native"
extra=()
[ "${RAINY_CARGO_OFFLINE:-0}" = "1" ] && extra+=(--offline)
cargo build --locked -p rainy --profile "$profile" "${extra[@]}" "$@"
