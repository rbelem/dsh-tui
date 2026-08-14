# Rendering architecture

Type: grilling
Status: resolved
Blocked by: 01, 03, 04

## Question

What is the TUI's rendering + update architecture?

Decide:

- How the web UI's virtualized chat-node tree maps onto terminal widgets: assistant
  streaming rows, tool rows with subagent recursion, diff views, approvals, retry
  and feedback affordances.
- The state → draw loop: event ingestion (WebSocket frames) → app state → ratatui
  draw; where scrolling/virtualization lives; how partial streaming updates render
  without flicker or full redraws.
- Input model: paste handling, IME/CJK input, keymap design for affordances that are
  click/hover in the web UI.

## Answer

Settled across three grilling rounds (Q1–Q19); per-feature acceptance targets remain ticket 07.

### State model (Q1)
Rust `SessionStore` mirroring the web's projection layer: consumes mux frames (`session/event`, `session/projection`, `session/subscribed`), per-session event buffers + seq bookkeeping (higher-seq-wins), derives the chat-node tree. Testable via TestBackend without a terminal.

### Session topology (Q2, amended — follow web)
Workspace browser + sidebar: grouped/flat session list, search (metadata + content), archive/rename/fork/delete, new-session hero with workspace picker + directory flow. Single active session rendered; background sessions keep their buffers.

### Threading (Q3)
tokio for the wire: WS reader → mpsc channel of parsed frames. Single main thread owns store + draw; draw on coalesced batches (~16ms tick + wakeup on frame arrival) to avoid flicker/terminal flood; crossterm events polled on the main loop.

### Chat-list rendering (Q4)
Cached per-row rendered lines + dirty set keyed by last-applied seq; a streaming chunk marks one row dirty; virtualization = viewport offset into the cached row array.

### Streaming markdown (Q5)
Re-parse the row's accumulated text on each chunk (pulldown-cmark), cache when idle; rows are bounded so O(n²) is invisible via the Q4 dirty set. Revisit only if profiling says otherwise.

### Approval/question UX (Q6)
Full-screen takeover (mirrors web's composer takeover — no pointer needed); non-blocking toasts for resolved states; plan-review cards (Approve/Refuse/Chat) render as the takeover body.

### Input pipeline (Q7)
`/` + `@` caret detection with a grouped candidate menu widget; command execution rides `command.execute` over the same RPC.

### History paging (Q8)
Auto load-more at 200 rows from the top; one-shot request dedup; contiguous loaded window with an `oldestSeq` watermark and a paging state row while in flight; retry/compaction nodes reconstruct from the window once loaded.

### Store bounds (Q9)
Per-session cap ~5k events, LRU evict beyond the window; non-active sessions unload event buffers (reload via `session.history` on switch); projections are cheap and kept per session.

### Resize (Q10)
Full re-render on terminal width change — rare, one-time cost, no incremental reflow machinery.

### Fold state (Q11)
Fold state lives in the store keyed by node id (survives re-render + session switch). Defaults mirror web: context-injection + compaction collapsed; tool rows show a one-line summary (lifecycle icon + title) expanded by default; assistant rows always expanded while streaming.

### Markdown surface (Q12)
CommonMark + tables + strikethrough; syntect-highlighted code fences; links open via keybind on the focused row (no hover); inline images render via ratatui-image when a protocol is available, else `[image]` placeholder (acceptance in 07).

### Approval actions (Q13)
Keys: allow-once / reject / allow-always-preset (permissions projection). Render toolName, reason, call context. Full-access risk-ack is an explicit second keypress (parity with web).

### Composer (Q14, amended)
Multi-line; Enter submits, **Shift+Enter newline**; bracketed paste inserts verbatim; `/`+`@` menus overlay above the composer; queue toggle showing `session/queue` items; **toggleable vim mode** (`Ctrl+J`, off by default, persists as a setting; remaps composer editing h/l/0/$/i/a/dd when on).

### Navigation (Q15)
Vim-style j/k, g/G, Ctrl+d/u half-page; Tab cycles chat → composer → sidebar; `?` help overlay; Esc closes overlays/takeovers; Ctrl+c cancels the running turn (`session.cancel`).

### Command entries (Q16, Q17 — two)
`/`-menu: source-grouped (commands, skills, subagents, permissions), inserts into composer. Plus a global launcher: `Ctrl+P`, fuzzy search over all client commands + settings actions, dispatches immediately, no leading-input state.

### Out-of-band placement (Q19, amended — follow web)
Jobs popover from the session header (`session/jobs` frames); queue dock above the composer; trajectory as a view-ring tab — a second cached-row renderer over the same store (same machinery as the chat list); no persistent third pane.
