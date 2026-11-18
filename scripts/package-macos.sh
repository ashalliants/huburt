#!/bin/zsh
# Builds a downloadable Huburt.app for macOS and zips it into dist/.
#
# Unlike ./build.sh (the local development build, which keeps the core library in
# ~/Library/Application Support/Huburt/ so rebuilds don't change the signed app), this puts the core library INSIDE the app
# (Contents/Frameworks), so the zip works on any Mac of the same architecture.
#
# Signing: ad-hoc by default (no Apple Developer account needed). macOS Gatekeeper then asks people to approve the app once
# (see the README). Set SIGN_IDENTITY to a "Developer ID Application: ..." identity to sign for real; notarising is a separate
# step (xcrun notarytool) not done here.
set -e
cd "$(dirname "$0")/.."

VERSION=$(jq -r .version src-tauri/tauri.conf.json)
ARCH=$(uname -m); [ "$ARCH" = "x86_64" ] && ARCH=x64
SIGN=${SIGN_IDENTITY:--}
BUILT_APP=src-tauri/target/release/bundle/macos/Huburt.app   # the development bundle: left untouched
APP=dist/stage/Huburt.app                                       # the copy that gets the core inside and is signed
CORE=src-tauri/target/release/libhuburt_core.dylib
OUT=dist/Huburt_${VERSION}_macos-${ARCH}.zip

if command -v cargo-tauri >/dev/null 2>&1; then TAURI=(cargo tauri); else TAURI=(npx --yes @tauri-apps/cli@2); fi

"${TAURI[@]}" build --bundles app                                          # the launcher and the app bundle
(cd src-tauri && cargo build --release --lib --features custom-protocol)   # the real app, as a library

# Work on a copy so the development bundle (and the signature ./build.sh gave it) isn't disturbed
rm -rf dist/stage && mkdir -p dist/stage
ditto "$BUILT_APP" "$APP"

# Put the core inside the bundle, then sign inside-out (library first, then the app that contains it)
mkdir -p "$APP/Contents/Frameworks"
cp "$CORE" "$APP/Contents/Frameworks/libhuburt_core.dylib"
codesign --force --sign "$SIGN" "$APP/Contents/Frameworks/libhuburt_core.dylib"
codesign --force --sign "$SIGN" "$APP"
codesign --verify --deep --strict "$APP"

mkdir -p dist
rm -f "$OUT"
ditto -c -k --keepParent "$APP" "$OUT"
rm -rf dist/stage
echo "Built $OUT ($(du -h "$OUT" | cut -f1))"
(cd dist && shasum -a 256 "$(basename "$OUT")")
