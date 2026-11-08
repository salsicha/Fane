#!/usr/bin/env bash
# Build dist/Fane.app (universal: Apple silicon + Intel) and dist/Fane-<version>-macos.dmg.
# Run on macOS with Xcode command-line tools.
set -euo pipefail
cd "$(dirname "$0")/../.."

version=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
targets=(aarch64-apple-darwin x86_64-apple-darwin)
for t in "${targets[@]}"; do
    rustup target add "$t"
    cargo build --release --locked --target "$t"
done

app=dist/Fane.app
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
lipo -create -output "$app/Contents/MacOS/fane" \
    target/aarch64-apple-darwin/release/fane target/x86_64-apple-darwin/release/fane

iconset=$(mktemp -d)/fane.iconset
mkdir -p "$iconset"
for s in 16 32 128 256 512; do
    "$app/Contents/MacOS/fane" render-icon "$iconset/icon_${s}x${s}.png" "$s"
    "$app/Contents/MacOS/fane" render-icon "$iconset/icon_${s}x${s}@2x.png" "$((s * 2))"
done
iconutil -c icns "$iconset" -o "$app/Contents/Resources/fane.icns"

# CodeGem with its own Python, one per CPU type (Fane picks codegem-<arch> at runtime).
packaging/codegem/bundle.sh "$app/Contents/Resources/codegem-aarch64" macos-aarch64
packaging/codegem/bundle.sh "$app/Contents/Resources/codegem-x86_64" macos-x86_64
if [ "$(uname -m)" = arm64 ]; then
    "$app/Contents/Resources/codegem-aarch64/python/bin/python3" -I -c \
        "import sys; from superqode.main import cli_main; sys.argv=['superqode','--version']; sys.exit(cli_main())"
fi

sed "s/@VERSION@/$version/g" packaging/macos/Info.plist > "$app/Contents/Info.plist"
# Ad-hoc signature: required to run on Apple silicon. Not notarized, so the first launch
# needs right-click → Open (or `xattr -dr com.apple.quarantine /Applications/Fane.app`).
codesign --force --deep --sign - "$app"

dmg=dist/Fane-$version-macos.dmg
rm -f "$dmg"
staging=$(mktemp -d)
cp -R "$app" "$staging/"
ln -s /Applications "$staging/Applications"
hdiutil create -volname Fane -srcfolder "$staging" -ov -format UDZO "$dmg"
echo "built $dmg"
