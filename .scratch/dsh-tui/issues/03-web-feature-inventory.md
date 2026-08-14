# Web UI feature inventory

Type: research
Status: resolved
Blocked by: —

## Question

What exactly is "every feature of the current web UI"?

Walk `packages/client/**` (ui-* modules, runtime, web) and `apps/web`. Produce a
definitive inventory table — the parity baseline for Rendering architecture,
Parity definition, and Testing strategy:

- Per feature/module: name, what it does, which protocol events/endpoints it consumes
  (where findable), and whether it is a chat-loop feature (conversation streaming,
  tool rows, approvals, input, session list/titles) or an out-of-band surface
  (settings, jobs, workspace, attachments, workflow, theme, i18n, trajectory).
- All chat node kinds (assistant, tool, retry, turn-tail, command, compaction, …)
  and their interactive affordances (expand/collapse, diff view, retry, feedback).
- The settings surfaces the web UI exposes (general, models, plugins, plugin inventory,
  permission presets, …).

Completeness over prose: a compact table of rows, not essays.

## Answer

Definitive inventory of the web UI, from READMEs + src indexes across `packages/client/**` and `apps/web`. This is the parity baseline for Rendering architecture, Parity definition, and Testing strategy.

### Feature/module inventory

| module | what it does | protocol consumed | surface |
|---|---|---|---|
| `runtime` | Session/Workspace object layer, event window+paging, slots inject, bindSettingsScope (revisioned set/unset), projections store (higher-seq-wins), pending-interaction classifier, retry/assistant/turn-error/turn-max-tokens Definitions, session fork, model-selection snapshot, queue projection, jobs mirror, session title projection, search action | `session.*`, `workspace.*`, `session/projection`, `session/jobs`, `session/queue`, `session/title`, `llm/retry`, `host/*` frames, `credentials.*`, `settings.*` | backbone |
| `connection` | Wire consumer: HTTP POST unary + WebSocket downlinks (`events.mux`, `events.host`), loopback state, `/api` browser-trust fence, generation-scoped reconnect | all RPC via api client; `host.describe`, `settings.*`, `credentials.*`, `agentPreset.*` | backbone |
| `web` / `web-react` / `modules` | Two-stage boot, shell title projection, React slot renderer/hooks | entry graph (`__DSH_BOOT__`) | backbone |
| `locale` | i18n zh/en dictionaries + locale-settings | `locale` settings | out-of-band |
| `ui-conversation` | Conversation domain: skeleton (header/tabs/composer/empty), chat view, composer dock, input dock (Queue+Todo), details shell, approvals takeover, plan chip seat, model seat, image intake/drop overlay, ContextMeter, stats line, Command launcher, view ring | `useProjection`, `session.prompt` (queue/steer), `command.execute`, `conversation.*` node defs | chat-loop |
| `ui-tool` | Tool rows: recursive root/child tree, render-intent cards (terminal/read/diff/search/web), generic classification, per-name atomic views via keyed `tool.call.toolview` slot | reads frozen call/result slice | chat-loop |
| `ui-sidebar` | Sidebar shell: wordmark, New Session, collapse rail, scroll-aware seat, bottom Settings seat | `startSession` | out-of-band (session list chrome) |
| `ui-workspace` | Workspace browser + picker (sidebar + new-session hero): grouped/flat sessions, reorder, search (metadata+content), archive/rename/fork/delete, session rows w/ status+subagent lineage, directory-flow hole | `workspace.*`, `session.search`, `archiveSession`, `sessions.open` | out-of-band |
| `ui-layout` | 3-column AppFrame, drag handles, concession chain, panel-geometry service, theme DOM presenter | `ctx.theme` snapshots | chrome |
| `ui-settings` | Settings domain base: `ctx.settingsScope`, slot types (`settings.trigger/header/close/action/section/plugins.tab/onboarding`) | `settings.*`, `settings/document-updated`, `connection/reset` | out-of-band |
| `ui-settings-general` | Settings shell (nav, chrome, trigger, "Open configuration file"), General section | `settings.describe`, `settings.openDocument` | out-of-band |
| `ui-settings-models` | Models page + onboarding (welcome notice + DeepSeek key step): provider editor cards (api key, baseURL, model list, display name/protocol), llm-pi-ai catalog interrogation | `llm.providers`, `settings.describe`, `credentials.describe/set`, `llm.discoverModels`, `settings.mutate` | out-of-band |
| `ui-settings-plugins` | Plugins section + Plugin-config tab (bash, agent-loop, web-search-deepseek cards) | settings scope per plugin namespace, `credentials/updated` | out-of-band |
| `ui-settings-plugin-inventory` | Read-only Plugin list tab: searchable catalog, enablement/status dots, Loader tree config | `pluginInventory.list()` | out-of-band |
| `ui-agent-preset` | Agent-preset surfaces: General row, new-session chip, header label, management section (copy/delete/default/viewer) | `agentPreset.list/read/copy/remove/openDocument`, `agent-presets` settings, `agent-preset/selected` | out-of-band |
| `ui-permission-presets` | Permission presets: General row (risk-ack for Full access) + `/permission` picker decoration | `settings.mutate`, `/permission`, `permissions` projection | both |
| `ui-jobs` | Background-job list in session header: live/settled rows, elapsed clock, badge | `jobsBySession` from `session/jobs` | out-of-band |
| `ui-theme` | Theme service (light/dark/system), `--dsw-*` token sheets, scrollbar contract, boot preference embed | `ui-theme.preference` settings, `theme/change` | out-of-band |
| `ui-trajectory` | Turn-aware event ledger view: selectable User/Assistant/Tool/Subtool, timeline Overview, token/timing inspector, search, fold, paging | its own Definitions over Session window | out-of-band |
| `ui-attachment` | Pure atoms: draft-image rail, chat-image gallery/lightbox, full-page drop overlay | image limits projection | chat-loop |
| `ui-user-questions` | Question composer takeover: progress nav, single/multi-select, custom answers, plan-review intent card (Approve/Refuse/Chat about it) | `question` RPC, `question/resolved` | chat-loop |
| `ui-message-feedback` | Like/Dislike + optional note per finalized assistant message | `messageFeedback.list/put/delete` (CAS versioned) | chat-loop |
| `ui-model-selection` | Model seat: provider-grouped picker | `modelSelection` projection | chat-loop |
| `ui-input-trigger` | `/` `@` caret detection pipeline, grouped candidate menu, pick routing, launcher | `inputTriggers` controller | chat-loop |
| `ui-commands` | Client command API: `/` command source, directory cache, execute/popupSelect/leadingInput dispatch, decorations | `command.list`, `command.execute`, `commands/change` | chat-loop |
| `ui-goal` | GoalBar dock (edit/pause/resume/clear) + `/goal` command-input Chat Node | `goals.*` RPC, `command/run` projection | both |
| `ui-skill` | `/` skill source (menu candidates) + skill tool row (Instructions card) | `skill.list`, `tool.call.toolview` | chat-loop |
| `ui-plan` | Plan-mode status chip (Plan × toggle) | `plan` projection, `command.execute` `/plan off` | chat-loop |
| `ui-subagent` | Subagent catalog tree (header), read-only composer for one-shot/continuable, `@` reference source | `subagentsByParent`, `openSubagent`, `subagent.prompt/interrupt` | both |
| `ui-workflow-run` | Durable top-level workflow runs as Chat nodes: run/phase/member tree, status, child navigation | `tool-workflow/*` events | chat-loop |
| `ui-deliverables` | Produced-files row in turn-tail + clickable inline-code file mentions; system-prompt guidance section | mutation `locations`, `openFile` | chat-loop |
| `ui-directory-picker-native` / `-browse` | Directory picking flows (OS chooser / in-app Miller-column dialog) | `workspaces.pickDirectory`, `host.listDirectory/createDirectory` | out-of-band |
| `ui-primitives` | Shared atoms: Button/Input/Menu/Modal/Pill/Toast/Tooltip/HoverCard/StateDot/DisclosureRow, DiffBlock/ReadBlock/SearchBlock/TerminalBlock/WebBlock/JsonTree/RiskConfirmation/Markdown, ansi, icons | — | chrome |
| `ui-slots` | Slot registry pure core: register, four-share props, chain-kind routing, store seats | — | chrome |

### Chat node kinds (affordances)

- **user bubble** — clock, copy; no branch (decision); no edit.
- **steering bubble** — user-style bubble at tail while pending; copy, clock.
- **context-injection / recalled-session node** — default-collapsed disclosure (`上下文注入`/`跨会话召回` + producer name), expand to ≤141px scroll body (instructions/catalog/opaque).
- **assistant** — streaming; IconActions row (copy/clock/branch) on finalized closing message of ended turn; branch forks + opens child; Think row collapsed w/ live-reasoning tail scroll.
- **tool-call** — recursive root/child tree; expand/collapse; render-intent cards (terminal, read, diff, search, web); per-name atomic views; **Inspect** affordance; open-file; lifecycle (running/success/failed/interrupted).
- **turn-tail / produced-files row** — between closing body and footer; file chips (cap 6) + "Show in folder"; openFile.
- **retry row** — stable muted row across retry turns, countdown, shimmer, "inspect" delay/failure; shows max or ∞; cancelled/completed states.
- **turn-error / turn-max-tokens node** — inline warning notice.
- **command node** — generic result row; `/goal` emits a `command-input` user-style bubble before it; `/compact` folds into compaction checkpoint.
- **compaction checkpoint** — collapsed row at flow position; expand reveals replaced-item/estimated-token counts + summary.
- **workflow-run node** — run/phase/member tree; disclosure per level; running members navigate into child session.
- **approval / question / plan-review** — composer takeover (not display placeholders).

### Settings surfaces

- **General** (`settings.general.item` slot): permission preset row, agent-preset default row, Language row, Appearance row, busyEnter preference, "Open configuration file" action.
- **Models** (`ui-settings-models`): provider editor cards, API-key (write-only via `credentials.set`), baseURL, model catalog/context/maxTokens, add custom provider, llm-pi-ai model-list + endpoint interrogation.
- **Plugins** section: `Plugin configuration` tab (per-plugin config cards w/ reset-to-default) + `Plugin list` inventory tab (read-only catalog).
- **Permission presets** (dynamic `defaultPreset` enum).
- **Onboarding** steps (welcome notice, DeepSeek credential).
- Plus **session header label**, **plan**, **i18n**, **theme** settings.

### Global affordances

- **Sidebar/session list** (ui-workspace): grouped/flat, search, archive/rename/fork/delete, unread/status dots, subagent lineage aggregation.
- **Workspace picker** (new-session hero + sidebar), directory-flow add.
- **Theme**: light/dark/system, token sheets, scrollbar behavior.
- **i18n**: zh/en (`locale` module, per-plugin dictionaries).
- **Trajectory view** (tab in view ring), **jobs** (session header popover).
- **View ring tabs**: Chat, Trajectory, others. **Details panel**: `conversation.details.tool` (raw-result fallback; no entry point in assembled app).

### Key parity notes for the TUI

- Settings RPCs are loopback-only (remote browsers inert).
- Feedback is a CAS-versioned sidecar not in the session log.
- Trajectory is a separate Definitions target over the same window.
- i18n strings come from per-plugin locale dictionaries.
