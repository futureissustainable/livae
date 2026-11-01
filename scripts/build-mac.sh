#!/bin/bash
# Build Livae on the Mac and install it to ~/Applications/Livae.app (menu-bar app, no Dock icon).
# Build output stays outside the synced Code folder.
set -euo pipefail
cd "$(dirname "$0")/.."
export CARGO_TARGET_DIR="$HOME/.cache/cargo-target/livae"
export PATH="$HOME/.cargo/bin:$PATH"
command -v cargo >/dev/null || { echo "Rust is missing: install it with https://rustup.rs (user-level, no sudo)"; exit 1; }
cargo build --release -p livae-mac
APP="$HOME/Applications/Livae.app"
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
rm -rf "$APP.new"
mkdir -p "$APP.new/Contents/MacOS" "$APP.new/Contents/Resources"
cp assets/Livae.icns "$APP.new/Contents/Resources/Livae.icns"
cp "$CARGO_TARGET_DIR/release/livae" "$APP.new/Contents/MacOS/Livae"
cat > "$APP.new/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleName</key><string>Livae</string>
  <key>CFBundleIdentifier</key><string>com.mateus.livae</string>
  <key>CFBundleExecutable</key><string>Livae</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleIconFile</key><string>Livae</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>CFBundleVersion</key><string>$VERSION</string>
  <key>LSMinimumSystemVersion</key><string>15.0</string>
  <key>NSHumanReadableCopyright</key><string>Livae. App discovery ideas from RustCast (MIT).</string>
  <key>LSUIElement</key><true/>
  <key>NSHighResolutionCapable</key><true/>
</dict></plist>
PLIST
# Sign with the local "Livae Local Signing" certificate (its own keychain), so macOS keeps the Accessibility
# permission across rebuilds. Without it, fall back to an ad-hoc signature (permission must be granted again).
KC="$HOME/Library/Keychains/livae-signing.keychain-db"
if [ -f "$KC" ] && [ -f "$HOME/.config/livae/keychain-pass" ]; then
  security unlock-keychain -p "$(cat "$HOME/.config/livae/keychain-pass")" "$KC"
  codesign --force --sign "Livae Local Signing" "$APP.new" >/dev/null
else
  codesign --force --sign - "$APP.new" >/dev/null
fi
# Stop only the everyday copy (no arguments; --test copies keep running), swap the bundle, then start it again.
MAIN=$(ps -eo pid=,args= | awk -v exe="$APP/Contents/MacOS/Livae" '$2 == exe && NF == 2 {print $1}')
[ -n "$MAIN" ] && kill $MAIN && for _ in $(seq 50); do kill -0 $MAIN 2>/dev/null || break; sleep 0.1; done
rm -rf "$APP" && mv "$APP.new" "$APP"
[ -n "$MAIN" ] && open -g "$APP"
echo "installed $APP${MAIN:+ (restarted)}"
