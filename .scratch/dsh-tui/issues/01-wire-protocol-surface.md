# Wire protocol surface for an external client

Type: research
Status: resolved
Blocked by: —

## Question

What is the exact wire surface a non-browser external client (a spawned Rust process)
needs to drive the harness exactly like the web client does?

Deliver, with `file:line` evidence from `packages/api/gateway`, `packages/client/connection`,
`packages/sdk`, `packages/host/webserver`:

1. All HTTP paths + methods the browser client calls.
2. The two WebSocket downlink paths and their frame schemas (mux frame, host frame).
3. Any auth / origin / CSRF / upgrade checks that would block or need relaxing for a
   non-browser local client.
4. The JSON-RPC method inventory the web client exercises.
5. The minimum request/response pairs for: session list, session load/resume, turn
   start + streaming, tool result display, approvals, interrupt.

## Answer

Key sources: `packages/client/connection/src/{api-path,index,rpc-host,http-bridge,api-request-trust,websocket-downlink}.ts`, `packages/host/apiproxy/src/{fetch/handler,fetch/client,api/rpc,api/rpc-map,api/events,api/events.schema}.ts`, `packages/host/webserver/src/index.ts`.

### 1. HTTP paths/methods

All POST `application/json`, body = `ClientRequest {type:'client-request', rpcId, method, payload}`; path = method name. **52 unary methods** across `RpcMethodMap` (rpc-map.ts:24-77): `session.*` (12: list/search/create/history/models/selectModel/rename/fork/prompt/attachment/updateQueue/cancel), `subagent.*` (4), `host.*` (5), `workspace.*` (7), `skill.list`, `agentPreset.*` (6), `goal.*` (6), `settings.*` (5), `credentials.*` (3), `llm.*` (3).

- `POST /api/respond` — answers to server-request frames (approval/question); body = `ClientResponse {type:'client-response', rpcId, result}`; returns `RpcReceipt {accepted:true} | {accepted:false, reason:'not-pending'|'bad-response'}`.
- `GET /api/session.export?sessionId=...&includeDescendants=` — host-only ZIP download.
- Responses: `ServerResponse {type:'server-response', rpcId, result:{ok:true,value}|{ok:false,error}}`; business errors are HTTP 200, carrier errors 404/415/400/500.

### 2. WebSocket downlinks

`/api/events.mux` and `/api/events.host` (exact-path upgrades; GET answers `426 Upgrade Required`). Frames = `ServerRequest {type:'server-request', rpcId, method, payload}` JSON text frames. Downlink-only — any client message closes 1008 `'downlink only'`.

- **MuxFrame** (events.ts:69-108): `session/event {sessionId, event, view?}`, `session/subscribed {sessionId,lastSeq}`, `approval/requested` (answerable), `approval/resolved`, `question/requested` (answerable), `question/resolved`, `session/queue`, `session/jobs`, `session/projection {sessionId,key,value,seq}`, `stream/error`.
- **HostFrame** (events.ts:127-155): `host/session-added`, `host/session-removed`, `host/session-status`, `host/agent-error`, `host/workspace-changed|removed|order-changed`, `host/archived-sessions-changed`, `host/remote-event`, `stream/error`.

### 3. Auth / origin / upgrade gates

No tokens/cookies/CSRF. Single gate `isTrustedApiRequest` (api-request-trust.ts:96-123) on every `/api` HTTP request and both WS upgrades: (a) Host header must be loopback or match `trustedHosts` (DNS-rebinding fence); (b) `sec-fetch-site: cross-site` refused; (c) attached Origin must equal Host, absent Origin fine. **A spawned local Rust TUI passes already** — set `Host: 127.0.0.1:<port>`, no cross-site Fetch-Metadata. No relaxation needed.

**Loopback-pinned privileged methods** (connection index.ts:89-119): `agentPreset.read/copy/openDocument/remove`, `host.pickDirectory/openPath`, `settings.describe/openDocument/update/replace/mutate`, `credentials.describe/set/unset`, `llm.discoverModels` — 403 unless loopback, even on LAN. Spawned TUI on same machine satisfies loopback; remote clients need real auth (out of scope).

### 4. Method inventory

The wire is **not JSON-RPC** — it's the custom four-quadrant RPC (rpc.ts). The web client exercises all 52 unary methods + `respond`. `packages/sdk` JSON-RPC is a separate surface for programmatic/ACP clients — not the web-client wire.

### 5. Minimum request/response pairs (all POST /api/<method> unless noted)

- **Session list**: `session.list {cursor?}` → `{items: SessionSummary[]}`.
- **Session load/resume**: `session.create {workspaceId?|cwd?, sessionId?, agentPreset?}` → `{sessionId, agentPreset?}` (idempotent by sessionId); `session.history {sessionId, beforeSeq?, maxMessages?}` → `{events[], hasMore, projections?}`; open `/api/events.mux`; baseline = `session/subscribed` then replay of pending approval/question frames.
- **Turn start + streaming**: `session.prompt {sessionId, mode:'queue'|'steer', content[], clientTimeZone?}` → `{accepted:true, command?}`. Output streams over mux as `session/event` frames (chunk/tool/result events).
- **Tool result display**: mux `session/event` with optional `view: ToolEventView` render intent.
- **Approvals**: request = mux `approval/requested`; answer = `POST /api/respond` with result `{ok:true, value:{sessionId, approvalId, outcome:'allowed-once'|'rejected'}}`; final = `approval/resolved`.
- **Interrupt**: `session.cancel {sessionId}` → `{accepted:true}`; subagents `subagent.interrupt`.

### Implication for the TUI

The chat-loop subset is small and fully specified: 1 POST path pattern, 2 WS downlinks, ~20 relevant methods, 4 answerable frame types. This is the contract the Rust client implements (see Typert → Rust codegen for how bindings are maintained).
