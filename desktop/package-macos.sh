#!/usr/bin/env bash
#
# Build a double-clickable macOS .app bundle for Watermark Remover.
#
#   ./package-macos.sh            # builds dist/Watermark Remover.app (+ a .zip)
#
# Uses only tools that ship with macOS (cargo, sips, iconutil, codesign).
# The resulting binary links only system frameworks, so the .app is
# self-contained — no dylibs to bundle.
set -euo pipefail
cd "$(dirname "$0")"

APP_NAME="Watermark Remover"
BIN_NAME="watermark-remover"
BUNDLE_ID="app.jderrick.watermark-remover"
ICON_SRC="assets/icon-1024.png"
DIST="dist"
APP="$DIST/$APP_NAME.app"
VERSION="$(grep -m1 '^version' Cargo.toml | sed -E 's/.*"(.*)".*/\1/')"

echo "▶ building release binary (v$VERSION)…"
cargo build --release

echo "▶ assembling bundle…"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"

cp "target/release/$BIN_NAME" "$APP/Contents/MacOS/$BIN_NAME"
chmod +x "$APP/Contents/MacOS/$BIN_NAME"

# .icns from the 1024px master, via a temporary .iconset
if [[ -f "$ICON_SRC" ]]; then
  echo "▶ generating AppIcon.icns…"
  TMP="$(mktemp -d)"; ICONSET="$TMP/AppIcon.iconset"; mkdir -p "$ICONSET"
  for s in 16 32 128 256 512; do
    sips -z "$s" "$s"     "$ICON_SRC" --out "$ICONSET/icon_${s}x${s}.png"    >/dev/null
    sips -z "$((s*2))" "$((s*2))" "$ICON_SRC" --out "$ICONSET/icon_${s}x${s}@2x.png" >/dev/null
  done
  iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/AppIcon.icns"
  rm -rf "$TMP"
fi

echo "▶ writing Info.plist…"
cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>$APP_NAME</string>
  <key>CFBundleDisplayName</key><string>$APP_NAME</string>
  <key>CFBundleExecutable</key><string>$BIN_NAME</string>
  <key>CFBundleIdentifier</key><string>$BUNDLE_ID</string>
  <key>CFBundleVersion</key><string>$VERSION</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleIconFile</key><string>AppIcon</string>
  <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>LSApplicationCategoryType</key><string>public.app-category.graphics-design</string>
</dict>
</plist>
PLIST

# Ad-hoc sign so Gatekeeper allows local launch (no paid Developer ID needed).
echo "▶ ad-hoc code-signing…"
codesign --force --deep --sign - "$APP" >/dev/null 2>&1 \
  && echo "  signed (ad-hoc)" \
  || echo "  codesign unavailable — skipped"

# A zip for easy sharing/distribution.
echo "▶ zipping…"
( cd "$DIST" && ditto -c -k --sequesterRsrc --keepParent "$APP_NAME.app" "$APP_NAME.zip" )

echo
echo "✔ done:"
echo "   $APP"
echo "   $DIST/$APP_NAME.zip"
echo
echo "Run it:  open \"$APP\"     (or double-click in Finder)"
