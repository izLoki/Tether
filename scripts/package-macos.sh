#!/bin/bash
# Build a Finder-launchable bundle and architecture-specific disk image.
# Usage: scripts/package-macos.sh [aarch64-apple-darwin|x86_64-apple-darwin]
set -euo pipefail
cd "$(dirname "$0")/.."
if [[ "$(uname -s)" != Darwin ]]; then
  echo 'macOS packaging requires Xcode Command Line Tools and macOS.' >&2
  exit 1
fi
target="${1:-$(rustc -vV | sed -n 's/^host: //p')}"
case "$target" in
  aarch64-apple-darwin) arch=aarch64; lipo_arch=arm64 ;;
  x86_64-apple-darwin) arch=x86_64; lipo_arch=x86_64 ;;
  *) echo "Unsupported macOS target: $target" >&2; exit 1 ;;
esac
version="$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -n 1)"
export MACOSX_DEPLOYMENT_TARGET=11.0
cargo build --release --locked --target "$target"
staging="$(mktemp -d "${TMPDIR:-/tmp}/tether-bundle.XXXXXX")"
trap 'rm -rf "$staging"' EXIT
bundle="$staging/Tether.app"
mkdir -p "$bundle/Contents/MacOS" "$bundle/Contents/Resources" "$staging/Tether.iconset" dist
cp "target/$target/release/tether" "$bundle/Contents/MacOS/tether"
chmod 755 "$bundle/Contents/MacOS/tether"
lipo -verify_arch "$lipo_arch" "$bundle/Contents/MacOS/tether"
python3 - "$bundle/Contents/Info.plist" "$version" <<'PY'
import plistlib, sys
with open(sys.argv[1], 'wb') as out:
    plistlib.dump({
        'CFBundleName': 'Tether',
        'CFBundleDisplayName': 'Tether',
        'CFBundleIdentifier': 'io.github.izloki.tether',
        'CFBundleExecutable': 'tether',
        'CFBundlePackageType': 'APPL',
        'CFBundleShortVersionString': sys.argv[2],
        'CFBundleVersion': sys.argv[2],
        'CFBundleIconFile': 'Tether.icns',
        'LSMinimumSystemVersion': '11.0',
        'NSHighResolutionCapable': True,
        'NSHumanReadableCopyright': 'Copyright Tether contributors. MIT License.',
    }, out)
PY
for size in 16 32 128 256 512; do
  sips -z "$size" "$size" assets/icon.png --out "$staging/Tether.iconset/icon_${size}x${size}.png" >/dev/null
  double=$((size * 2))
  sips -z "$double" "$double" assets/icon.png --out "$staging/Tether.iconset/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$staging/Tether.iconset" -o "$bundle/Contents/Resources/Tether.icns"
rm -rf "$staging/Tether.iconset"
# Ad-hoc signing permits local execution on Apple Silicon. It is not Developer ID
# signing or notarization; see docs/macos.md before distributing to other users.
codesign --force --deep --sign - "$bundle"
codesign --verify --deep --strict "$bundle"
plutil -lint "$bundle/Contents/Info.plist"
ln -s /Applications "$staging/Applications"
image="Tether-$version-macos-$arch.dmg"
hdiutil create -volname Tether -srcfolder "$staging" -ov -format UDZO "dist/$image"
(cd dist && shasum -a 256 "$image" > "$image.sha256")
echo "Created dist/$image"
