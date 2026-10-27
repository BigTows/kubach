#!/usr/bin/env bash
# Packages the release build for Linux into target/dist:
# kubach-<version>-linux-<arch>.tar.gz, Kubach-<version>-<arch>.AppImage,
# .deb, .rpm and Arch .pkg.tar.zst.
#
#   cargo build --release -p kubach-app && scripts/package-linux.sh
#
# Needs nfpm and appimagetool in PATH (CI downloads them). The packages
# don't bundle libraries: build on the oldest glibc they should run on
# (CI uses Ubuntu 22.04, glibc 2.35).
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
target="${CARGO_TARGET_DIR:-$root/target}"
version="$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -1)"
case "$(uname -m)" in
    x86_64) arch=x86_64 nfpm_arch=amd64 ;;
    aarch64|arm64) arch=aarch64 nfpm_arch=arm64 ;;
    *) echo "unsupported architecture: $(uname -m)" >&2; exit 2 ;;
esac

bin="$target/release/kubach"
[ -x "$bin" ] || { echo "no release build at $bin" >&2; exit 2; }
icons="$root/crates/kubach-app/assets/icon"
dist="$target/dist"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
mkdir -p "$dist"

# The /usr tree every format is made from.
stage="$work/stage"
install -Dm755 "$bin" "$stage/usr/bin/kubach"
install -Dm644 "$icons/dev.kubach.app.desktop" "$stage/usr/share/applications/dev.kubach.app.desktop"
install -Dm644 "$icons/kubach-512.png" "$stage/usr/share/icons/hicolor/512x512/apps/dev.kubach.app.png"
install -Dm644 "$icons/kubach.svg" "$stage/usr/share/icons/hicolor/scalable/apps/dev.kubach.app.svg"
install -Dm644 "$root/LICENSE" "$stage/usr/share/licenses/kubach/LICENSE"

# tar.gz: bin/ and share/, to unpack into /usr/local or ~/.local.
name="kubach-$version-linux-$arch"
cp -a "$stage/usr" "$work/$name"
tar -C "$work" -czf "$dist/$name.tar.gz" "$name"

# AppImage: the same tree, with the desktop entry and icon at the root.
appdir="$work/Kubach.AppDir"
cp -a "$stage" "$appdir"
ln -s usr/bin/kubach "$appdir/AppRun"
cp "$icons/dev.kubach.app.desktop" "$appdir/"
cp "$icons/kubach-512.png" "$appdir/dev.kubach.app.png"
ARCH="$arch" APPIMAGE_EXTRACT_AND_RUN=1 \
    appimagetool --no-appstream "$appdir" "$dist/Kubach-$version-$arch.AppImage"

for packager in deb rpm archlinux; do
    (cd "$stage" && ARCH="$nfpm_arch" VERSION="$version" \
        MAINTAINER="${KUBACH_MAINTAINER:-Kubach <https://github.com/BigTows/kubach/issues>}" \
        nfpm package --config "$root/packaging/linux/nfpm.yaml" --packager "$packager" --target "$dist/")
done

ls -l "$dist"
