#!/usr/bin/env bash
set -e

FLAGS=$(cat /proc/cpuinfo | grep flags | head -n 1)

if [ "$1" = "--native" ]; then
    TARGET_LEVEL="native"
    shift
else
    FLAGS=$(cat /proc/cpuinfo | grep flags | head -n 1)

    if echo "$FLAGS" | grep -q "avx512f" && echo "$FLAGS" | grep -q "avx512vl" && echo "$FLAGS" | grep -q "avx512bw"; then
        TARGET_LEVEL="x86-64-v4"
    elif echo "$FLAGS" | grep -q "avx2" && echo "$FLAGS" | grep -q "bmi2" && echo "$FLAGS" | grep -q "fma"; then
        TARGET_LEVEL="x86-64-v3"
    elif echo "$FLAGS" | grep -q "popcnt" && echo "$FLAGS" | grep -q "sse4_2"; then
        TARGET_LEVEL="x86-64-v2"
    else
        TARGET_LEVEL="x86-64"
    fi
fi

echo "Target Architecture Level Detected: $TARGET_LEVEL"

FLAGS_ARRAY=(
  "-C" "target-cpu=$TARGET_LEVEL"
  "-C" "link-arg=-Wl,-z,now"
  "-Z" "tls-model=initial-exec"
  "-C" "force-unwind-tables=no"
  "-C" "llvm-args=-align-all-functions=5"
  "-C" "llvm-args=-x86-pad-for-align=false"
  "-C" "llvm-args=--inline-threshold=275"
  "-C" "code-model=small"
)

export RUSTFLAGS="${FLAGS_ARRAY[*]}"

if [ $# -eq 0 ]; then
    echo "Running release build..."
    cargo build --release
else
    echo "Running: cargo $*"
    cargo "$@"
fi
