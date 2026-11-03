# CLAUDE.md - Project Guidelines

## No Regressions Policy

- **NEVER** break existing features when implementing new ones.
- Before committing, verify ALL existing features still work — not just the new changes.
- Run the build (`pnpm run compile`) to confirm compilation succeeds.
- When modifying shared code (stores, IPC handlers, types), trace all consumers to ensure nothing breaks.

## Package Management

- This repository uses **pnpm**. Do not use `npm install`, `npm ci`, or `npx` for project workflows.
- The pinned package manager is declared in `package.json` (`packageManager`: `pnpm@10.33.2`).
- Use `pnpm install --frozen-lockfile` for reproducible installs.
- Use `pnpm exec <tool>` instead of `npx <tool>`.
- Keep `pnpm-lock.yaml` committed and do not reintroduce `package-lock.json`.
- pnpm v10 blocks dependency lifecycle scripts unless explicitly allowed; required build-script packages are listed under `pnpm.onlyBuiltDependencies` in `package.json`.
- Standard verification commands:
  - `pnpm exec tsc --noEmit --pretty false`
  - `pnpm run compile`
  - `pnpm run test:sidecar`
  - `pnpm run check:tauri-rust`
  - `pnpm run test:tauri-rust`
  - For local packaging verification without macOS signing/notarization: `pnpm run tauri:build:debug`

## Logging

- **Frontend (renderer)**: Use `window.batAppAPI.debug.log(...)` instead of `console.log()`. This sends logs to the Tauri host logger, which writes to disk.
- **Backend (Tauri/Rust)**: Use the project Rust logging/debug helpers so logs are persisted.
- Do NOT use `console.log()` for debugging — use the logger so logs are persisted and visible in the log file.
- **Log file location**:
  - Tauri writes renderer/Rust logs to `<app-data>/logs/debug.log` and sidecar logs to `<app-data>/logs/sidecar.log`.
  - macOS fresh Tauri install: `~/Library/Application Support/com.tonyq.better-agent-terminal/logs/debug.log`
  - macOS existing Electron migration: `~/Library/Application Support/BetterAgentTerminal/logs/debug.log`
  - Windows fresh Tauri install: `%APPDATA%\com.tonyq.better-agent-terminal\logs\debug.log`
  - Linux fresh Tauri install: `~/.local/share/com.tonyq.better-agent-terminal/logs/debug.log` or the platform-resolved Tauri app data dir.
  - `BAT_TAURI_DATA_DIR` overrides the app data directory in dev/tests.
  - In the app, Settings → Open Logs Folder opens the active logs directory.

## Sub-agent / Active Tasks Tracking

- Sidecar `node-sidecar/src/handlers/claude-send.mjs` mirrors SDK `task_started` / `task_updated` / `task_notification` into `s.activeTasks` and emits `claude:task`; the renderer merges them with Agent/Task `tool_use` blocks in `renderer/src/lib/agent-task-tree.ts`.
- Background tasks (run_in_background Agent/Bash, Monitor, ...) live in `node-sidecar/src/lib/background-tasks.mjs`: SDK `background_tasks_changed` (non-ambient entries) is authoritative; edge events (task_started / task_updated / task_notification, run_in_background tool results) are a fallback until the first level frame, reconciled with `s.activeTasks` at each result. Changes are emitted as `claude:background-tasks {sessionId, tasks}` (a new tracker first sends `[]`) and returned by `claude.getSessionState().backgroundTasks`.
- Only **agent** tasks (`isAgent`: SDK task types `local_agent` / `local_workflow`, or an untyped entry with `subagent_type`) make a turn hold its notification and count in the indicator. Background shells (`local_bash`, e.g. dev servers), monitors and launch-only entries are listed in the Active Tasks bar but never start a hold (some can delay the CLI's `'idle'` while an agent hold waits; the watchdog bounds that).
- `task_notification` ends (emits `claude:task`) only background tasks; foreground tasks keep their tool_result and the turn-end cleanup.
- Completion notification: a completed `claude:turn-end` with no live agents and no held completion notifies on the turn-end itself. Otherwise it carries `pendingBackground > 0` (live agents) or `awaitingIdle: true` and the notification is sent later as `claude:background-settled`. With `session_state_changed` (the sidecar sets `CLAUDE_CODE_EMIT_SESSION_STATE_EVENTS=1`) that happens on `'idle'` with no live agents; an `'idle'` that precedes the result notifies on the turn-end, and a 30 s watchdog covers a missing `'idle'`. Older CLIs use a 10 s grace after the last agent ends with no main-thread frame (`status: requesting`, `thinking_tokens`, stream/assistant/user frames). A new `claude.sendMessage` turn first announces a completion held with no live agents (the CLI sends no `'idle'` between queued turns).
- The host (`src-tauri/src/commands/notification.rs`) holds while `pendingBackground > 0` or `awaitingIdle` and notifies once per `workId`. Work ids are unique across session records and sidecar restarts; the host also forgets the last one on session registration and `claude:session-reset`.
- A CLI that exits or is closed while a completion is held announces it via `claude:background-settled`, except for a user abort (`closeLiveQuery(s, { userAbort: true })`: abort / stop / reset / rest / reload / resume swap).
- `stopTask()` maps a `toolUseId` to its `task_id` and works after the turn ends while background tasks are alive.
- Renderer: `App.tsx` stores the list per terminal (`workspaceStore.setTerminalBackgroundTasks`; panel mount and remote reconnect re-adopt `getSessionState().backgroundTasks`); `ActivityIndicator` shows a slow blue pulse with the background agent count when agents > 0 and the main agent is idle; the Active Tasks bar keeps background agents running and lists other background tasks until they end. Elapsed clocks in the bar tick on their own (`TaskClock`), not by re-rendering the panel.

## React Rendering

- Use `flushSync` from `react-dom` for Agent/Task tool state changes (`setMessages` in `onToolUse` and `onToolResult`) to prevent rendering delays from React 18 batching during streaming.
- Do NOT use `flushSync` for regular tool calls — only for state changes that affect the active tasks bar visibility.

## Status Line

- Our status line implementation is superior to external alternatives (e.g., ccstatusline). Do not replace it.
- 15 configurable items (see `STATUSLINE_ITEMS` in `renderer/src/types/index.ts`) with custom colors, zone alignment, and template-based config.
- Usage polling: Chrome session key (primary, lenient rate limits) → OAuth fallback (strict rate limits).

## Remote State Ownership

- Remote mode is host-owned by default. Except for purely local presentation state, clients do not own remote state.
- Message filters are client-only presentation state and may stay local because they do not change host data.
- All other remote actions must be sent to the host, and the client should render the host response or host broadcast reflection.
- When a remote client requests a mutation, the host applies or rejects it first, then returns the result and broadcasts canonical shared-state changes to every connected client.
- Avoid client-side optimistic final state for remote workflows. Loading/pending UI is fine while waiting for host confirmation.

## Git Workflow

- Do **NOT** auto-create a branch before committing. Commit directly to the current branch (including `main`) unless the user explicitly asks for a branch or PR. This overrides the default "branch first on the default branch" behavior.
- Still only commit/push when the user asks.

## Release

- `package.json` 與 `src-tauri/tauri.conf.json` 的 committed version 固定維持 `0.0.1-dev`。
- 發版時只建立 / push tag；不要 commit release version bump。GitHub Actions 會從 tag 注入實際 build 版本。
- **說法對應**：下面每一組都是指同一個動作，直接執行，不要反問要哪一種。
  - 「發布測試版」/「測試版」/「預覽版」/「發 pre」/ `release a pre tag version` / `release new pre tag version` → **預覽版**流程
  - 「發布正式版」/「正式版」/「發正式」/ `release a tag version` / `release new tag version` → **正式版**流程
- **正式版**: 建立 tag 並 push
  - 最新 tag 是正式版時：遞增 patch 版號。例：`v2.2.27` → `v2.2.28`
  - 最新 tag 是 `-pre` 時：**直接用它的版號，不要再遞增**。例：`v2.2.28-pre.3` → `v2.2.28`
    - `-pre.N` 是「X 的預覽」，所以轉正式就是 X 本身。遞增會跳號並讓那些預覽變成從未發布過的版本。
- **預覽版**: `release new pre tag version` → 基於最新 tag 遞增 patch 版號，加 `-pre.1` 後綴
  - 例：目前 `v2.2.27` → 建立 `v2.2.28-pre.1`
  - 若已有 `v2.2.28-pre.1` → 建立 `v2.2.28-pre.2`
- Tag 含 `-pre` 時 GitHub Release 自動標為 Pre-release，不更新 Homebrew tap
- Tag 不含 `-pre` 時為正式版，更新 Homebrew tap
