#!/usr/bin/env bash
# SHA256SUMS for a dist/prebuild directory (the byte-pin contract): generate
# mode writes `SHA256SUMS` next to the binary; verify mode recomputes and
# compares, failing on any mismatch or missing file.
#
# Usage:
#   scripts/release/sha256.sh <dir>            # generate SHA256SUMS
#   scripts/release/sha256.sh --verify <dir>   # verify (exit 1 on mismatch)
set -euo pipefail

if [[ "${1:-}" == "--verify" ]]; then
  DIR="${2:?usage: sha256.sh --verify <dir>}"
  SUMFILE="$DIR/SHA256SUMS"
  if [[ ! -f "$SUMFILE" ]]; then
    echo "sha256: $SUMFILE missing" >&2
    exit 1
  fi
  # Verify the listed entries against the on-disk bytes (whitespace-agnostic
  # parse: "hash  name" per line, the format `sha256sum` emits).
  (cd "$DIR" && sha256sum -c --quiet SHA256SUMS)
  echo "sha256: $DIR verified ok"
  exit 0
fi

DIR="${1:?usage: sha256.sh <dir>}"
if [[ ! -d "$DIR" ]]; then
  echo "sha256: no such directory: $DIR" >&2
  exit 1
fi
(cd "$DIR" && sha256sum dsh-tui > SHA256SUMS)
echo "sha256: wrote $DIR/SHA256SUMS ($(wc -l < "$DIR/SHA256SUMS") entry)"
