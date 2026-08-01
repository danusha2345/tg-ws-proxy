#!/usr/bin/env bash
set -euo pipefail

: "${RELEASE_VERSION:?RELEASE_VERSION is required}"

PROJECT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
DIST_DIR="$PROJECT_DIR/dist"
ARM_DIR="$PROJECT_DIR/target/aarch64-apple-darwin/release"
INTEL_DIR="$PROJECT_DIR/target/x86_64-apple-darwin/release"

for binary in \
    "$ARM_DIR/tg-ws-proxy" \
    "$ARM_DIR/tg-ws-proxy-desktop" \
    "$INTEL_DIR/tg-ws-proxy" \
    "$INTEL_DIR/tg-ws-proxy-desktop"
do
    if [[ ! -x "$binary" ]]; then
        echo "Missing release binary: $binary" >&2
        exit 1
    fi
done

mkdir -p "$DIST_DIR"

SCRATCH_BASE="${RUNNER_TEMP:-$PROJECT_DIR/.scratch}"
mkdir -p "$SCRATCH_BASE"
WORK_DIR="$(mktemp -d "$SCRATCH_BASE/tg-ws-proxy-macos.XXXXXX")"
trap 'rm -rf "$WORK_DIR"' EXIT

UNIVERSAL_DIR="$WORK_DIR/universal"
mkdir -p "$UNIVERSAL_DIR"

for binary in tg-ws-proxy tg-ws-proxy-desktop; do
    lipo -create \
        "$ARM_DIR/$binary" \
        "$INTEL_DIR/$binary" \
        -output "$UNIVERSAL_DIR/$binary"
    chmod 755 "$UNIVERSAL_DIR/$binary"

    ARCHES="$(lipo -archs "$UNIVERSAL_DIR/$binary")"
    if [[ "$ARCHES" != *arm64* || "$ARCHES" != *x86_64* ]]; then
        echo "Universal binary is missing an architecture: $binary ($ARCHES)" >&2
        exit 1
    fi

    codesign --force --sign - "$UNIVERSAL_DIR/$binary"
    codesign --verify --strict "$UNIVERSAL_DIR/$binary"
done

APP_PATH="$WORK_DIR/TG WS Proxy.app"
mkdir -p "$APP_PATH/Contents/MacOS" "$APP_PATH/Contents/Resources"
install -m 755 \
    "$UNIVERSAL_DIR/tg-ws-proxy-desktop" \
    "$APP_PATH/Contents/MacOS/TG WS Proxy"
ICONSET="$WORK_DIR/TgWsProxy.iconset"
mkdir -p "$ICONSET"
for size in 16 32 128 256 512; do
    sips -z "$size" "$size" "$PROJECT_DIR/assets/generated/icon-1024.png" \
        --out "$ICONSET/icon_${size}x${size}.png" >/dev/null
    double=$((size * 2))
    sips -z "$double" "$double" "$PROJECT_DIR/assets/generated/icon-1024.png" \
        --out "$ICONSET/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$ICONSET" -o "$APP_PATH/Contents/Resources/TgWsProxy.icns"
BASE_VERSION="${RELEASE_VERSION%%-*}"

cat >"$APP_PATH/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "https://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleDevelopmentRegion</key>
  <string>ru</string>
  <key>CFBundleDisplayName</key>
  <string>TG WS Proxy</string>
  <key>CFBundleExecutable</key>
  <string>TG WS Proxy</string>
  <key>CFBundleIdentifier</key>
  <string>io.github.danusha2345.tg-ws-proxy</string>
  <key>CFBundleIconFile</key>
  <string>TgWsProxy</string>
  <key>CFBundleInfoDictionaryVersion</key>
  <string>6.0</string>
  <key>CFBundleName</key>
  <string>TG WS Proxy</string>
  <key>CFBundlePackageType</key>
  <string>APPL</string>
  <key>CFBundleShortVersionString</key>
  <string>$BASE_VERSION</string>
  <key>CFBundleVersion</key>
  <string>$BASE_VERSION</string>
  <key>LSMinimumSystemVersion</key>
  <string>11.0</string>
  <key>LSUIElement</key>
  <true/>
  <key>NSHighResolutionCapable</key>
  <true/>
  <key>NSPrincipalClass</key>
  <string>NSApplication</string>
</dict>
</plist>
EOF

codesign --force --deep --sign - "$APP_PATH"
codesign --verify --deep --strict "$APP_PATH"

"$PROJECT_DIR/packaging/dmg/build_dmg.sh" \
    "$APP_PATH" \
    "TG WS Proxy" \
    "$DIST_DIR/TgWsProxy_macos_universal.dmg"

ARCHIVE_ROOT="$WORK_DIR/tg-ws-proxy-$RELEASE_VERSION-macos-universal"
mkdir -p "$ARCHIVE_ROOT"
install -m 755 "$UNIVERSAL_DIR/tg-ws-proxy" "$ARCHIVE_ROOT/tg-ws-proxy"
install -m 755 \
    "$UNIVERSAL_DIR/tg-ws-proxy-desktop" \
    "$ARCHIVE_ROOT/tg-ws-proxy-desktop"
install -m 644 "$PROJECT_DIR/LICENSE" "$ARCHIVE_ROOT/LICENSE"
install -m 644 "$PROJECT_DIR/packaging/rust/README.md" "$ARCHIVE_ROOT/README.md"
install -m 644 "$PROJECT_DIR/docs/RUST_PORT.md" "$ARCHIVE_ROOT/RUST_PORT.md"

tar -C "$WORK_DIR" -czf \
    "$DIST_DIR/TgWsProxy_macos_universal.tar.gz" \
    "$(basename "$ARCHIVE_ROOT")"
