#!/bin/sh
# Download and verify a GitHub release before installing. No root required.
set -eu
repo=https://github.com/KotalaKishanReddy/shrinkgate
version=${SHRINKGATE_VERSION:-}
if [ -z "$version" ]; then
  release_url=$(curl --proto '=https' --tlsv1.2 -fsSL -o /dev/null -w '%{url_effective}' "$repo/releases/latest")
  version=${release_url##*/}
fi
case "$version" in v[0-9]*.[0-9]*.[0-9]*) ;; *) echo 'Invalid release version' >&2; exit 1 ;; esac
case "$version" in *[!v0-9.]*) echo 'Only stable numeric release tags are supported' >&2; exit 1 ;; esac
case "$(uname -s)/$(uname -m)" in
  Linux/x86_64) platform=x86_64-linux ;;
  Darwin/arm64) platform=aarch64-macos ;;
  Darwin/x86_64) platform=x86_64-macos ;;
  *) echo 'Unsupported platform; build with cargo install shrinkgate --locked' >&2; exit 1 ;;
esac
archive=shrinkgate-$version-$platform.tar.gz
install_dir=${SHRINKGATE_INSTALL_DIR:-"$HOME/.local/bin"}
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT HUP INT TERM
curl --proto '=https' --tlsv1.2 -fsSL "$repo/releases/download/$version/$archive" -o "$tmp/$archive"
curl --proto '=https' --tlsv1.2 -fsSL "$repo/releases/download/$version/sha256sums.txt" -o "$tmp/checksums"
awk -v name="$archive" '$2 == name { print; found=1 } END { if (!found) exit 1 }' "$tmp/checksums" > "$tmp/selected.sha256"
(cd "$tmp" && if command -v sha256sum >/dev/null 2>&1; then sha256sum -c selected.sha256; else shasum -a 256 -c selected.sha256; fi)
# Extract only the executable, after its archive checksum passes.
tar -xzf "$tmp/$archive" -C "$tmp" ./shrinkgate
mkdir -p "$install_dir"
install -m 755 "$tmp/shrinkgate" "$install_dir/shrinkgate"
printf 'Installed %s to %s/shrinkgate\nEnsure %s is on PATH.\n' "$version" "$install_dir" "$install_dir"
