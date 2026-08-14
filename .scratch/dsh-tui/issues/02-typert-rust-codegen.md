# Typert → Rust codegen

Type: research
Status: resolved
Blocked by: —

## Question

How do the wire types reach Rust?

Deliver, with `file:line` evidence from `packages/typert` (generator, loader, runtime)
and `packages/api/gateway`:

1. What output formats the Typert generator emits today — TypeScript only, or any
   JSON Schema / OpenAPI / intermediate representation?
2. The generator's pipeline structure (input types → IR → TS emit) and whether a
   JSON-Schema emitter would be a small sibling of the TS emitter.
3. How the repo gates generated code (regeneration checks, snapshot gates?).
4. Which subset of wire types a chat TUI actually needs (session, conversation nodes,
   tool calls, approvals, settings).

Answer the decision: is the smallest credible path (a) adding a JSON-Schema emit to
the generator + `typify`/`progenitor` for Rust bindings, or (b) hand-maintained serde
models for a frozen protocol subset? State which is less total work including the
protocol-evolution update story.

## Answer

**Decision: (b) hand-maintained serde for a frozen subset — less total work today.**

### Findings

1. **Output formats today: TypeScript only.** `FaceModelEmitter.emit()` produces per package per face a `.js` (embedded zod schemas + serialized `TYPERT` runtime model) and `.d.ts` (emitter.ts:104-128); host packages additionally emit `.remote-client.js/.d.ts` (emitter.ts:42-46). No JSON-Schema/OpenAPI/serialized-IR artifact. The only IR is in-memory (`FaceModel`/`TypeGraph`/`TypeNodeModel`, model.ts:184,350).
2. **Pipeline:** analyzer.ts (TS AST → FaceModel) → `FaceModelEmitter.emit` (emitter.ts:104) → two sinks: `SchemaEmitter` (zod; emitter.ts:512) and `TypeGraphRenderer` (.d.ts strings; renderer.ts:26). Both consume `TypeNodeModel` via the same `TypeGraphRenderer` boundary. `SchemaEmitter` already switches over the 22 node kinds (emitter.ts:604-795). A JSON-Schema emitter would be a **genuinely small sibling** — a parallel `renderSchema(node, …)` switch over the same kinds, no new analysis.
3. **No reusable gate for (a).** Wire contracts are build-time artifacts, not committed: `lib/` is gitignored, generation runs in tsdown's `writeBundle` (tsdown-plugin.ts:60-124); the gate is just `typert-contracts` = the build compiling (run-gates.ts:458-459). No regeneration/diff/snapshot gate on wire bytes exists. Freshness gates that exist (gen-*-catalog --check, verify-type-equiv.ts) are for docs/catalogs, not wire contracts.
4. **Chat-TUI subset is tiny and already declaratively specified.** Gateway envelope `InvokeRemoteRequest`/`TypertGatewayErrorCode` (api/gateway/src/types.ts:7,19); dispatch `/api/<namespace>/<method>` with `{ args }` (gateway index.ts:194-222). Data types are the host/client RPC schemas in `packages/host/apiproxy/src/api/`: `sessions.schema.ts:27-202`, `events.schema.ts:44-86`, `approvals.schema.ts`, `settings.schema.ts:18-81`, `questions.schema.ts`. Hand-authored zod schemas with explicit `Wire<...>` types — the exact frozen shape a serde struct set mirrors (~a dozen structs).

### Why (b) wins

- Porting is mechanical: ~a dozen structs mirroring the zod schemas.
- (a) means building the freshness mechanism from scratch: commit a schema artifact, add a `gen-*-schema --check` stale gate, the emitter sibling, typify/progenitor wiring, plus test/snapshot suite — all net-new.
- The evolution story for (b) fits existing architecture: zod schemas stay the single source of truth; a `verify-type-equiv`-style gate (same pattern as the 13K TS↔docs gate) asserts Rust models match source schemas. A Rust change without a schema change fails like doc drift does.

(a) only wins if the wire surface grows well beyond the chat subset. **Start with (b); revisit (a) if the surface grows.**
