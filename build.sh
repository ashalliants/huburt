#!/bin/zsh
# Builds and signs Huburt.
# Huburt is split in two so macOS keychain approvals survive rebuilds (same trick as the Swift Hub):
#   - Huburt.app: a tiny launcher (src-tauri/src/main.rs) that never changes, so its identity and approvals stay put.
#   - libhuburt_core.dylib: the real app (src-tauri/src/lib.rs), loaded by the launcher from
#     ~/Library/Application Support/Huburt/. Every build installs it there; the app bundle is left alone when the
#     launcher's hash hasn't changed.
# Usage: ./build.sh            builds + signs, installs the core library, bundle stays in src-tauri/target/...
#        ./build.sh --install  also copies Huburt.app to /Applications/Huburt.app (never touches /Applications/Hub.app)
set -e
cd "$(dirname "$0")"
cargo tauri build --bundles app                      # the launcher + app bundle
(cd src-tauri && cargo build --release --lib --features custom-protocol)   # the real app
APP=src-tauri/target/release/bundle/macos/Huburt.app
CORE=src-tauri/target/release/libhuburt_core.dylib
SUPPORT="$HOME/Library/Application Support/Huburt"
CORE_NAME=libhuburt_core.dylib

# Sign with the same stable local identity as the Swift app
IDENT="Hub Local Signing"
if security find-identity -v -p codesigning | grep -q "$IDENT"; then SIGN="$IDENT"; else
  echo "Note: '$IDENT' certificate not found; using ad-hoc signing (macOS will treat every build as a new app)"; SIGN="-"
fi
cdhash() { codesign -dvvv "$1" 2>&1 | sed -n 's/^CDHash=//p'; }
codesign --force --sign "$SIGN" "$CORE"
codesign --force --sign "$SIGN" "$APP"
echo "Built $PWD/$APP ($(du -sh "$APP" | cut -f1)), launcher hash $(cdhash "$APP")"

# The real code: copy to a temp name first so a half-written file is never loaded
mkdir -p "$SUPPORT"
cp "$CORE" "$SUPPORT/.$CORE_NAME.new" && mv -f "$SUPPORT/.$CORE_NAME.new" "$SUPPORT/$CORE_NAME"
echo "Updated $SUPPORT/$CORE_NAME"

if [ "$1" = "--install" ]; then
  DEST=/Applications/Huburt.app
  if pgrep -x huburt >/dev/null; then echo "Huburt is running: quit it from its menu bar icon first, then re-run."; exit 1; fi
  if [ -d "$DEST" ] && [ "$(cdhash "$DEST")" = "$(cdhash "$APP")" ]; then
    echo "Launcher unchanged: $DEST left as is (keychain approval kept)"
  else
    rm -rf "$DEST" && cp -R "$APP" "$DEST"
    echo "Installed $DEST (the launcher changed, so macOS may ask to allow WhatsApp's key once)"
  fi
fi
