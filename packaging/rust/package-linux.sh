#!/usr/bin/env bash
set -euo pipefail

: "${RELEASE_VERSION:?RELEASE_VERSION is required}"
: "${ARCH_SUFFIX:?ARCH_SUFFIX is required}"
: "${DEB_ARCH:?DEB_ARCH is required}"
: "${RPM_ARCH:?RPM_ARCH is required}"
: "${MUSL_TARGET:?MUSL_TARGET is required}"

PROJECT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
DIST_DIR="$PROJECT_DIR/dist"
GNU_CLI="$PROJECT_DIR/target/release/tg-ws-proxy"
GNU_DESKTOP="$PROJECT_DIR/target/release/tg-ws-proxy-desktop"
MUSL_CLI="$PROJECT_DIR/target/$MUSL_TARGET/release/tg-ws-proxy"

for binary in "$GNU_CLI" "$GNU_DESKTOP" "$MUSL_CLI"; do
    if [[ ! -x "$binary" ]]; then
        echo "Missing release binary: $binary" >&2
        exit 1
    fi
done

mkdir -p "$DIST_DIR"

DESKTOP_ASSET="$DIST_DIR/TgWsProxy_linux_$ARCH_SUFFIX"
CLI_ASSET="$DIST_DIR/tg-ws-proxy_cli_linux_$ARCH_SUFFIX"
MUSL_ASSET="$DIST_DIR/tg-ws-proxy_cli_linux_${ARCH_SUFFIX}_musl"

install -m 755 "$GNU_DESKTOP" "$DESKTOP_ASSET"
install -m 755 "$GNU_CLI" "$CLI_ASSET"
install -m 755 "$MUSL_CLI" "$MUSL_ASSET"

SCRATCH_BASE="${RUNNER_TEMP:-$PROJECT_DIR/.scratch}"
mkdir -p "$SCRATCH_BASE"
WORK_DIR="$(mktemp -d "$SCRATCH_BASE/tg-ws-proxy-linux.XXXXXX")"
trap 'rm -rf "$WORK_DIR"' EXIT

ARCHIVE_ROOT="$WORK_DIR/tg-ws-proxy-$RELEASE_VERSION-linux-$ARCH_SUFFIX"
mkdir -p "$ARCHIVE_ROOT"
install -m 755 "$GNU_CLI" "$ARCHIVE_ROOT/tg-ws-proxy"
install -m 755 "$GNU_DESKTOP" "$ARCHIVE_ROOT/tg-ws-proxy-desktop"
install -m 755 "$MUSL_CLI" "$ARCHIVE_ROOT/tg-ws-proxy-musl"
install -m 644 "$PROJECT_DIR/LICENSE" "$ARCHIVE_ROOT/LICENSE"
install -m 644 "$PROJECT_DIR/packaging/rust/README.md" "$ARCHIVE_ROOT/README.md"
install -m 644 "$PROJECT_DIR/docs/RUST_PORT.md" "$ARCHIVE_ROOT/RUST_PORT.md"

tar -C "$WORK_DIR" -czf \
    "$DIST_DIR/TgWsProxy_linux_$ARCH_SUFFIX.tar.gz" \
    "$(basename "$ARCHIVE_ROOT")"

PACKAGE_ROOT="$WORK_DIR/package-root"
mkdir -p \
    "$PACKAGE_ROOT/usr/bin" \
    "$PACKAGE_ROOT/usr/share/applications" \
    "$PACKAGE_ROOT/usr/share/icons/hicolor/256x256/apps" \
    "$PACKAGE_ROOT/usr/share/doc/tg-ws-proxy"

install -m 755 "$GNU_CLI" "$PACKAGE_ROOT/usr/bin/tg-ws-proxy"
install -m 755 "$GNU_DESKTOP" "$PACKAGE_ROOT/usr/bin/tg-ws-proxy-desktop"
install -m 644 "$PROJECT_DIR/LICENSE" \
    "$PACKAGE_ROOT/usr/share/doc/tg-ws-proxy/LICENSE"
install -m 644 "$PROJECT_DIR/packaging/rust/README.md" \
    "$PACKAGE_ROOT/usr/share/doc/tg-ws-proxy/README.md"
install -m 644 "$PROJECT_DIR/docs/RUST_PORT.md" \
    "$PACKAGE_ROOT/usr/share/doc/tg-ws-proxy/RUST_PORT.md"
install -m 644 "$PROJECT_DIR/assets/generated/icon-256.png" \
    "$PACKAGE_ROOT/usr/share/icons/hicolor/256x256/apps/tg-ws-proxy.png"

cat >"$PACKAGE_ROOT/usr/share/applications/tg-ws-proxy.desktop" <<'EOF'
[Desktop Entry]
Type=Application
Name=TG WS Proxy
GenericName=Telegram Proxy
Comment=Telegram Desktop WebSocket bridge proxy
Exec=tg-ws-proxy-desktop
Terminal=false
Categories=Network;
StartupNotify=false
Icon=tg-ws-proxy
Keywords=telegram;proxy;websocket;
EOF

DEB_ROOT="$WORK_DIR/deb-root"
cp -a "$PACKAGE_ROOT" "$DEB_ROOT"
mkdir -p "$DEB_ROOT/DEBIAN"
DEB_VERSION="${RELEASE_VERSION%%-*}"
if [[ "$RELEASE_VERSION" == *-* ]]; then
    DEB_VERSION+="~${RELEASE_VERSION#*-}"
fi

cat >"$DEB_ROOT/DEBIAN/control" <<EOF
Package: tg-ws-proxy
Version: $DEB_VERSION
Section: net
Priority: optional
Architecture: $DEB_ARCH
Maintainer: danusha2345
Depends: libc6 (>= 2.31), libgcc-s1
Homepage: https://github.com/danusha2345/tg-ws-proxy
Description: Telegram Desktop WebSocket bridge proxy
 Rust implementation of a local MTProto proxy with CLI and tray frontends.
EOF

dpkg-deb --build --root-owner-group \
    "$DEB_ROOT" \
    "$DIST_DIR/TgWsProxy_linux_$ARCH_SUFFIX.deb"

if ! command -v fpm >/dev/null 2>&1; then
    echo "fpm is required to build the RPM package" >&2
    exit 1
fi

RPM_VERSION="${RELEASE_VERSION%%-*}"
RPM_ITERATION="1"
if [[ "$RELEASE_VERSION" == *-* ]]; then
    RPM_ITERATION="0.${RELEASE_VERSION#*-}"
    RPM_ITERATION="${RPM_ITERATION//-/.}"
fi

fpm \
    --input-type dir \
    --output-type rpm \
    --name tg-ws-proxy \
    --version "$RPM_VERSION" \
    --iteration "$RPM_ITERATION" \
    --architecture "$RPM_ARCH" \
    --license MIT \
    --vendor danusha2345 \
    --maintainer danusha2345 \
    --url https://github.com/danusha2345/tg-ws-proxy \
    --description "Rust MTProto/WebSocket bridge proxy for Telegram Desktop" \
    --depends "glibc >= 2.31" \
    --depends libgcc \
    --package "$DIST_DIR/TgWsProxy_linux_$ARCH_SUFFIX.rpm" \
    --chdir "$PACKAGE_ROOT" \
    .
