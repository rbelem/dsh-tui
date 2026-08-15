#!/usr/bin/env bash
# Publish the dsh-tui npm packages: the four per-platform prebuilds first,
# the entry bundle last. Publishes the SAME on-disk artifacts the release
# pipeline built (scripts/release/build.sh + prebuild-packages.sh) — this
# script never rebuilds, only packs/publishes the assembled package
# directories (byte-pinned: every prebuild carries bin/SHA256SUMS).
#
# Version source of truth: bundle/package.json (the established contract —
# build.sh asserts crate == bundle, release.yml asserts tag == bundle
# version). publish.sh asserts ALL five packages + the crate agree and
# fails otherwise.
#
# Package order: platform packages FIRST, entry bundle LAST — npm resolves
# the entry's optionalDependencies at install time, so the platform
# packages must already exist on the registry before the entry references
# them.
#
# Safety:
#   - Default is DRY-RUN: `npm pack --dry-run` (with a file-list sanity
#     check) then `npm publish --dry-run` for every package — zero
#     registry mutation.
#   - Real publish requires the explicit `--publish` flag OR `PUBLISH=1`.
#   - `npm whoami` is checked first; unauthenticated real publish refuses
#     to run (no token is ever printed or logged).
#   - Fail-fast: stops at the first failing package and reports which
#     packages already landed.
#
# Usage:
#   scripts/release/publish.sh               # dry-run for all 5 packages
#   scripts/release/publish.sh --publish     # real publish (auth required)
#   PUBLISH=1 scripts/release/publish.sh     # same as --publish
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"

# ---------------------------------------------------------------------------
# flags
# ---------------------------------------------------------------------------
DO_PUBLISH=0
for arg in "$@"; do
  case "$arg" in
    --publish) DO_PUBLISH=1 ;;
    *) echo "publish: unknown argument: $arg (expected --publish)" >&2; exit 2 ;;
  esac
done
if [[ "${PUBLISH:-}" == "1" ]]; then
  DO_PUBLISH=1
fi

# ---------------------------------------------------------------------------
# version alignment: bundle/package.json is the source of truth; crate and
# every package must agree (build.sh's contract, enforced here too).
# ---------------------------------------------------------------------------
BUNDLE_VERSION="$(node -p "require('$ROOT/bundle/package.json').version")"
CRATE_VERSION="$(cargo metadata --no-deps --format-version 1 | node -pe "JSON.parse(require('fs').readFileSync(0)).packages[0].version")"
if [[ "$CRATE_VERSION" != "$BUNDLE_VERSION" ]]; then
  echo "publish: version mismatch — crate $CRATE_VERSION != bundle $BUNDLE_VERSION" >&2
  exit 1
fi

PLATFORMS=(linux-x64 linux-arm64 darwin-x64 darwin-arm64)
# Publish order: platforms first, entry bundle last.
PACKAGES=()
for platform in "${PLATFORMS[@]}"; do
  PACKAGES+=("prebuilds/$platform")
done
PACKAGES+=(bundle)

for pkg in "${PACKAGES[@]}"; do
  version="$(node -p "require('$ROOT/$pkg/package.json').version")"
  if [[ "$version" != "$BUNDLE_VERSION" ]]; then
    echo "publish: version mismatch — $pkg $version != bundle $BUNDLE_VERSION" >&2
    exit 1
  fi
done
echo "publish: versions agree (crate == bundle == all packages == $BUNDLE_VERSION)"

# ---------------------------------------------------------------------------
# entry dependency contract: the bundle's optionalDependencies must pin the
# four platform packages at exactly the bundle version (npm resolves them
# at install time; a drift here publishes an entry that cannot find its
# prebuilds).
# ---------------------------------------------------------------------------
node -e '
const bundle = require("./bundle/package.json");
const version = bundle.version;
const platforms = ["linux-x64", "linux-arm64", "darwin-x64", "darwin-arm64"];
const deps = bundle.optionalDependencies || {};
let failed = false;
for (const p of platforms) {
  const name = `@rbelem/dsh-tui-${p}`;
  if (deps[name] !== version) {
    console.error(`publish: bundle optionalDependency ${name} = ${deps[name] ?? "(missing)"}, expected exact ${version}`);
    failed = true;
  }
}
if (failed) process.exit(1);
console.log(`publish: bundle optionalDependencies pin all ${platforms.length} platforms at ${version}`);
'

# ---------------------------------------------------------------------------
# registry auth: never print or log tokens. Real publish refuses to run
# unauthenticated; dry-run continues (publish --dry-run is warn-only
# without auth and mutates nothing).
# ---------------------------------------------------------------------------
WHOAMI="$(npm whoami 2>/dev/null || true)"
if [[ -z "$WHOAMI" ]]; then
  if [[ "$DO_PUBLISH" == "1" ]]; then
    echo "publish: not authenticated — run \`npm login\` (or set NODE_AUTH_TOKEN + registry-url in CI) before --publish" >&2
    exit 1
  fi
  echo "publish: WARNING not authenticated (npm whoami failed) — DRY-RUN only; real publish requires auth"
else
  echo "publish: authenticated as $WHOAMI"
fi

# ---------------------------------------------------------------------------
# per-package pack/publish
# ---------------------------------------------------------------------------
LANDED=()
fail() {
  echo "publish: FAILED at ${1:-unknown step}" >&2
  if [[ "${#LANDED[@]}" -gt 0 ]]; then
    echo "publish: packages already published: ${LANDED[*]}" >&2
  fi
  exit 1
}
trap 'fail "$BASH_COMMAND"' ERR

for pkg in "${PACKAGES[@]}"; do
  name="$(node -p "require('$ROOT/$pkg/package.json').name")"
  echo "=== $name ($BUNDLE_VERSION) — $pkg ==="

  # Prebuild packages must already be assembled by prebuild-packages.sh
  # (binary + byte-pinned checksum); the bundle ships its source-build
  # fallback bin. Fail fast with a clear message otherwise.
  if [[ "$pkg" == prebuilds/* ]]; then
    if [[ ! -f "$ROOT/$pkg/bin/dsh-tui" || ! -f "$ROOT/$pkg/bin/SHA256SUMS" ]]; then
      echo "publish: $pkg/bin missing dsh-tui/SHA256SUMS — run scripts/release/prebuild-packages.sh first (darwin targets on a macOS runner)" >&2
      exit 1
    fi
    "$ROOT/scripts/release/sha256.sh" --verify "$ROOT/$pkg/bin"
  fi

  # Sanity: pack dry-run with a file-list check. Explicit `files` entries
  # are REQUIRED — a `files: ["bin/"]` glob lets the package's nested
  # .gitignore exclude the binary from the tarball (regression guard).
  PACK_JSON="/tmp/dsh-tui-pack-${pkg//\//-}.json"
  (cd "$ROOT/$pkg" && npm pack --dry-run --json) > "$PACK_JSON" 2>/dev/null
  node -e '
const fs = require("fs");
const pkg = process.argv[1];
const bundle = pkg === "bundle";
const files = JSON.parse(fs.readFileSync(`/tmp/dsh-tui-pack-${pkg.replaceAll("/", "-")}.json`, "utf8"))[0].files.map(f => f.path);
const required = bundle
  ? ["lib/index.js", "lib/startup.js", "cordis.patch.yml", "package.json"]
  : ["bin/dsh-tui", "bin/SHA256SUMS", "package.json"];
const missing = required.filter(r => !files.includes(r));
if (missing.length) {
  console.error(`publish: ${pkg} tarball missing ${missing.join(", ")} (files: ${files.join(", ")}) — explicit files entries required`);
  process.exit(1);
}
console.log(`publish: ${pkg} pack ok (${files.length} files: ${files.join(", ")})`);
' "$pkg"
  rm -f "$PACK_JSON"

  echo "publish: npm publish --dry-run $name@$BUNDLE_VERSION"
  (cd "$ROOT/$pkg" && npm publish --dry-run)

  if [[ "$DO_PUBLISH" == "1" ]]; then
    echo "publish: REAL npm publish $name@$BUNDLE_VERSION"
    (cd "$ROOT/$pkg" && npm publish)
    LANDED+=("$name@$BUNDLE_VERSION")
  fi
done

trap - ERR
if [[ "$DO_PUBLISH" == "1" ]]; then
  echo "publish: DONE — published: ${LANDED[*]}"
else
  echo "publish: DONE — dry-run only (no registry mutation); re-run with --publish to publish"
fi
