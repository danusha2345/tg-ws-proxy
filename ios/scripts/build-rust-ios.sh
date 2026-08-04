#!/bin/sh
# Builds the Rust core as a static library for the architectures Xcode asked
# for and places it where the app target links from. Run automatically by the
# "Build Rust core" phase; can also be run by hand.
set -eu

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
TARGET_DIR="${DERIVED_FILE_DIR:-$ROOT/ios/.build}/rust-target"
OUTPUT_DIR="${BUILT_PRODUCTS_DIR:-$ROOT/ios/.build}/rust"

export PATH="$HOME/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:$PATH"

if ! command -v cargo >/dev/null 2>&1; then
  echo "error: cargo was not found. Install Rust from https://rustup.rs" >&2
  exit 1
fi

export IPHONEOS_DEPLOYMENT_TARGET="${IPHONEOS_DEPLOYMENT_TARGET:-16.0}"

architectures() {
  if [ -n "${ARCHS:-}" ]; then
    printf '%s\n' "$ARCHS"
  else
    printf '%s\n' "arm64"
  fi
}

rust_target() {
  case "${PLATFORM_NAME:-iphoneos}:$1" in
    iphoneos:arm64) printf '%s\n' "aarch64-apple-ios" ;;
    iphonesimulator:arm64) printf '%s\n' "aarch64-apple-ios-sim" ;;
    iphonesimulator:x86_64) printf '%s\n' "x86_64-apple-ios" ;;
    *)
      echo "error: unsupported platform/architecture ${PLATFORM_NAME:-unknown}/$1" >&2
      exit 1
      ;;
  esac
}

mkdir -p "$OUTPUT_DIR"

# The proxy is unusably slow when the core is built without optimisations, so
# release is used for Debug builds of the app as well.
set --
for ARCHITECTURE in $(architectures); do
  RUST_TARGET="$(rust_target "$ARCHITECTURE")"

  if ! rustc --print target-list | grep -qx "$RUST_TARGET"; then
    echo "error: this toolchain does not know $RUST_TARGET" >&2
    exit 1
  fi

  CARGO_TARGET_DIR="$TARGET_DIR" cargo build \
    --manifest-path "$ROOT/Cargo.toml" \
    --package tg-ws-proxy-ios \
    --target "$RUST_TARGET" \
    --release \
    --locked

  LIBRARY="$TARGET_DIR/$RUST_TARGET/release/libtgwsproxy.a"
  if [ ! -f "$LIBRARY" ]; then
    echo "error: cargo finished but $LIBRARY is missing" >&2
    exit 1
  fi
  set -- "$@" "$LIBRARY"
done

if [ "$#" -eq 1 ]; then
  cp "$1" "$OUTPUT_DIR/libtgwsproxy.a"
else
  xcrun lipo -create "$@" -output "$OUTPUT_DIR/libtgwsproxy.a"
fi

echo "Rust core: ${PLATFORM_NAME:-iphoneos} [$(architectures)] -> $OUTPUT_DIR/libtgwsproxy.a"
