#!/usr/bin/env bash
# Cross-compile libcastify.so.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
OUT="${1:-$ROOT/../android-csv-tv/app/src/main/jniLibs}"
NDK="${ANDROID_NDK_HOME:-${ANDROID_HOME:+$ANDROID_HOME/ndk/27.0.12077973}}"
if [[ -z "${NDK}" || ! -d "${NDK}" ]]; then
  echo "Set ANDROID_NDK_HOME to an installed NDK (sdkmanager \"ndk;27.0.12077973\")."
  exit 1
fi
export ANDROID_NDK_HOME="$NDK"
export ANDROID_NDK_ROOT="$NDK"
CARGO="${CARGO:-$HOME/.cargo/bin/cargo}"
API=21
HOST="$(uname -s | tr '[:upper:]' '[:lower:]')-x86_64"
TOOLCHAIN="$NDK/toolchains/llvm/prebuilt/$HOST/bin"
if [[ ! -d "$TOOLCHAIN" ]]; then
  HOST="$(uname -s | tr '[:upper:]' '[:lower:]')-arm64"
  TOOLCHAIN="$NDK/toolchains/llvm/prebuilt/$HOST/bin"
fi

build_abi() {
  local rust_target="$1" abi="$2" prefix="$3"
  local linker="$TOOLCHAIN/${prefix}${API}-clang"
  rustup target add "$rust_target" >/dev/null
  echo "building $abi"
  env -u CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER \
     -u CARGO_TARGET_ARMV7_LINUX_ANDROIDEABI_LINKER \
     -u CARGO_TARGET_X86_64_LINUX_ANDROID_LINKER \
     "CARGO_TARGET_$(echo "$rust_target" | tr 'a-z-' 'A-Z_')_LINKER=$linker" \
     CC="$linker" AR="$TOOLCHAIN/llvm-ar" \
     CARGO_TARGET_DIR="$ROOT/target" \
     "$CARGO" build -p castify-ffi --release --target "$rust_target"
  mkdir -p "$OUT/$abi"
  cp "$ROOT/target/$rust_target/release/libcastify.so" "$OUT/$abi/"
}

build_abi aarch64-linux-android arm64-v8a aarch64-linux-android
build_abi armv7-linux-androideabi armeabi-v7a armv7a-linux-androideabi
build_abi x86_64-linux-android x86_64 x86_64-linux-android
echo "jniLibs -> $OUT"
