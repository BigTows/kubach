#!/usr/bin/env bash
# Builds and signs Kubach.app for macOS, so that Touch ID unlock works:
# the data protection keychain needs a signed app whose entitlements the
# provisioning profile allows (ADR 0008).
#
#   scripts/bundle-macos.sh [--debug] [--profile FILE] [--identity NAME] [--unsigned] [--dmg]
#
#   --debug      the dev build (fast) instead of release
#   --profile    provisioning profile, default $KUBACH_PROVISIONING_PROFILE
#   --identity   signing identity, default $KUBACH_SIGN_IDENTITY or the
#                first "Apple Development" / "Developer ID Application" one
#   --unsigned   no profile: ad-hoc signature, bundle id dev.kubach.app;
#                Touch ID is unavailable and the vault uses the password
#   --dmg        also pack the app into target/dist/Kubach-<version>-macos-<arch>.dmg
#
# The bundle identifier and team come from the profile. The app lands in
# target/<release|debug>/bundle/Kubach.app.
set -euo pipefail

build=release
profile="${KUBACH_PROVISIONING_PROFILE:-}"
identity="${KUBACH_SIGN_IDENTITY:-}"
unsigned=
dmg=
while [ $# -gt 0 ]; do
    case "$1" in
        --debug) build=debug ;;
        --profile) profile="$2"; shift ;;
        --identity) identity="$2"; shift ;;
        --unsigned) unsigned=1 ;;
        --dmg) dmg=1 ;;
        -h|--help) sed -n '2,19p' "$0"; exit 0 ;;
        *) echo "unknown option: $1" >&2; exit 2 ;;
    esac
    shift
done

if [ -z "$unsigned" ]; then
    [ -n "$profile" ] || { echo "no provisioning profile: pass --profile, set KUBACH_PROVISIONING_PROFILE or pass --unsigned" >&2; exit 2; }
    [ -f "$profile" ] || { echo "no such profile: $profile" >&2; exit 2; }
fi

root="$(cd "$(dirname "$0")/.." && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
version="$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -1)"

if [ -n "$unsigned" ]; then
    bundle_id=dev.kubach.app
    echo "bundle id  $bundle_id (unsigned: ad-hoc signature, no Touch ID)"
else
    # What the profile allows.
    security cms -D -i "$profile" > "$work/profile.plist"
    plist() { /usr/libexec/PlistBuddy -c "Print :$1" "$work/profile.plist"; }
    app_id="$(plist Entitlements:com.apple.application-identifier)"
    team="$(plist Entitlements:com.apple.developer.team-identifier)"
    bundle_id="${app_id#"$team".}"
    expires="$(plist ExpirationDate)"

    if [ -z "$identity" ]; then
        # A development profile lists devices and goes with an Apple
        # Development certificate; a Developer ID profile with Developer ID.
        if plist ProvisionedDevices >/dev/null 2>&1; then
            kind="Apple Development"
        else
            kind="Developer ID Application"
        fi
        identity="$(security find-identity -v -p codesigning \
            | grep -oE "\"$kind: [^\"]+\"" | head -1 | tr -d '"')"
    fi
    [ -n "$identity" ] || { echo "no signing identity in the keychain" >&2; exit 2; }

    echo "bundle id  $bundle_id (team $team, profile until $expires)"
    echo "identity   $identity"
fi
echo "build      $build, version $version"

cd "$root"
if [ "$build" = release ]; then
    cargo build --release -p kubach-app
else
    cargo build -p kubach-app
fi

target="${CARGO_TARGET_DIR:-$root/target}"
app="$target/$build/bundle/Kubach.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$target/$build/kubach" "$app/Contents/MacOS/kubach"
cp crates/kubach-app/assets/icon/kubach.icns "$app/Contents/Resources/kubach.icns"
[ -n "$unsigned" ] || cp "$profile" "$app/Contents/embedded.provisionprofile"

cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleDevelopmentRegion</key><string>en</string>
    <key>CFBundleDisplayName</key><string>Kubach</string>
    <key>CFBundleExecutable</key><string>kubach</string>
    <key>CFBundleIconFile</key><string>kubach</string>
    <key>CFBundleIdentifier</key><string>$bundle_id</string>
    <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
    <key>CFBundleName</key><string>Kubach</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>$version</string>
    <key>CFBundleVersion</key><string>$version</string>
    <key>LSApplicationCategoryType</key><string>public.app-category.developer-tools</string>
    <key>LSMinimumSystemVersion</key><string>11.0</string>
    <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST

if [ -n "$unsigned" ]; then
    codesign --force --sign - "$app"
else
    # Only what Touch ID needs; the keychain group is the app's own.
    cat > "$work/kubach.entitlements" <<ENT
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>com.apple.application-identifier</key><string>$app_id</string>
    <key>com.apple.developer.team-identifier</key><string>$team</string>
    <key>keychain-access-groups</key><array><string>$app_id</string></array>
</dict>
</plist>
ENT

    # Notarization wants a secure timestamp; development builds skip it.
    case "$identity" in
        "Developer ID Application"*) timestamp=--timestamp ;;
        *) timestamp=--timestamp=none ;;
    esac
    codesign --force --options runtime "$timestamp" \
        --entitlements "$work/kubach.entitlements" --sign "$identity" "$app"
fi
codesign --verify --strict --verbose=2 "$app"
echo "signed     $app"

if [ -n "$dmg" ]; then
    mkdir -p "$target/dist"
    image="$target/dist/Kubach-$version-macos-$(uname -m).dmg"
    stage="$work/dmg"
    mkdir -p "$stage"
    cp -R "$app" "$stage/"
    ln -s /Applications "$stage/Applications"
    rm -f "$image"
    hdiutil create -volname Kubach -srcfolder "$stage" -fs HFS+ -format UDZO "$image"
    echo "dmg        $image"
fi
