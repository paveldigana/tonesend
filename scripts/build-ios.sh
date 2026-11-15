#!/bin/bash
# Build the Rust modem as a static library for iOS.
# Called by Xcode before compiling (uses $PLATFORM_NAME), or by hand to build both.
set -euo pipefail
export PATH="$HOME/.cargo/bin:/opt/homebrew/opt/rustup/bin:/opt/homebrew/bin:$PATH"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

# Xcode exports SDK variables that confuse cargo's host build; drop them.
unset SDKROOT MACOSX_DEPLOYMENT_TARGET LIBRARY_PATH
# Build Rust *and* bundled C code (libwebp) for the app's minimum iOS version,
# otherwise clang defaults to the newest SDK and the linker warns/fails on older iOS.
# Keep in sync with deploymentTarget in ios/project.yml.
export IPHONEOS_DEPLOYMENT_TARGET=17.0

case "${PLATFORM_NAME:-all}" in
  iphoneos)        targets=(aarch64-apple-ios) ;;
  iphonesimulator) targets=(aarch64-apple-ios-sim) ;;
  *)               targets=(aarch64-apple-ios aarch64-apple-ios-sim) ;;
esac

for t in "${targets[@]}"; do
  echo "building tonesend for $t"
  cargo build --release -p tonesend --target "$t"
done
