#!/bin/sh
# Builds the iOS app without a signing identity.
#
#   ./build.sh device     unsigned IPA in dist/, for AltStore, SideStore,
#                         Sideloadly, TrollStore and other signers
#   ./build.sh simulator  .app for the iOS Simulator
#
# Signing is intentionally left to the tool that installs the build: a free
# Apple ID cannot sign here, and the paid entitlements this app would need do
# not exist, because it keeps itself alive with background audio instead of a
# Network Extension.
set -eu

ROOT="$(cd "$(dirname "$0")" && pwd)"
cd "$ROOT"

PLATFORM="${1:-device}"
CONFIGURATION="${CONFIGURATION:-Release}"
DERIVED="$ROOT/.build/$PLATFORM"

case "$PLATFORM" in
  device)
    SDK="iphoneos"
    DESTINATION="generic/platform=iOS"
    ;;
  simulator)
    SDK="iphonesimulator"
    DESTINATION="generic/platform=iOS Simulator"
    ;;
  *)
    echo "usage: $0 [device|simulator]" >&2
    exit 2
    ;;
esac

if ! command -v xcodebuild >/dev/null 2>&1; then
  echo "error: xcodebuild was not found. Install the full Xcode." >&2
  exit 1
fi

xcodebuild \
  -project TgWsProxy.xcodeproj \
  -scheme TgWsProxy \
  -configuration "$CONFIGURATION" \
  -sdk "$SDK" \
  -destination "$DESTINATION" \
  -derivedDataPath "$DERIVED" \
  CODE_SIGNING_ALLOWED=NO \
  CODE_SIGNING_REQUIRED=NO \
  CODE_SIGN_IDENTITY="" \
  build

APP="$DERIVED/Build/Products/$CONFIGURATION-$SDK/TG WS Proxy.app"
[ -d "$APP" ] || { echo "error: $APP was not produced" >&2; exit 1; }

if [ "$PLATFORM" = "simulator" ]; then
  echo "Built for the simulator: $APP"
  echo "Install with: xcrun simctl install booted \"$APP\""
  exit 0
fi

STAGE="$ROOT/.build/payload"
rm -rf "$STAGE"
mkdir -p "$STAGE/Payload"
cp -R "$APP" "$STAGE/Payload/"

mkdir -p "$ROOT/dist"
IPA="$ROOT/dist/TgWsProxy-unsigned.ipa"
rm -f "$IPA"
(cd "$STAGE" && zip -q -r "$IPA" Payload)
rm -rf "$STAGE"

echo "Built unsigned IPA: $IPA"
