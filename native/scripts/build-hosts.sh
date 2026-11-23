#!/usr/bin/env bash
# Build the optimised Rust Host for both execution targets: native/target/release/rainy-host.exe (Windows) and
# native/target/x86_64-unknown-linux-gnu/release/rainy-host (WSL and SSH servers, glibc 2.17 or later), the second with
# cargo-zigbuild and Zig (RAINY_ZIG names its folder; default ~/.rainy-release/tools/zig/ziglang).
# RAINY_PROFILE=qa builds the faster QA profile into target/qa and target/x86_64-unknown-linux-gnu/qa instead.
set -euo pipefail
native="$(cd "$(dirname "$0")/.." && pwd)"
zig="${RAINY_ZIG:-$HOME/.rainy-release/tools/zig/ziglang}"
profile="${RAINY_PROFILE:-release}"
export PATH="$zig:$HOME/.cargo/bin:/c/msys64/ucrt64/bin:$PATH"
cd "$native"
cargo build -p rainy-host --profile "$profile" "$@"
cargo zigbuild -p rainy-host --profile "$profile" --target x86_64-unknown-linux-gnu.2.17 "$@"
ls -l "target/$profile/rainy-host.exe" "target/x86_64-unknown-linux-gnu/$profile/rainy-host"
