#!/bin/sh
# Builds dist/RustTorrent.app: a release build wrapped as a Mac app that accepts magnet
# links and .torrent files. Pass extra cargo arguments after the script name.
set -eu
cd "$(dirname "$0")/.."

VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
APP=dist/RustTorrent.app

cargo build --release -p rusttorrent "$@"

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS"
cp target/release/rusttorrent "$APP/Contents/MacOS/rusttorrent"
# The icon is drawn in assets/icon.svg; see the README for how the .icns is made from it.
mkdir -p "$APP/Contents/Resources"
cp assets/RustTorrent.icns "$APP/Contents/Resources/RustTorrent.icns"
cp assets/RustTorrent.sdef "$APP/Contents/Resources/RustTorrent.sdef"

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>RustTorrent</string>
  <key>CFBundleDisplayName</key><string>RustTorrent</string>
  <key>CFBundleIdentifier</key><string>io.github.manojknaikade.rusttorrent</string>
  <key>CFBundleExecutable</key><string>rusttorrent</string>
  <key>CFBundleIconFile</key><string>RustTorrent</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleVersion</key><string>$VERSION</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.utilities</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSAppleScriptEnabled</key><true/>
  <key>OSAScriptingDefinition</key><string>RustTorrent.sdef</string>
  <key>CFBundleURLTypes</key>
  <array>
    <dict>
      <key>CFBundleURLName</key><string>BitTorrent Magnet Link</string>
      <key>CFBundleTypeRole</key><string>Viewer</string>
      <key>CFBundleURLSchemes</key><array><string>magnet</string></array>
    </dict>
  </array>
  <key>CFBundleDocumentTypes</key>
  <array>
    <dict>
      <key>CFBundleTypeName</key><string>BitTorrent File</string>
      <key>CFBundleTypeRole</key><string>Viewer</string>
      <key>LSHandlerRank</key><string>Default</string>
      <key>LSItemContentTypes</key><array><string>org.bittorrent.torrent</string></array>
      <key>CFBundleTypeExtensions</key><array><string>torrent</string></array>
    </dict>
  </array>
  <key>UTImportedTypeDeclarations</key>
  <array>
    <dict>
      <key>UTTypeIdentifier</key><string>org.bittorrent.torrent</string>
      <key>UTTypeDescription</key><string>BitTorrent File</string>
      <key>UTTypeConformsTo</key><array><string>public.data</string></array>
      <key>UTTypeTagSpecification</key>
      <dict>
        <key>public.filename-extension</key><array><string>torrent</string></array>
        <key>public.mime-type</key><array><string>application/x-bittorrent</string></array>
      </dict>
    </dict>
  </array>
</dict>
</plist>
PLIST

# Ad-hoc signature: enough to run on the Mac that built it and for Homebrew installs.
codesign --force -s - "$APP"

echo "Built $APP ($(du -sh "$APP" | cut -f1))"
