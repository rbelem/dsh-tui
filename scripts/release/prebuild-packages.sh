#!/usr/bin/env bash
# Assemble the per-platform prebuild packages from built dist artifacts:
# for each of prebuilds/<platform>/, copy the matching dist binary into
# bin/dsh-tui, chmod 755, write SHA256SUMS (the byte-pin contract), and
# verify. The result of each directory is `npm pack`-able.
#
# Usage: scripts/release/prebuild-packages.sh [--skip-version-check]
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"

# platform dir -> target triple
declare -A TARGETS=(
  [linux-x64]=x86_64-unknown-linux-gnu
  [linux-arm64]=aarch64-unknown-linux-gnu
  [darwin-x64]=x86_64-apple-darwin
  [darwin-arm64]=aarch64-apple-darwin
)

for platform in "${!TARGETS[@]}"; do
  target="${TARGETS[$platform]}"
  dist="dist/dsh-tui-$target"
  if [[ ! -f "$dist/dsh-tui" ]]; then
    echo "prebuild: $dist missing — run scripts/release/build.sh --target $target first (darwin targets on a macOS runner)" >&2
    MISSING=1
    continue
  fi
  pkg="prebuilds/$platform"
  mkdir -p "$pkg/bin"
  cp "$dist/dsh-tui" "$pkg/bin/dsh-tui"
  chmod 755 "$pkg/bin/dsh-tui"
  # The checksum lives next to the binary inside the package; regenerate
  # it from the copied bytes (the dist one is the same content, but the
  # copy is the shipped artifact).
  (cd "$pkg/bin" && sha256sum dsh-tui > SHA256SUMS)
  scripts/release/sha256.sh --verify "$pkg/bin"
  echo "prebuild: $pkg assembled (bin/dsh-tui + bin/SHA256SUMS)"
done

if [[ -n "${MISSING:-}" ]]; then
  echo "prebuild: some targets were not built — rerun after building the missing dists" >&2
  exit 1
fi
echo "prebuild: all packages assembled — run \`npm pack\` in each prebuilds/<platform>/ to produce the release tarballs"
