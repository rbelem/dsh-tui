#!/usr/bin/env bash
# Release build for one target (Q4: linux-x64/arm64 + darwin-x64/arm64
# prebuilds; Windows builds from source — no Windows target here).
#
# Cross-compiling macOS targets requires a macOS host (the darwin targets
# are built on the CI macos runners, never locally on Linux — rustup can
# install the toolchain, but linking needs the Apple SDK). This script
# builds the target when the host can, and fails with a clear message
# otherwise.
#
# Output: dist/dsh-tui-<target>/dsh-tui + dist/dsh-tui-<target>/SHA256SUMS
# (the byte-pin contract — SHA256SUMS is regenerated after every build).
#
# Version contract: the binary version must match the bundle version
# (bundle/package.json → tag v<version>). Asserted here, so a release build
# from a mismatched tree fails before any binary is produced.
#
# Usage: scripts/release/build.sh [--target <triple>]
#   (default target: the host triple)
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
BUNDLE_VERSION="$(node -p "require('$ROOT/bundle/package.json').version")"

TARGET="$(rustc -vV | sed -n 's/^host: //p')"
for arg in "$@"; do
  case "$arg" in
    --target) TARGET="" ;;
    --target=*) TARGET="${arg#--target=}" ;;
    *) if [[ -z "$TARGET" ]]; then TARGET="$arg"; fi ;;
  esac
done
if [[ -z "$TARGET" ]]; then
  echo "build: --target requires a triple" >&2
  exit 2
fi

CRATE_VERSION="$(cargo metadata --no-deps --format-version 1 | node -pe "JSON.parse(require('fs').readFileSync(0)).packages[0].version")"
if [[ "$CRATE_VERSION" != "$BUNDLE_VERSION" ]]; then
  echo "build: version mismatch — crate $CRATE_VERSION != bundle $BUNDLE_VERSION (tag v$BUNDLE_VERSION)" >&2
  exit 1
fi
echo "build: versions agree (crate == bundle == $BUNDLE_VERSION)"

# macOS targets only build on macOS hosts (Apple SDK for linking). Linux
# targets build on any Linux host with the target installed.
case "$TARGET" in
  *apple-darwin)
    if [[ "$(uname -s)" != "Darwin" ]]; then
      echo "build: $TARGET requires a macOS host — run the release workflow's darwin matrix leg (macos runner), not this machine" >&2
      exit 1
    fi
    ;;
esac

if ! rustup target list --installed | grep -qx "$TARGET"; then
  echo "build: target $TARGET not installed — run: rustup target add $TARGET" >&2
  exit 1
fi

OUT="dist/dsh-tui-$TARGET"
mkdir -p "$OUT"
echo "build: cargo build --release --target $TARGET"
(cd "$ROOT" && cargo build --release --target "$TARGET")
cp "$ROOT/target/$TARGET/release/dsh-tui" "$OUT/dsh-tui"
chmod 755 "$OUT/dsh-tui"

# Byte-pin: regenerate the checksum from the copied artifact.
"$ROOT/scripts/release/sha256.sh" "$OUT"
echo "build: $OUT ready (binary + SHA256SUMS)"
