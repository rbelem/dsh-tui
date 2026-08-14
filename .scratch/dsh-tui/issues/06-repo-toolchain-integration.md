# Repo + toolchain integration

Type: grilling
Status: resolved
Blocked by: 02

## Question

Where does Rust live in the repo and how does `dsh tui` boot?

Decide:

- Crate location: top-level `tui/` vs under `native/`; Cargo workspace vs standalone crate.
- Dev boot: how `dsh tui` builds/spawns the TUI in development (cargo run spawn vs
  prebuilt binary) and how the bundle glue plugin attaches it to the in-process gateway.
- Distribution: prebuilds à la `native/landlock-run`, or build-from-source; the
  platform matrix (linux/macOS/Windows terminal support).
- CI: cargo test/clippy/rustfmt on the matrix, and how it plugs into the repo gates
  (`pnpm run hygiene`-adjacent).
- Consequence on protocol bindings: the answer depends on Typert → Rust codegen.

## Facts (from fact-find lane)

- **landlock-run precedent**: C (musl-gcc static), per-platform npm packages (`linux-x64`, `linux-arm64`) with `os`/`cpu` + `optionalDependencies` on the entry package, loader via `require.resolve`, `assemble-prebuilds.mjs` + byte-pinned `verify-packed-install.mjs`, **no lifecycle install scripts** invariant, CI is builder of record, no cross toolchain, no Rust anywhere in repo.
- **Web profile boot**: `tui` is already a valid profile name (`--profile tui` works via `resolveBoot`); `web` is a hardcoded CLI alias (args.ts:156-169). web-app bundle = `dsh.bundle.patch: ./cordis.patch.yml` + deps; patch mounts `webserver` (host 127.0.0.1, port default 3080, `port:0` = OS-assigned read back via `get port()`), `api-gateway` (`dsh-host-apiproxy`), `connection` (`dsh-client-connection`, registers `/api` prefix + both WS upgrades on the webserver). URL line printed app-side gated on `config.printUrl`, awaiting Loader settlement (readiness signal).
- **Spawn patterns**: `subprocess-local` `spawnSubprocess` (detached, scrubbed env, group signalling, taskkill fallback) is the in-harness seam; `sdk/client` spawns runtime over JSON-RPC stdio as the documented out-of-context exception.
- **CI**: `.github/workflows/ci.yml` (937 lines) + `landlock-run.yml` (per-arch matrix) + `landlock-run-release.yml` (prebuild artifacts); all gates funnel through `scripts/run-gates.ts`; windows under Wine via `wine-windows-gates.sh`; `hygiene` chains knip/publint/constraints.

## Answer (settled so far)

- **Q1 (location, amended)**: code lives in a **new repository `rbelem/dsh-tui`** — not in the harness monorepo. The harness is untouched by default.
- **Q2 (workspace)**: standalone crate (workspace-shaped later if the protocol-bindings module needs sharing).
- **Q3 (dev boot)**: dev = `cargo run` (spawn with port + session env); release = prebuilt binary.
- **Q4 (distribution)**: prebuilds for linux-x64/arm64 + darwin-x64/arm64; Windows build-from-source initially; source-build fallback when no prebuild matches.
- **Q5 (shape, amended)**: `dsh tui` is a **plugin/bundle living in the external repo**, installed into the harness — mirrors the web-app bundle shape (`cordis.patch.yml` patch layer + glue plugin), but shipped from `rbelem/dsh-tui`.
- **Q6 (CI)**: cargo job (fmt/clippy/test) + gate wrapper; in the new repo's own CI (not the harness gates).
- **Q7 (boot, amended)**: the TUI must work **while the web UI is available** — it connects to the same running gateway (which may also be serving `dsh web`) and **continues from where the web left off** (same sessions, same state).

### Round 2 (Q8–Q11) — settled

- **Q8 (attach-or-boot)**: `dsh tui` attaches to a running harness serving the gateway (web or headless profile); if none is running, it boots the gateway half (webserver + `/api` + WS downlinks, `port: 0` OS-assigned) then spawns the TUI with `DSH_PORT` env. The TUI itself is a **pure client** — reads `DSH_PORT`/flag, never boots anything, attachable to any profile.
- **Q9 (continuity)**: on attach, TUI does `session.list` → opens the most recently active session → `session.history` for the full log → subscribes `events.mux`; wire replays pending approval/question frames on subscribe, so an **in-flight turn started in the web appears live in the TUI** — mid-turn state included, no server changes. No active session → land on session list.
- **Q10 (concurrent clients)**: no client exclusivity — web and TUI are interchangeable (prompt/cancel/answer from either); first response wins per approvalId; `approval/resolved` broadcasts to both — exactly like two browser tabs. No new server behavior.
- **Q11 (surface)**: external-only first — install `dsh plugin --profile tui add @rbelem/dsh-tui`, boot with `dsh --profile tui`; the bundle's cmdline plugin owns TUI flags (port, session dir). Coexist-with-web case needs no profile boot at all: `dsh web` running + `dsh-tui --port <port>`. A hardcoded `dsh tui` alias in `apps/cli/src/args.ts` (mirroring `web`) is a **later one-line convenience PR**, after the external path is proven.

### External bundle install (fact-find lane 2)

- **No `dsh bundle` command.** Install path = `dsh plugin --profile <name> add <package>` (args.ts:171-181): forwards to `pnpm add` in the profile dir (`$DSH_HOME/profiles/<name>`), then `reconcilePlugins` (plugin.ts:59-91) rewrites the profile's `dsh.profile.bundles` from installed deps — any dep whose manifest declares `dsh.bundle.patch` is appended (exportsPatch :36-45).
- **External bundle = npm package** with `{"dsh":{"bundle":{"patch":"./cordis.patch.yml"}}}` (packages/bundle/README.md:5,13). `resolveBundleDir` (app-boot profile.ts:344-355) resolves two-anchored: dsh install first, profile dir second — out-of-tree bundles come from the profile's node_modules.
- **`web` is a shipped profile template** (`PROFILE_TEMPLATES.web`, profile.ts:114-117); `dsh web` alias is hardcoded in args.ts:156.
- **Extension surface**: a bundle patches composition + provides app-owned args via a cmdline plugin (`ctx.cmdlineArgs`, `dsh --profile <name> --<flag>`); it CANNOT register a new top-level `dsh <subcommand>` — that requires editing apps/cli/src/args.ts.
