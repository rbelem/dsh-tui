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

const require = createRequire(import.meta.url)

/** Stable Cordis plugin name. */
export const name = 'tui-runtime'

/** Services required before the TUI can be spawned. */
export const inject = ['tuiStartup', 'webServer']

/** Resolve the TUI binary path: config override, else the package's own bin. */
function resolveBinary(config) {
  if (config.binary) return config.binary
  try {
    return require.resolve('../bin/dsh-tui')
  } catch {
    /* v8 ignore next 2 -- reachable only on a broken package layout */
    throw new Error('tui: cannot resolve the TUI binary (bundle/bin/dsh-tui missing); build with `cargo build --release` and copy the binary into bundle/bin/ (see bundle/README.md)')
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
