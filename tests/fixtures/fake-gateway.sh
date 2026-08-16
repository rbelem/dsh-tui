#!/usr/bin/env bash
# tests/fixtures/fake-gateway.sh — a stand-in for `dsh web` under the
# DSH_TUI_GATEWAY_BIN injection seam: binds the --port argument and holds
# the port open (so the client's loopback probe succeeds). Exits 77 when
# python3 is unavailable so tests can skip.
set -u
PORT=""
while [ $# -gt 0 ]; do
  case "$1" in
    --port) PORT="${2:-}"; shift 2 ;;
    --port=*) PORT="${1#*=}"; shift ;;
    *) shift ;;
  esac
done
if [ -z "$PORT" ]; then
  echo "fake-gateway: no --port argument" >&2
  exit 2
fi
if ! command -v python3 >/dev/null 2>&1; then
  echo "fake-gateway: python3 unavailable" >&2
  exit 77
fi
exec python3 - "$PORT" <<'EOF'
import socket, sys
port = int(sys.argv[1])
s = socket.socket()
s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
s.bind(("127.0.0.1", port))
s.listen(1)
while True:
    conn, _ = s.accept()
    conn.close()
EOF
