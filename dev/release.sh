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
# Signing and notarizing need nothing stored on the machine: no Developer ID certificate in the
# keychain and no app-specific password. Both go through the Apple ID that Xcode is signed in to
# (Xcode > Settings > Accounts, a member of the team below). Xcode signs with a cloud-managed
# Developer ID certificate, uploads for notarization over the same account, and hands the notarized
# app back with its ticket stapled.
# Signed out, or offline, the script still builds the bundle and says what it skipped, so it can be
# built and run locally; such a bundle is only for testing and must not be released.
set -eu

TEAM=F5H2D34D9G
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
# Nothing but the binary goes in MacOS/: Apple's notary service reads everything there as code and
# rejects the signature over a font or a licence sitting next to the executable ("The signature of
# the binary is invalid"). The font the app draws with is built into the binary, so the bundle needs
# no assets folder at all.

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
# Ad-hoc, with the hardened runtime turned on: Xcode's distribution step reads the runtime flag off
# the binary it is given and refuses an input without it ("Hardened Runtime is Not Enabled"). It
# replaces this signature with the real one.
codesign --force --options runtime --sign - "$app"

# ---------------------------------------------------------------- notarize
# A hand-made archive is all `xcodebuild -exportArchive` needs to sign and submit.
archive="$dist/AgentSignals.xcarchive"
rm -rf "$archive" "$dist/notarized"
mkdir -p "$archive/Products/Applications"
cp -R "$app" "$archive/Products/Applications/"
cat > "$archive/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>ArchiveVersion</key><integer>2</integer>
    <key>Name</key><string>$APP_NAME</string>
    <key>SchemeName</key><string>$APP_NAME</string>
    <key>CreationDate</key><date>$(date -u +%Y-%m-%dT%H:%M:%SZ)</date>
    <key>ApplicationProperties</key><dict>
        <key>ApplicationPath</key><string>Applications/$APP_NAME.app</string>
        <key>CFBundleIdentifier</key><string>$BUNDLE_ID</string>
        <key>CFBundleShortVersionString</key><string>$version</string>
        <key>CFBundleVersion</key><string>$version</string>
    </dict>
</dict>
</plist>
PLIST
cat > "$dist/exportOptions.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>method</key><string>developer-id</string>
    <key>teamID</key><string>$TEAM</string>
    <key>signingStyle</key><string>automatic</string>
    <key>destination</key><string>upload</string>
</dict>
</plist>
PLIST

release="$app"
echo "signing and submitting for notarization"
if xcodebuild -exportArchive -archivePath "$archive" -exportOptionsPlist "$dist/exportOptions.plist" \
        -exportPath "$dist/uploaded" -allowProvisioningUpdates > "$dist/upload.log" 2>&1; then
    # Notarization takes a minute or two; the notarized app comes back with its ticket stapled.
    echo "waiting for notarization"
    for _ in $(seq 1 30); do
        sleep 20
        rm -rf "$dist/notarized"
        if xcodebuild -exportNotarizedApp -archivePath "$archive" -exportPath "$dist/notarized" > "$dist/notarize.log" 2>&1; then
            release="$dist/notarized/$APP_NAME.app"
            break
        fi
        # "Invalid" is Apple's verdict, not a wait: stop and show why.
        if grep -q 'error: Invalid' "$dist/notarize.log"; then
            echo "notarization was refused; Apple's reasons:" >&2
            for log in "$archive"/Submissions/*/notarization-log.json; do
                [ -f "$log" ] && /usr/bin/python3 -c 'import json,sys
d = json.load(open(sys.argv[1]))
print(" ", d.get("status"), "-", d.get("statusSummary"))
for i in d.get("issues") or []:
    print("  -", i.get("severity"), i.get("path", ""), i.get("message"))' "$log" >&2
            done
            exit 1
        fi
    done
    if [ "$release" = "$app" ]; then
        echo "WARNING: notarization did not finish in time; see $dist/notarize.log." >&2
        echo "         Run the script again: the submission keeps going on Apple's side." >&2
        exit 1
    fi
    xcrun stapler validate "$release" > /dev/null
    spctl --assess --type execute "$release" > /dev/null
    echo "notarized, stapled, and Gatekeeper accepts it"
else
    echo "WARNING: could not sign through the Apple account; see $dist/upload.log."
    echo "         The bundle below is ad-hoc signed for local use only and must not be released."
fi

# ---------------------------------------------------------------- the zip
zip="$dist/AgentSignals-$version-macos-universal.zip"
rm -f "$zip"
# ditto keeps the bundle's symlinks and metadata; a plain zip does not.
ditto -c -k --keepParent "$release" "$zip"

echo
echo "$release"
echo "$zip"
echo "SHA256 $(shasum -a 256 "$zip" | cut -d' ' -f1)"
