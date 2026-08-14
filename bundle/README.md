# `@rbelem/dsh-tui`

The dsh terminal-UI bundle. [`cordis.patch.yml`](cordis.patch.yml) rides over
`dsh-base` and inserts the transport rows the TUI needs — webserver
(127.0.0.1, `port: 0` = OS-assigned, read back after bind), the API gateway
(`dsh-host-apiproxy`), and the connection (`dsh-client-connection`, which
registers the `/api` prefix and both WS downlinks on the webserver) — plus
this bundle's `tui-runtime` glue plugin. That plugin resolves the TUI binary
(config `binary`, defaulting to the package's own `bin/dsh-tui`), awaits the
bound port, spawns the TUI with `DSH_PORT=<port>` and inherited stdio, and
prints `dsh-tui attached to 127.0.0.1:<port>` when `printUrl` is true, after
its Loader tree settles. The bundle also owns the app command line: the
ordinary `tui-startup` provider ([`lib/startup.js`](lib/startup.js)) injects
`ctx.cmdlineArgs`, parses `--port` and `--no-spawn` and the app's `--help`,
then provides `tuiStartup`. Flag-configured rows inject the service and read
it from lazy config, so nothing binds a port before argument resolution and
`dsh --profile tui --help` starts no server.

The TUI itself is a **pure client**: it reads `DSH_PORT` (or `--port`) and
never boots anything, so it also attaches to any running gateway serving the
web profile (`dsh web` running + the binary with `DSH_PORT`).

## Install

```sh
dsh plugin --profile tui add @rbelem/dsh-tui
```

## Boot

```sh
dsh --profile tui              # boots the gateway (OS-assigned port) and spawns the TUI
dsh --profile tui --port 8080  # fixed port
dsh --profile tui --no-spawn   # gateway only; attach later with the binary + DSH_PORT
```

The gateway keeps serving after the TUI quits (attach-continuity: web and
TUI are interchangeable clients of the same sessions).

## Attach-only usage

With `dsh web` (or any gateway-bearing profile) already running:

```sh
dsh-tui --port <port>
# or: DSH_PORT=<port> dsh-tui
```

## Prebuild contract

`bundle/bin/dsh-tui` is the layout contract for prebuilt binaries. v1 ships a
placeholder that fails loud with a build hint. The release pipeline (a later
task, mirroring the harness's `native/landlock-run` precedent) replaces it
with platform binaries shipped as per-platform optional-dependency packages
(`linux-x64`, `linux-arm64`, `darwin-x64`, `darwin-arm64`; Windows builds
from source), loaded via `require.resolve`, with no lifecycle install
scripts.

Until a prebuild matches, build and copy:

```sh
cargo build --release
cp target/release/dsh-tui bundle/bin/
```

## Not yet

- The prebuild release pipeline and per-platform packages (see the contract
  above).
- A hardcoded `dsh tui` alias in the dsh CLI (`apps/cli/src/args.ts`,
  mirroring `web`) — the external install path above comes first.
- A real `dsh` smoke in CI — `scripts/smoke-install.sh` runs locally when
  `dsh` is on PATH.
