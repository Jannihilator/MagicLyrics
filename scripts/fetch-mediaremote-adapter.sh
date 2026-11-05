#!/usr/bin/env bash
# Build ungive/mediaremote-adapter (BSD-3) and place it where the macOS build expects it:
#   src-tauri/macos/mediaremote-adapter.pl
#   src-tauri/macos/MediaRemoteAdapter.framework
# Run on a Mac (needs Xcode Command Line Tools and cmake: `brew install cmake`).
set -euo pipefail

VERSION="v0.7.7"
COMMIT="e3ff5021eb0875858bd05f48d2e9ba2e962d1cf6"   # tag v0.7.7, pinned

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DEST="$ROOT/src-tauri/macos"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

git clone --quiet https://github.com/ungive/mediaremote-adapter.git "$WORK/src"
git -C "$WORK/src" checkout --quiet "$COMMIT"
cmake -S "$WORK/src" -B "$WORK/build" -DCMAKE_BUILD_TYPE=Release >/dev/null
cmake --build "$WORK/build" --config Release >/dev/null

rm -rf "$DEST"
mkdir -p "$DEST"
cp "$WORK/src/bin/mediaremote-adapter.pl" "$DEST/"
cp -R "$WORK/build/MediaRemoteAdapter.framework" "$DEST/"
cp "$WORK/src/LICENSE" "$DEST/LICENSE-mediaremote-adapter"

echo "mediaremote-adapter $VERSION -> $DEST"
# Quick check: prints the current now-playing info (or null).
/usr/bin/perl "$DEST/mediaremote-adapter.pl" "$DEST/MediaRemoteAdapter.framework" get --no-artwork || true
