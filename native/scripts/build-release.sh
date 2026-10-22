#!/usr/bin/env bash
# Build the optimised Rainy.exe in native/target/release. GPUI precompiles its shaders with fxc.exe in release
# builds; without a Windows SDK, scripts/fxc.cs stands in for it, built with the C# compiler every Windows has.
set -euo pipefail
native="$(cd "$(dirname "$0")/.." && pwd)"
export PATH="$HOME/.cargo/bin:/c/msys64/ucrt64/bin:$PATH"
fxc="$native/target/tools/fxc.exe"
if [ ! -f "$fxc" ] || [ "$native/scripts/fxc.cs" -nt "$fxc" ]; then
  mkdir -p "$native/target/tools"
  MSYS2_ARG_CONV_EXCL='*' /c/Windows/Microsoft.NET/Framework64/v4.0.30319/csc.exe -nologo -optimize -platform:x64 \
    "-out:$(cygpath -w "$fxc")" "$(cygpath -w "$native/scripts/fxc.cs")"
fi
export GPUI_FXC_PATH="$(cygpath -w "$fxc")"
cd "$native"
cargo build -p rainy --release --offline "$@"
