#!/usr/bin/env bash
# Build the optimised Rust Host for both execution targets: native/target/release/rainy-host.exe (Windows) and
# native/target/x86_64-unknown-linux-gnu/release/rainy-host (WSL and SSH servers, glibc 2.17 or later), the second with
# cargo-zigbuild and Zig (RAINY_ZIG names its folder; default ~/.rainy-release/tools/zig/ziglang).
# RAINY_PROFILE=qa builds the faster QA profile into target/qa and target/x86_64-unknown-linux-gnu/qa instead.
set -euo pipefail
native="$(cd "$(dirname "$0")/.." && pwd)"
zig="${RAINY_ZIG:-$HOME/.rainy-release/tools/zig/ziglang}"
msys2="${RAINY_MSYS2:-/c/msys64/ucrt64/bin}"
profile="${RAINY_PROFILE:-release}"
export PATH="$zig:$HOME/.cargo/bin:$msys2:$PATH"
if ! command -v zig >/dev/null 2>&1; then
  echo "zig not found on PATH (checked RAINY_ZIG=$zig)" >&2
  exit 1
fi
if ! command -v cargo-zigbuild >/dev/null 2>&1; then
  echo "cargo-zigbuild not found on PATH" >&2
  exit 1
fi
cd "$native"
cargo build --locked -p rainy-host --profile "$profile" "$@"
cargo zigbuild --locked -p rainy-host --profile "$profile" --target x86_64-unknown-linux-gnu.2.17 "$@"
ls -l "target/$profile/rainy-host.exe" "target/x86_64-unknown-linux-gnu/$profile/rainy-host"
