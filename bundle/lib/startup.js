/**
 * The TUI's command-line provider (mirrors the web-app bundle's startup
 * plugin): parses the `dsh --profile tui` flag family (`--port`,
 * `--no-spawn`) and its `--help` text, then provides the immutable values as
 * the `tuiStartup` service. Rows configured from flags inject that service
 * and read it from lazy config, so nothing binds a port before argument
 * resolution and `dsh --profile tui --help` starts no server.
 *
 * Plain ESM JS, no build step: the plugin shape is `name` + `apply`, and the
 * artifact is checked in (the web-app bundle compiles TS; this bundle keeps
 * the glue dependency-free and build-free).
 * @module @rbelem/dsh-tui/startup
 */

import { Command } from 'commander'
import { parseCmdline } from '@deepseek-ai/dsh-cmdline'

/** Stable Cordis plugin name. */
export const name = 'tui-startup'

/** Services required before the flags can be resolved. */
export const inject = ['cmdlineArgs']

/** Service provided by this ordinary plugin and injected by flag-configured rows. */
export const TUI_STARTUP_SERVICE = 'tuiStartup'

/** The TUI flag family, as commander parsed it. */
function tuiCommand() {
  return new Command()
    .name('dsh --profile tui')
    .description('Boot the gateway and spawn the dsh-tui terminal UI.')
    .helpOption('-h, --help', 'show this help')
    .option('--port <port>', 'gateway listen port; 0 lets the OS pick a free one (default)')
    .option('--no-spawn', 'boot the gateway without spawning the TUI (attach later from another terminal)')
    .addHelpText('after', `
Examples:
  dsh --profile tui                         boot the gateway (OS-assigned port) and spawn the TUI
  dsh --profile tui --port 8080             boot on a fixed port
  dsh --profile tui --no-spawn              gateway only; run the binary yourself with DSH_PORT set
`)
}

/**
 * Parse and provide the TUI invocation as an ordinary Cordis service. The
 * command's action publishes the flags this invocation named; a non-numeric
 * `--port` is a usage error, so on rejection (and on `--help`) nothing is
 * provided and no row binds a port.
 * @param ctx - plugin context carrying the command line.
 */
export function apply(ctx) {
  const program = tuiCommand()
  program.action(() => {
    const options = program.opts()
    if (options.port !== undefined && !/^\d+$/.test(options.port)) {
      program.error(`error: --port must be a number, got ${JSON.stringify(options.port)}`)
    }
    ctx.provide(TUI_STARTUP_SERVICE, {
      ...(options.port !== undefined ? { port: Number(options.port) } : {}),
      spawn: options.spawn,
    })
  })
  parseCmdline(ctx, program)
}
