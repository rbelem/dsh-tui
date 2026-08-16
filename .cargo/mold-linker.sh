#!/bin/sh
# Soft-fail mold linker (issue #16): use mold when it is on PATH (devbox.json
# provides it locally; CI installs it via apt), otherwise fall back to the
# default cc driver so a bare shell without mold never hard-fails.
#
# The flag is APPENDED after rustc's own args on purpose: gcc honors the LAST
# `-fuse-ld` flag it sees, so appending lets mold win when present and leaves
# rustc's default linker behavior untouched otherwise.
if command -v mold >/dev/null 2>&1; then
    exec cc "$@" -fuse-ld=mold
else
    exec cc "$@"
fi
