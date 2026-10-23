#!/bin/sh
# Builds the Agent Signals.app that goes into a GitHub release, signed, notarized and stapled:
# target/dist/Agent Signals.app, zipped beside it.
#
#   sh dev/release.sh
#
# A Rust binary keeps the absolute paths of the sources it was built from (panic messages,
# generated code), which name the builder's home folder; they are remapped to /cargo and /build
# here, and the check near the end fails if a local path is left.
#
# Signing and notarizing need, once, on this machine:
#   - a "Developer ID Application" certificate in the keychain (Xcode > Settings > Accounts >
#     Manage Certificates > + > Developer ID Application);
#   - a notarytool keychain profile named below:
#       xcrun notarytool store-credentials agentsignals-notary \
#           --apple-id <apple id> --team-id <team id> --password <app-specific password>
# Without either, the script still builds the bundle and says what it skipped, so the bundle can be
# built and run locally; such a bundle is only for testing and must not be released.
set -eu

PROFILE=agentsignals-notary
BUNDLE_ID=com.appomobi.agentsignals
APP_NAME="Agent Signals"
MIN_MACOS=11.0

repo=$(cd "$(dirname "$0")/.." && pwd)
dist="$repo/target/dist"
app="$dist/$APP_NAME.app"
cargo_home=${CARGO_HOME:-$HOME/.cargo}
version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$repo/Cargo.toml" | head -1)

echo "Agent Signals $version"

# ---------------------------------------------------------------- the binary
# Both architectures, so the download runs on Apple silicon and on Intel.
RUSTFLAGS="--remap-path-prefix=$cargo_home=/cargo --remap-path-prefix=$repo=/build"
export RUSTFLAGS
CARGO_TARGET_DIR="$dist"
export CARGO_TARGET_DIR
cd "$repo"
for target in aarch64-apple-darwin x86_64-apple-darwin; do
    echo "building $target"
    cargo build --release --target "$target"
done

# ---------------------------------------------------------------- the bundle
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
lipo -create -output "$app/Contents/MacOS/agentsignals" \
    "$dist/aarch64-apple-darwin/release/agentsignals" \
    "$dist/x86_64-apple-darwin/release/agentsignals"
# The engine reads assets from beside the binary; the font is also built in, so this is a spare.
cp -R "$repo/assets" "$app/Contents/MacOS/assets"

# The icon: the same icon.ico the Windows build uses, as an .icns.
iconset=$(mktemp -d)/AppIcon.iconset
mkdir -p "$iconset"
sips -s format png "$repo/icon.ico" --out "$iconset/base.png" > /dev/null
for size in 16 32 128 256 512; do
    sips -z $size $size "$iconset/base.png" --out "$iconset/icon_${size}x${size}.png" > /dev/null
    double=$((size * 2))
    sips -z $double $double "$iconset/base.png" --out "$iconset/icon_${size}x${size}@2x.png" > /dev/null
done
rm "$iconset/base.png"
iconutil -c icns "$iconset" -o "$app/Contents/Resources/AppIcon.icns"
rm -rf "$(dirname "$iconset")"

cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>$APP_NAME</string>
    <key>CFBundleDisplayName</key><string>$APP_NAME</string>
    <key>CFBundleIdentifier</key><string>$BUNDLE_ID</string>
    <key>CFBundleExecutable</key><string>agentsignals</string>
    <key>CFBundleIconFile</key><string>AppIcon</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>$version</string>
    <key>CFBundleVersion</key><string>$version</string>
    <key>LSMinimumSystemVersion</key><string>$MIN_MACOS</string>
    <key>NSHighResolutionCapable</key><true/>
    <!-- A normal app with a Dock icon and a window, not a menu-bar-only one. -->
    <key>LSUIElement</key><false/>
</dict>
</plist>
PLIST

# No path of this machine inside the binary.
for path in "$HOME" "$repo" "$(id -un)"; do
    if strings -a "$app/Contents/MacOS/agentsignals" | grep -qF "$path"; then
        echo "local paths still in the binary: $path" >&2
        exit 1
    fi
done

# ---------------------------------------------------------------- signature
identity=$(security find-identity -v -p codesigning | sed -n 's/.*"\(Developer ID Application: .*\)"/\1/p' | head -1)
if [ -z "$identity" ]; then
    echo "WARNING: no Developer ID Application certificate; signing the bundle for local use only."
    echo "         This bundle must not be released."
    codesign --force --deep --sign - "$app"
    signed=no
else
    # Hardened runtime and a secure timestamp: what notarization requires.
    codesign --force --options runtime --timestamp --sign "$identity" "$app"
    codesign --verify --strict --verbose=2 "$app"
    signed=yes
fi

# ---------------------------------------------------------------- notarize
zip="$dist/AgentSignals-$version-macos.zip"
rm -f "$zip"
# ditto keeps the bundle's symlinks and metadata; a plain zip does not.
ditto -c -k --keepParent "$app" "$zip"

if [ "$signed" = yes ] && xcrun notarytool history --keychain-profile "$PROFILE" > /dev/null 2>&1; then
    echo "notarizing"
    xcrun notarytool submit "$zip" --keychain-profile "$PROFILE" --wait
    xcrun stapler staple "$app"
    # The stapled ticket lives in the bundle, so the zip is made again from it.
    rm -f "$zip"
    ditto -c -k --keepParent "$app" "$zip"
    spctl --assess --type execute --verbose=4 "$app"
else
    [ "$signed" = yes ] && echo "WARNING: no notarytool profile '$PROFILE'; not notarized." \
        || echo "not notarized (unsigned)."
fi

echo
echo "$app"
echo "$zip"
echo "SHA256 $(shasum -a 256 "$zip" | cut -d' ' -f1)"
