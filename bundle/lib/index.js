/**
 * The dsh-tui runtime glue (mirrors the web-app bundle's web-runtime plugin):
 * resolves the TUI binary, awaits the webserver's OS-assigned port, spawns
 * the TUI with `DSH_PORT`, and prints the attach line once the Loader tree
 * settles (a sibling failure must not announce a dead app).
 *
 * The TUI itself is a pure client (reads DSH_PORT, never boots anything);
 * this glue owns the boot half: webserver + /api + WS downlinks on a port 0
 * the OS assigns, read back via `ctx.get('webServer').port` after bind.
 *
 * Plain ESM JS, no build step.
 * @module @rbelem/dsh-tui
 */

import { createRequire } from 'node:module'
import { spawn } from 'node:child_process'
import { dirname, join } from 'node:path'
import fs from 'node:fs'

const require = createRequire(import.meta.url)

/** Stable Cordis plugin name. */
export const name = 'tui-runtime'

/** Services required before the TUI can be spawned. */
export const inject = ['tuiStartup', 'webServer']

/**
 * The per-platform prebuild package for this host (npm's `os`/`cpu` fields
 * make installers fetch only the matching one): linux-x64, linux-arm64,
 * darwin-x64, darwin-arm64. Windows has no prebuild in v1 — the fallback
 * below (the bundle's own bin) carries the source-build story.
 */
const PLATFORM_PACKAGE = `@rbelem/dsh-tui-${process.platform}-${process.arch}`

/**
 * Resolve the TUI binary path. Resolution order:
 *
 *   1. `config.binary` — explicit override, always wins.
 *   2. The per-platform prebuild package (`@rbelem/dsh-tui-<platform>-<arch>`,
 *      an optional dependency of this bundle): resolved via `require.resolve`
 *      of its `package.json`, like the harness's landlock-run loader. When
 *      the package is installed but its binary is missing (partial install),
 *      fall through — the source-build bin is the meaningful fallback.
 *   3. The bundle's own `bin/dsh-tui` — the source-build fallback: the
 *      placeholder that fails loud with a build hint, or a locally built
 *      binary copied there (`cargo build --release && cp target/release/dsh-tui
 *      bundle/bin/`). Windows users build from source via this path.
 *
 * Throws when neither the platform package nor the local bin resolves
 * (broken package layout).
 */
export function resolveBinary(config) {
  if (config.binary) return config.binary
  try {
    const pkg = require.resolve(`${PLATFORM_PACKAGE}/package.json`)
    const binary = join(dirname(pkg), 'bin', 'dsh-tui')
    if (fs.existsSync(binary)) return binary
  } catch {
    // No such package for this host, or not installed — the local bin is
    // the fallback (the placeholder or a source-built copy).
  }
  try {
    return require.resolve('../bin/dsh-tui')
  } catch {
    /* v8 ignore next 2 -- reachable only on a broken package layout */
    throw new Error('tui: cannot resolve the TUI binary (no prebuild package and bundle/bin/dsh-tui missing); build with `cargo build --release` and copy the binary into bundle/bin/ (see bundle/README.md)')
  }
}

/**
 * Mount the TUI runtime: spawn the binary against the bound gateway and
 * print the attach line when `printUrl` is set.
 * @param ctx - plugin context carrying the tuiStartup and webServer services.
 * @param config - this row's config: `binary` override, `printUrl`, `spawn`.
 */
export function apply(ctx, config) {
  const port = ctx.webServer.port
  if (port === undefined) throw new Error('tui-runtime: webServer service missing while resolving the gateway port')

  if (config.spawn !== false) {
    const binary = resolveBinary(config)
    // The TUI connects over the loopback fence; stdio is inherited so the
    // terminal UI owns the screen. The gateway keeps serving after the TUI
    // quits (attach-continuity: `dsh web` and the TUI are interchangeable
    // clients of the same gateway).
    const child = spawn(binary, [], {
      env: { ...process.env, DSH_PORT: String(port) },
      stdio: 'inherit',
    })
    child.on('error', (error) => {
      console.error(`dsh-tui: failed to spawn the TUI: ${error.message}`)
    })
  }

  if (config.printUrl) {
    // The attach line is a readiness signal (supervisors and the keyless CLI
    // smoke RPC as soon as they observe it), so it must not print while
    // sibling rows (the /api route owner) are still mounting. Await Loader
    // settlement first; a hand-built tree without a Loader prints at once.
    const printAttach = () => {
      console.log(`dsh-tui attached to 127.0.0.1:${String(port)}`)
    }
    const settled = ctx.get('loader')?.await()
    if (settled === undefined) printAttach()
    else {
      void settled.then(() => {
        // The tree can be disposed while the boot was in flight (early
        // SIGTERM); an attach line for a dead gateway would mislead.
        if (ctx.get('webServer') !== undefined) printAttach()
      }, () => {})
    }
  }
}
