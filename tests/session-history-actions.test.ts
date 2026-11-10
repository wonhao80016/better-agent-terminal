// Run with: pnpm run test:session-history
import * as assert from 'node:assert/strict'

type TauriInvoke = <T>(cmd: string, args?: Record<string, unknown>) => Promise<T>

const invokedCommands: string[] = []
const savedSettings: string[] = []
let storedSettings: string | null = null
;(globalThis as { window?: unknown }).window = {
  __TAURI_INTERNALS__: {
    invoke: (async <T>(cmd: string, args?: Record<string, unknown>) => {
      invokedCommands.push(cmd)
      if (cmd === 'workspace_save') return true as T
      if (cmd === 'app_set_dock_badge') return undefined as T
      if (cmd === 'settings_save') {
        savedSettings.push(String(args?.data))
        return undefined as T
      }
      if (cmd === 'settings_load') return storedSettings as T
      throw new Error(`unexpected invoke: ${cmd}`)
    }) satisfies TauriInvoke,
  },
  batAppAPI: { debug: { isDebugMode: false } },
  location: { search: '' },
}

type Entry = import('../renderer/src/types/session-history.ts').SessionHistoryEntry
type Group = import('../renderer/src/types/session-history.ts').SessionHistoryGroup
type Result = import('../renderer/src/types/session-history.ts').SessionHistoryResult

function entry(overrides: Partial<Entry>): Entry {
  return {
    agent: 'claude',
    id: '00000000-0000-0000-0000-000000000001',
    title: 'Untitled',
    firstPrompt: null,
    lastReply: null,
    messageCount: 1,
    subagentCount: 0,
    model: null,
    gitBranch: null,
    cwd: 'C:\\Repo',
    cwdExists: true,
    startedAt: '2026-11-01T00:00:00Z',
    updatedAt: '2026-11-01T00:00:00Z',
    groupKey: 'repo:c:\\repo',
    skippedLines: 0,
    interactive: true,
    archived: false,
    ...overrides,
  }
}

function group(overrides: Partial<Group>): Group {
  return {
    key: 'repo:c:\\repo',
    kind: 'repo',
    name: 'Repo',
    rootPath: 'C:\\Repo',
    normalizedRootPath: 'c:\\repo',
    worktreeCount: 0,
    sessionCount: 1,
    latestUpdatedAt: '2026-11-01T00:00:00Z',
    ...overrides,
  }
}

async function main() {
  const actions = await import('../renderer/src/utils/session-history-actions.ts')
  const { workspaceStore } = await import('../renderer/src/stores/workspace-store.ts')
  const seed = (data: unknown) => (workspaceStore as unknown as {
    applySerializedData(data: string): void
  }).applySerializedData(JSON.stringify(data))

  // normalizeHostPath — Windows paths are always case-insensitive.
  const { normalizeHostPath } = actions
  assert.equal(normalizeHostPath('C:\\Users\\Me\\Repo\\', true), 'c:\\users\\me\\repo')
  assert.equal(normalizeHostPath('C:/Users//Me/Repo/', false), 'c:\\users\\me\\repo')
  assert.equal(normalizeHostPath('C:\\', true), 'c:\\')
  assert.equal(normalizeHostPath('C:/', true), 'c:\\')
  assert.equal(normalizeHostPath('D:', true), 'd:\\')
  assert.equal(normalizeHostPath('\\\\Server\\Share\\\\Dir\\', true), '\\\\server\\share\\dir')
  assert.equal(
    normalizeHostPath('D:\\codebase\\Repo', true),
    normalizeHostPath('d:/CODEBASE/repo/', true),
  )
  // POSIX — case preserved unless the host FS is case-insensitive.
  assert.equal(normalizeHostPath('/Users//me/Repo/', false), '/Users/me/Repo')
  assert.equal(normalizeHostPath('/Users/me/Repo', true), '/users/me/repo')
  assert.equal(normalizeHostPath('/', false), '/')
  assert.equal(normalizeHostPath('//', false), '/')
  assert.notEqual(normalizeHostPath('/a/Repo', false), normalizeHostPath('/a/repo', false))

  // Search — every whitespace-separated term must match some field, case-insensitively.
  const searchable = entry({
    title: 'Fix duplicate windows',
    firstPrompt: 'Why does BAT open two windows?',
    lastReply: 'Guarded the reopen handler.',
    gitBranch: 'fix-dup-window',
    model: 'claude-opus-5-5',
    cwd: 'D:\\codebase\\BAT',
    id: '3f2a9c1e-7b4d-4e0a-9c2f-1d8e6a5b4c3d',
  })
  assert.equal(actions.matchesSessionQuery(searchable, ''), true)
  assert.equal(actions.matchesSessionQuery(searchable, '   '), true)
  assert.equal(actions.matchesSessionQuery(searchable, 'DUPLICATE'), true)
  assert.equal(actions.matchesSessionQuery(searchable, 'duplicate  reopen'), true)
  assert.equal(actions.matchesSessionQuery(searchable, 'fix-dup opus codebase 3f2a9c1e'), true)
  assert.equal(actions.matchesSessionQuery(searchable, 'duplicate codex'), false)
  assert.equal(actions.matchesSessionQuery(entry({ title: 'x', cwd: null }), 'codebase'), false)

  assert.equal(actions.sessionResumeCommand({ agent: 'claude', id: 'abc' }), 'claude --resume abc')
  assert.equal(actions.sessionResumeCommand({ agent: 'codex', id: 'abc' }), 'codex resume abc')
  assert.equal(
    actions.sessionResumeCommand({ agent: 'codex', id: '0aaaaaaa-0000-7000-8000-000000000001' }),
    'codex resume 0aaaaaaa-0000-7000-8000-000000000001',
  )

  // Session ids — anything but a plain token never becomes a copyable shell command.
  for (const valid of ['3f2a9c1e-7b4d-4e0a-9c2f-1d8e6a5b4c3d', 'a', 'A1._-z']) {
    assert.equal(actions.isValidSessionId(valid), true, valid)
  }
  for (const invalid of ['', '-rf', '.hidden', '_x', 'abc;rm -rf ~', 'a b', 'a&b', '$(id)', 'a`b`', 'a|b', 'a/b', 'a\\b', 'été', 'a\nb']) {
    assert.equal(actions.isValidSessionId(invalid), false, JSON.stringify(invalid))
    assert.equal(actions.sessionResumeCommand({ agent: 'claude', id: invalid }), null, JSON.stringify(invalid))
    assert.equal(actions.sessionResumeCommand({ agent: 'codex', id: invalid }), null, JSON.stringify(invalid))
  }

  // Escape — the trash dialog handles it in the capture phase, before window bubble listeners (agent panels) run.
  type Listener = (event: Event) => void
  const registered: Array<{ listener: Listener; capture: boolean }> = []
  const fakeWindow = {
    addEventListener: (_type: string, listener: Listener, capture?: boolean | AddEventListenerOptions) => {
      registered.push({ listener, capture: capture === true })
    },
    removeEventListener: (_type: string, listener: Listener, capture?: boolean | EventListenerOptions) => {
      const index = registered.findIndex(r => r.listener === listener && r.capture === (capture === true))
      if (index >= 0) registered.splice(index, 1)
    },
  }
  const dispatchKey = (key: string) => {
    let stopped = false
    const event = {
      key,
      defaultPrevented: false,
      preventDefault() { this.defaultPrevented = true },
      stopImmediatePropagation() { stopped = true },
    }
    const ordered = [...registered.filter(r => r.capture), ...registered.filter(r => !r.capture)]
    for (const { listener } of ordered) {
      if (stopped) break
      listener(event as unknown as Event)
    }
    return event
  }
  const panelKeys: string[] = []
  fakeWindow.addEventListener('keydown', event => { panelKeys.push((event as KeyboardEvent).key) })
  let closed = 0
  const unbindEscape = actions.listenForDialogEscape(fakeWindow, () => { closed++ })
  assert.equal(registered.filter(r => r.capture).length, 1, 'dialog listens in the capture phase')
  const escape = dispatchKey('Escape')
  assert.equal(closed, 1)
  assert.equal(escape.defaultPrevented, true)
  assert.deepEqual(panelKeys, [], 'Escape must not reach the agent panel while the dialog is open')
  dispatchKey('a')
  assert.deepEqual(panelKeys, ['a'], 'other keys still reach window listeners')
  unbindEscape()
  dispatchKey('Escape')
  assert.equal(closed, 1)
  assert.deepEqual(panelKeys, ['a', 'Escape'], 'a closed dialog no longer intercepts Escape')
  assert.equal(actions.sessionAgentPreset('claude'), 'claude-code')
  assert.equal(actions.sessionAgentPreset('codex'), 'codex-agent')
  assert.equal(actions.canAddGroupAsWorkspace(group({ kind: 'missing' })), false)
  assert.equal(actions.canAddGroupAsWorkspace(group({ kind: 'unknown', rootPath: null })), false)
  assert.equal(actions.canAddGroupAsWorkspace(group({ kind: 'dir' })), true)

  // View — filters, existing-workspace detection, sorting, trailing missing groups.
  const repo = group({})
  const other = group({ key: 'dir:/tmp/other', kind: 'dir', name: 'other', rootPath: 'D:\\Other', normalizedRootPath: 'd:\\other' })
  const missing = group({ key: 'dir:d:\\gone', kind: 'missing', name: 'gone', rootPath: 'D:\\Gone', normalizedRootPath: 'd:\\gone' })
  const result: Result = {
    sessions: [
      entry({ id: 'm1', groupKey: missing.key, updatedAt: '2026-11-03T00:00:00Z', cwdExists: false }),
      entry({ id: 'r1', groupKey: repo.key, updatedAt: '2026-11-02T00:00:00Z', messageCount: 3 }),
      entry({ id: 'o1', agent: 'codex', groupKey: other.key, updatedAt: '2026-11-01T12:00:00Z', messageCount: 50 }),
      entry({ id: 'r2', groupKey: repo.key, updatedAt: '2026-10-01T00:00:00Z', startedAt: '2026-11-05T00:00:00Z', messageCount: 9 }),
    ],
    groups: [repo, other, missing],
    scan: {
      fileCount: 4, parsedCount: 4, cacheHits: 0, failedCount: 0, skippedLines: 0,
      durationMs: 1, scannedAt: '2026-11-02T00:00:00Z', gitAvailable: true,
    },
    caseInsensitivePaths: true,
  }
  const workspaces = [{ id: 'ws-repo', name: 'Repo', folderPath: 'c:/repo/', createdAt: 1 }]
  const filters = { query: '', agent: 'all' as const, sort: 'updated' as const, onlyNotAdded: false }

  let view = actions.buildSessionHistoryView(result, filters, workspaces)
  assert.deepEqual(view.map(v => v.group.key), [repo.key, other.key, missing.key], 'missing groups stay last')
  assert.deepEqual(view[0].sessions.map(s => s.id), ['r1', 'r2'])
  assert.equal(view[0].workspace?.id, 'ws-repo', 'existing workspace detected case-insensitively')
  assert.equal(view[1].workspace, undefined)

  view = actions.buildSessionHistoryView(result, { ...filters, onlyNotAdded: true }, workspaces)
  assert.deepEqual(view.map(v => v.group.key), [other.key, missing.key])

  view = actions.buildSessionHistoryView(result, { ...filters, agent: 'codex' }, workspaces)
  assert.deepEqual(view.map(v => v.group.key), [other.key])

  view = actions.buildSessionHistoryView(result, { ...filters, sort: 'messages' }, workspaces)
  assert.deepEqual(view.map(v => v.group.key), [other.key, repo.key, missing.key])
  assert.deepEqual(view[1].sessions.map(s => s.id), ['r2', 'r1'])

  view = actions.buildSessionHistoryView(result, { ...filters, sort: 'started' }, workspaces)
  assert.deepEqual(view[0].sessions.map(s => s.id), ['r2', 'r1'])

  view = actions.buildSessionHistoryView(result, { ...filters, query: 'nothing-matches' }, workspaces)
  assert.deepEqual(view, [])

  // POSIX host with case-sensitive FS must not merge differently-cased folders.
  const posixGroup = group({ key: 'dir:/srv/Repo', kind: 'dir', rootPath: '/srv/Repo', normalizedRootPath: '/srv/Repo' })
  assert.equal(actions.findWorkspaceForGroup([{ id: 'w', name: 'r', folderPath: '/srv/repo', createdAt: 1 }], posixGroup, false), undefined)
  assert.equal(actions.findWorkspaceForGroup([{ id: 'w', name: 'r', folderPath: '/srv/Repo/', createdAt: 1 }], posixGroup, false)?.id, 'w')

  // ensureWorkspace — existing workspace is reused and activated, not duplicated.
  seed({
    workspaces: [
      { id: 'ws-a', name: 'A', folderPath: 'D:\\A', createdAt: 1 },
      { id: 'ws-repo', name: 'Repo', folderPath: 'c:/REPO', createdAt: 2, group: 'Work' },
    ],
    activeWorkspaceId: 'ws-a',
    activeTerminalId: null,
    terminals: [],
  })
  const reused = actions.ensureWorkspace(workspaceStore, repo, true)
  assert.equal(reused?.id, 'ws-repo')
  assert.equal(workspaceStore.getState().workspaces.length, 2)
  assert.equal(workspaceStore.getState().activeWorkspaceId, 'ws-repo')

  // ensureWorkspace — a new workspace joins the active sidebar group.
  ;(workspaceStore as unknown as { activeGroup: string | null }).activeGroup = 'Work'
  const added = actions.ensureWorkspace(workspaceStore, other, true)
  assert.ok(added)
  const addedState = workspaceStore.getState().workspaces.find(w => w.id === added!.id)
  assert.equal(addedState?.folderPath, 'D:\\Other')
  assert.equal(addedState?.name, 'other')
  assert.equal(addedState?.group, 'Work')
  assert.equal(workspaceStore.getState().activeWorkspaceId, added!.id)
  assert.equal(actions.ensureWorkspace(workspaceStore, other, true)?.id, added!.id, 'second call dedupes')
  assert.equal(workspaceStore.getState().workspaces.length, 3)

  ;(workspaceStore as unknown as { activeGroup: string | null }).activeGroup = null
  const ungrouped = actions.ensureWorkspace(workspaceStore, group({ key: 'dir:d:\\x', kind: 'dir', name: 'x', rootPath: 'D:\\X', normalizedRootPath: 'd:\\x' }), true)
  assert.equal(workspaceStore.getState().workspaces.find(w => w.id === ungrouped?.id)?.group, undefined)
  assert.equal(actions.ensureWorkspace(workspaceStore, missing, true), null, 'missing folders are never added')
  assert.equal(actions.ensureWorkspace(workspaceStore, group({ key: 'unknown', kind: 'unknown', rootPath: null, normalizedRootPath: null }), true), null)
  assert.equal(workspaceStore.getState().workspaces.length, 4)

  // resumeInWorkspace — creates an agent terminal in the session cwd with the sdk session id.
  seed({
    workspaces: [{ id: 'ws-a', name: 'A', folderPath: 'D:\\A', createdAt: 1 }],
    activeWorkspaceId: 'ws-a',
    activeTerminalId: null,
    terminals: [],
  })
  const worktreeEntry = entry({ id: 'sess-claude', cwd: 'C:\\Repo\\.bat-worktrees\\feat', groupKey: repo.key })
  const created = actions.resumeInWorkspace(workspaceStore, worktreeEntry, repo, true)
  assert.equal(created.kind, 'created')
  const state = workspaceStore.getState()
  const createdTerminal = state.terminals.find(t => created.kind === 'created' && t.id === created.terminalId)
  assert.ok(createdTerminal)
  assert.equal(createdTerminal!.agentPreset, 'claude-code')
  assert.equal(createdTerminal!.cwd, 'C:\\Repo\\.bat-worktrees\\feat')
  assert.equal(createdTerminal!.sdkSessionId, 'sess-claude')
  assert.equal(state.focusedTerminalId, createdTerminal!.id)
  const repoWorkspace = state.workspaces.find(w => w.folderPath === 'C:\\Repo')
  assert.equal(createdTerminal!.workspaceId, repoWorkspace?.id)
  assert.equal(state.activeWorkspaceId, repoWorkspace?.id)

  const codexCreated = actions.resumeInWorkspace(workspaceStore, entry({ id: 'sess-codex', agent: 'codex' }), repo, true)
  assert.equal(codexCreated.kind, 'created')
  const codexTerminal = workspaceStore.getState().terminals.find(t => codexCreated.kind === 'created' && t.id === codexCreated.terminalId)
  assert.equal(codexTerminal?.agentPreset, 'codex-agent')
  assert.equal(codexTerminal?.sdkSessionId, 'sess-codex')
  assert.equal(workspaceStore.getState().workspaces.filter(w => w.folderPath === 'C:\\Repo').length, 1)

  // resumeInWorkspace — an already open session is focused instead of duplicated.
  workspaceStore.setFocusedTerminal(null)
  workspaceStore.setActiveWorkspace('ws-a')
  const terminalCount = workspaceStore.getState().terminals.length
  const focused = actions.resumeInWorkspace(workspaceStore, worktreeEntry, repo, true)
  assert.deepEqual(focused, { kind: 'focused', terminalId: createdTerminal!.id })
  assert.equal(workspaceStore.getState().terminals.length, terminalCount)
  assert.equal(workspaceStore.getState().focusedTerminalId, createdTerminal!.id)
  assert.equal(workspaceStore.getState().activeWorkspaceId, repoWorkspace?.id)

  // resumeInWorkspace — a missing or unknown cwd is a no-op.
  const before = workspaceStore.getState()
  assert.deepEqual(
    actions.resumeInWorkspace(workspaceStore, entry({ id: 'no-cwd', cwd: null, cwdExists: false }), repo, true),
    { kind: 'unavailable', reason: 'no-cwd' },
  )
  assert.deepEqual(
    actions.resumeInWorkspace(workspaceStore, entry({ id: 'gone', cwd: 'D:\\Gone', cwdExists: false }), missing, true),
    { kind: 'unavailable', reason: 'cwd-missing' },
  )
  assert.equal(workspaceStore.getState(), before, 'unavailable resume must not touch the store')
  assert.equal(actions.resumeUnavailableReason(entry({ cwd: null })), 'no-cwd')
  assert.equal(actions.resumeUnavailableReason(entry({ cwdExists: false })), 'cwd-missing')
  assert.equal(actions.resumeUnavailableReason(entry({})), null)

  // An unconfirmed cwd in a dir group was not reported missing by the host (timeout or access error).
  const offline = group({ key: 'dir:\\\\nas\\share\\proj', kind: 'dir', name: 'proj', rootPath: '\\\\nas\\share\\proj', normalizedRootPath: '\\\\nas\\share\\proj' })
  const offlineEntry = entry({ id: 'offline', cwd: '\\\\nas\\share\\proj', cwdExists: false, groupKey: offline.key })
  assert.equal(actions.resumeUnavailableReason(offlineEntry, offline), 'cwd-unverified')
  assert.equal(actions.resumeUnavailableReason(entry({ cwdExists: false }), missing), 'cwd-missing')
  assert.equal(actions.resumeUnavailableReason(entry({ cwdExists: false }), repo), 'cwd-missing')
  assert.deepEqual(
    actions.resumeInWorkspace(workspaceStore, offlineEntry, offline, true),
    { kind: 'unavailable', reason: 'cwd-unverified' },
  )
  assert.equal(workspaceStore.getState(), before, 'unverified resume must not touch the store')

  // Detached workspaces — the main window does not render them, so nothing is created or activated there.
  seed({
    workspaces: [
      { id: 'ws-a', name: 'A', folderPath: 'D:\\A', createdAt: 1 },
      { id: 'ws-repo', name: 'Repo', folderPath: 'C:\\Repo', createdAt: 2 },
    ],
    activeWorkspaceId: 'ws-a',
    activeTerminalId: null,
    terminals: [],
  })
  const detachedIds = new Set(['ws-repo'])
  const beforeDetached = workspaceStore.getState()
  assert.equal(actions.ensureWorkspace(workspaceStore, repo, true, detachedIds), null)
  assert.deepEqual(
    actions.resumeInWorkspace(workspaceStore, entry({ id: 'sess-detached' }), repo, true, detachedIds),
    { kind: 'unavailable', reason: 'detached' },
  )
  assert.equal(workspaceStore.getState(), beforeDetached, 'detached dedupe must not touch the store')
  assert.equal(workspaceStore.getState().activeWorkspaceId, 'ws-a')

  const detachedOwner = workspaceStore.addTerminal('ws-repo', 'claude-code', { cwd: 'C:\\Repo' })
  workspaceStore.setTerminalSdkSessionId(detachedOwner.id, 'sess-owned')
  workspaceStore.setFocusedTerminal(null)
  const terminalsWithOwner = workspaceStore.getState().terminals.length
  assert.deepEqual(
    actions.resumeInWorkspace(workspaceStore, entry({ id: 'sess-owned' }), other, true, detachedIds),
    { kind: 'unavailable', reason: 'detached' },
    'a session already open in a detached window is not focused from the main window',
  )
  assert.equal(workspaceStore.getState().focusedTerminalId, null)
  assert.equal(workspaceStore.getState().terminals.length, terminalsWithOwner)

  const reattached = actions.resumeInWorkspace(workspaceStore, entry({ id: 'sess-detached' }), repo, true, new Set())
  assert.equal(reattached.kind, 'created', 'without the detached flag the existing workspace is used')
  assert.equal(reattached.kind === 'created' && reattached.workspaceId, 'ws-repo')

  await (workspaceStore as unknown as { _savePromise: Promise<void> })._savePromise
  assert.ok(invokedCommands.includes('workspace_save'), 'workspace changes must be persisted')

  // List options — every list() carries the Settings → Agent → Session History values.
  const MiB = 1024 * 1024
  assert.deepEqual(actions.sessionHistoryListOptions({}, false), {
    refresh: false, includeNonInteractive: false, includeCodexArchived: false, maxLineBytes: 10 * MiB,
  })
  assert.deepEqual(actions.sessionHistoryListOptions({
    sessionHistoryIncludeNonInteractive: true,
    sessionHistoryIncludeCodexArchived: true,
    sessionHistoryMaxLineMiB: 3,
  }, true), {
    refresh: true, includeNonInteractive: true, includeCodexArchived: true, maxLineBytes: 3 * MiB,
  })
  assert.equal(actions.sessionHistoryListOptions({ sessionHistoryMaxLineMiB: 0 }, false).maxLineBytes, MiB, 'minimum is 1 MiB')
  for (const [input, expected] of [
    [undefined, 10], [Number.NaN, 10], [Number.POSITIVE_INFINITY, 10], ['5', 10], [null, 10],
    [0, 1], [-4, 1], [0.5, 1], [1, 1], [2.9, 2], [64, 64], [5000, 1024],
  ] as const) {
    assert.equal(actions.normalizeSessionHistoryMaxLineMiB(input), expected, String(input))
  }

  // Settings store — defaults stay implicit, values are normalized on set and on load.
  const { settingsStore } = await import('../renderer/src/stores/settings-store.ts')
  settingsStore.setSessionHistoryIncludeNonInteractive(true)
  settingsStore.setSessionHistoryIncludeCodexArchived(true)
  settingsStore.setSessionHistoryMaxLineMiB(0)
  let saved = JSON.parse(savedSettings.at(-1) ?? '{}') as Record<string, unknown>
  assert.equal(saved.sessionHistoryIncludeNonInteractive, true)
  assert.equal(saved.sessionHistoryIncludeCodexArchived, true)
  assert.equal(saved.sessionHistoryMaxLineMiB, 1)
  assert.deepEqual(actions.sessionHistoryListOptions(settingsStore.getSettings(), false), {
    refresh: false, includeNonInteractive: true, includeCodexArchived: true, maxLineBytes: MiB,
  })
  settingsStore.setSessionHistoryIncludeNonInteractive(false)
  settingsStore.setSessionHistoryIncludeCodexArchived(false)
  settingsStore.setSessionHistoryMaxLineMiB(10)
  saved = JSON.parse(savedSettings.at(-1) ?? '{}') as Record<string, unknown>
  assert.equal('sessionHistoryIncludeNonInteractive' in saved, false, 'off is stored as absent')
  assert.equal('sessionHistoryIncludeCodexArchived' in saved, false)
  assert.equal('sessionHistoryMaxLineMiB' in saved, false, 'the default is stored as absent')
  storedSettings = JSON.stringify({ sessionHistoryIncludeNonInteractive: 'yes', sessionHistoryMaxLineMiB: -3 })
  await settingsStore.load()
  assert.equal(settingsStore.getSettings().sessionHistoryIncludeNonInteractive, undefined)
  assert.equal(settingsStore.getSettings().sessionHistoryMaxLineMiB, 1)
  storedSettings = JSON.stringify({ sessionHistoryIncludeCodexArchived: true, sessionHistoryMaxLineMiB: 'big' })
  await settingsStore.load()
  assert.equal(settingsStore.getSettings().sessionHistoryIncludeCodexArchived, true)
  assert.equal(settingsStore.getSettings().sessionHistoryMaxLineMiB, undefined)

  // Polling — skips busy or hidden ticks, never polls after stop.
  type Tick = { handler: () => void; ms: number; cleared: boolean }
  const ticks: Tick[] = []
  const fakeTimers = {
    setInterval: (handler: () => void, ms: number) => {
      const tick = { handler, ms, cleared: false }
      ticks.push(tick)
      return tick
    },
    clearInterval: (handle: unknown) => { (handle as Tick).cleared = true },
  }
  let polls = 0
  let busy = false
  let hidden = false
  const stopPolling = actions.startSessionHistoryPolling({
    poll: () => { polls++ },
    isBusy: () => busy,
    isHidden: () => hidden,
    timers: fakeTimers,
  })
  assert.equal(ticks.length, 1)
  assert.equal(ticks[0].ms, 10_000, 'polls every 10 s')
  assert.equal(polls, 0, 'no poll before the first tick')
  ticks[0].handler()
  assert.equal(polls, 1)
  busy = true
  ticks[0].handler()
  assert.equal(polls, 1, 'a request in flight skips the tick')
  busy = false
  hidden = true
  ticks[0].handler()
  assert.equal(polls, 1, 'a hidden document skips the tick')
  hidden = false
  ticks[0].handler()
  assert.equal(polls, 2)
  stopPolling()
  assert.equal(ticks[0].cleared, true, 'leaving the History tab clears the interval')
  ticks[0].handler()
  assert.equal(polls, 2, 'a tick that fires after stop is ignored')
  stopPolling()
  assert.equal(ticks.length, 1)

  // Move all to Recycle Bin — only the sessions visible after search/filter, minus sessions open in a terminal.
  const gone = group({ key: 'dir:d:\\gone', kind: 'missing', name: 'gone', rootPath: 'D:\\Gone', normalizedRootPath: 'd:\\gone' })
  const goneResult: Result = {
    ...result,
    sessions: [
      entry({ id: 'g-alpha-1', title: 'alpha one', groupKey: gone.key, cwd: 'D:\\Gone', cwdExists: false }),
      entry({ id: 'g-alpha-2', title: 'alpha two', groupKey: gone.key, cwd: 'D:\\Gone', cwdExists: false }),
      entry({ id: 'g-alpha-3', agent: 'codex', title: 'alpha three', groupKey: gone.key, cwd: 'D:\\Gone', cwdExists: false }),
      entry({ id: 'g-beta', title: 'beta', groupKey: gone.key, cwd: 'D:\\Gone', cwdExists: false }),
    ],
    groups: [gone],
  }
  const goneView = actions.buildSessionHistoryView(goneResult, { ...filters, query: 'alpha', agent: 'claude' }, [])
  assert.equal(goneView.length, 1)
  seed({
    workspaces: [{ id: 'ws-a', name: 'A', folderPath: 'D:\\A', createdAt: 1 }],
    activeWorkspaceId: 'ws-a',
    activeTerminalId: null,
    terminals: [],
  })
  const openTerminal = workspaceStore.addTerminal('ws-a', 'claude-code', { cwd: 'D:\\A' })
  workspaceStore.setTerminalSdkSessionId(openTerminal.id, 'g-alpha-2')
  const codexSameId = workspaceStore.addTerminal('ws-a', 'codex-agent', { cwd: 'D:\\A' })
  workspaceStore.setTerminalSdkSessionId(codexSameId.id, 'g-alpha-1')
  assert.equal(actions.isSessionOpenInTerminal(workspaceStore, entry({ id: 'g-alpha-2' })), true)
  assert.equal(
    actions.isSessionOpenInTerminal(workspaceStore, entry({ id: 'g-alpha-1' })),
    false,
    'a codex terminal does not own a claude session with the same id',
  )
  assert.deepEqual(actions.selectTrashableSessions(goneView[0].sessions, workspaceStore), {
    sessions: [{ agent: 'claude', id: 'g-alpha-1' }],
    skippedOpen: 1,
  })
  assert.deepEqual(
    actions.selectTrashableSessions(actions.buildSessionHistoryView(goneResult, filters, [])[0].sessions, workspaceStore)
      .sessions.map(ref => ref.id).sort(),
    ['g-alpha-1', 'g-alpha-3', 'g-beta'],
  )
  assert.deepEqual(actions.selectTrashableSessions([], workspaceStore), { sessions: [], skippedOpen: 0 })
  assert.equal(actions.sessionRefKey({ agent: 'codex', id: 'x' }), 'codex:x')

  // Trash preview and result — only unblocked sessions are confirmed; the summary drives the banner tone.
  assert.deepEqual(actions.trashableRefsFromPreview({
    items: [
      { agent: 'claude', id: 'a', fileCount: 2, totalBytes: 10, newestModifiedAt: null, blockedReason: null },
      { agent: 'codex', id: 'b', fileCount: 1, totalBytes: 5, newestModifiedAt: null, blockedReason: 'recently-updated' },
      { agent: 'codex', id: 'c', fileCount: 3, totalBytes: 7, newestModifiedAt: '2026-11-02T00:00:00Z', blockedReason: null },
    ],
    fileCount: 5,
    totalBytes: 17,
    blockedCount: 1,
  }), [{ agent: 'claude', id: 'a' }, { agent: 'codex', id: 'c' }])
  assert.deepEqual(
    actions.summarizeTrashResult({ trashed: [{ agent: 'claude', id: 'a' }], blocked: [], failed: [] }),
    { trashed: 1, blocked: 0, failed: 0, tone: 'ok' },
  )
  assert.deepEqual(
    actions.summarizeTrashResult({
      trashed: [{ agent: 'claude', id: 'a' }],
      blocked: [{ agent: 'codex', id: 'b', reason: 'recently-updated' }],
      failed: [],
    }),
    { trashed: 1, blocked: 1, failed: 0, tone: 'warn' },
  )
  assert.deepEqual(
    actions.summarizeTrashResult({
      trashed: [{ agent: 'claude', id: 'a' }],
      blocked: [],
      failed: [{ agent: 'codex', id: 'c', error: 'access denied' }],
    }),
    { trashed: 1, blocked: 0, failed: 1, tone: 'warn' },
  )
  assert.deepEqual(
    actions.summarizeTrashResult({ trashed: [], blocked: [], failed: [{ agent: 'codex', id: 'c', error: 'x' }] }),
    { trashed: 0, blocked: 0, failed: 1, tone: 'error' },
  )
  assert.deepEqual(
    actions.summarizeTrashResult({ trashed: [], blocked: [{ agent: 'claude', id: 'a', reason: 'not-found' }], failed: [] }),
    { trashed: 0, blocked: 1, failed: 0, tone: 'warn' },
  )

  // Batches — at most the host's per-request limit, previewed and moved one batch at a time.
  {
    const { readFile } = await import('node:fs/promises')
    const hostTrash = await readFile('src-tauri/crates/bat-session-index/src/trash.rs', 'utf8')
    const hostLimit = /pub const MAX_TRASH_SESSIONS_PER_REQUEST: usize = (\d+);/.exec(hostTrash)?.[1]
    assert.equal(Number(hostLimit), actions.SESSION_TRASH_MAX_PER_REQUEST, 'renderer batch size matches the host limit')
    const refs = Array.from({ length: 450 }, (_, index) => ({ agent: 'claude' as const, id: `s${index}` }))
    assert.deepEqual(actions.chunkSessionRefs(refs).map(batch => batch.length), [200, 200, 50])
    assert.deepEqual(actions.chunkSessionRefs([...refs.slice(0, 3), refs[1]], 2), [refs.slice(0, 2), [refs[2]]])
    assert.deepEqual(actions.chunkSessionRefs([]), [])

    const previewCalls: number[] = []
    const preview = await actions.previewTrashInBatches(refs, async batch => {
      previewCalls.push(batch.length)
      return {
        items: batch.map((ref, index) => ({
          ...ref,
          fileCount: 1,
          totalBytes: 10,
          newestModifiedAt: null,
          blockedReason: index === 0 ? 'recently-updated' as const : null,
        })),
        fileCount: batch.length - 1,
        totalBytes: 10 * (batch.length - 1),
        blockedCount: 1,
      }
    })
    assert.deepEqual(previewCalls, [200, 200, 50])
    assert.equal(preview.items.length, 450)
    assert.equal(preview.blockedCount, 3)
    assert.equal(preview.fileCount, 447)
    assert.equal(preview.totalBytes, 4470)
    assert.equal(actions.trashableRefsFromPreview(preview).length, 447)
    await assert.rejects(
      actions.previewTrashInBatches(refs, async batch => {
        if (batch[0].id === 's200') throw new Error('timed out')
        return { items: [], fileCount: 0, totalBytes: 0, blockedCount: 0 }
      }),
      /timed out/,
    )

    const trashCalls: string[] = []
    const merged = await actions.trashInBatches(refs, async batch => {
      trashCalls.push(batch[0].id)
      return {
        trashed: batch.slice(2),
        blocked: [{ ...batch[0], reason: 'recently-updated' as const }],
        failed: [{ ...batch[1], error: 'locked' }],
      }
    }, 'not attempted')
    assert.deepEqual(trashCalls, ['s0', 's200', 's400'])
    assert.equal(merged.batches, 3)
    assert.equal(merged.rejected, false)
    assert.deepEqual(actions.summarizeTrashResult(merged.result), { trashed: 444, blocked: 3, failed: 3, tone: 'warn' })

    const stopped = await actions.trashInBatches(refs, async batch => {
      if (batch[0].id === 's200') throw new Error('host unreachable')
      return { trashed: batch, blocked: [], failed: [] }
    }, 'not attempted')
    assert.equal(stopped.batches, 2)
    assert.equal(stopped.rejected, true)
    assert.equal(stopped.result.trashed.length, 200)
    assert.equal(stopped.result.failed.length, 250)
    assert.deepEqual(stopped.result.failed[0], { agent: 'claude', id: 's200', error: 'host unreachable' })
    assert.deepEqual(stopped.result.failed[199], { agent: 'claude', id: 's399', error: 'host unreachable' })
    assert.deepEqual(stopped.result.failed[200], { agent: 'claude', id: 's400', error: 'not attempted' })
    assert.equal(actions.summarizeTrashResult(stopped.result).tone, 'warn')
  }

  // Wording follows the host OS; hosts without hostPlatform keep the Recycle Bin wording.
  assert.equal(actions.trashBinKind('windows'), 'recycle-bin')
  assert.equal(actions.trashBinKind(null), 'recycle-bin')
  assert.equal(actions.trashBinKind(undefined), 'recycle-bin')
  assert.equal(actions.trashBinKind('macos'), 'trash')
  assert.equal(actions.trashBinKind('linux'), 'trash')
  assert.equal(actions.trashTextKey('moveToTrash', 'recycle-bin'), 'sessionHistory.moveToTrash')
  assert.equal(actions.trashTextKey('moveToTrash', 'trash'), 'sessionHistory.osTrash.moveToTrash')

  // Dismissal — a running move keeps the dialog open.
  assert.equal(actions.trashDialogDismissAction(null), 'ignore')
  assert.equal(actions.trashDialogDismissAction({ confirming: false }), 'close-dialog')
  assert.equal(actions.trashDialogDismissAction({ confirming: true }), 'ignore')

  // Activation — the first view loads in the foreground; later views refresh quietly unless busy.
  assert.equal(actions.sessionHistoryActivationLoad({ activatedBefore: false, busy: false }), 'initial')
  assert.equal(actions.sessionHistoryActivationLoad({ activatedBefore: false, busy: true }), 'initial')
  assert.equal(actions.sessionHistoryActivationLoad({ activatedBefore: true, busy: false }), 'background')
  assert.equal(actions.sessionHistoryActivationLoad({ activatedBefore: true, busy: true }), 'skip')

  // Sidebar tab — presentation-only, unknown or unreadable values fall back to Workspaces.
  {
    const tabs = await import('../renderer/src/utils/sidebar-tab.ts')
    assert.deepEqual(tabs.SIDEBAR_TABS, ['workspaces', 'history'])
    assert.equal(tabs.parseSidebarTab('history'), 'history')
    for (const value of ['workspaces', null, undefined, '', 'History', 'settings', 1]) {
      assert.equal(tabs.parseSidebarTab(value), 'workspaces', String(value))
    }
    const memory = new Map<string, string>()
    const storage = {
      getItem: (key: string) => memory.get(key) ?? null,
      setItem: (key: string, value: string) => { memory.set(key, value) },
    }
    assert.equal(tabs.readStoredSidebarTab(() => storage), 'workspaces', 'nothing stored yet')
    tabs.writeStoredSidebarTab('history', () => storage)
    assert.equal(memory.get(tabs.SIDEBAR_TAB_STORAGE_KEY), 'history')
    assert.equal(tabs.readStoredSidebarTab(() => storage), 'history')
    memory.set(tabs.SIDEBAR_TAB_STORAGE_KEY, 'garbage')
    assert.equal(tabs.readStoredSidebarTab(() => storage), 'workspaces')
    const throwing = () => { throw new Error('storage disabled') }
    assert.equal(tabs.readStoredSidebarTab(throwing), 'workspaces', 'unavailable storage reads as Workspaces')
    assert.doesNotThrow(() => tabs.writeStoredSidebarTab('history', throwing), 'unavailable storage is ignored')
    assert.doesNotThrow(() => tabs.writeStoredSidebarTab('history', () => ({
      setItem: () => { throw new Error('quota exceeded') },
    })))
  }

  // Sizes — binary units, one decimal below 100.
  for (const [bytes, text] of [
    [0, '0 B'], [-1, '0 B'], [Number.NaN, '0 B'], [1, '1 B'], [1023, '1023 B'], [1024, '1.0 KB'],
    [1536, '1.5 KB'], [100 * 1024, '100 KB'], [10 * MiB, '10.0 MB'], [1.25 * 1024 * MiB, '1.3 GB'],
    [5 * 1024 * 1024 * MiB, '5.0 TB'], [2048 * 1024 * 1024 * MiB, '2048 TB'],
  ] as const) {
    assert.equal(actions.formatByteSize(bytes), text, String(bytes))
  }

  // Sidebar wiring — History is a tab, not a modal opened from the footer.
  const { readFile } = await import('node:fs/promises')
  const panelSource = await readFile('renderer/src/components/SessionHistoryPanel.tsx', 'utf8')
  const sidebarSource = await readFile('renderer/src/components/Sidebar.tsx', 'utf8')
  const appSource = await readFile('renderer/src/App.tsx', 'utf8')
  assert.ok(sidebarSource.includes('<SessionHistoryPanel'), 'the sidebar hosts the history panel')
  assert.ok(!sidebarSource.includes('onOpenHistory'), 'the footer no longer opens history')
  assert.ok(!appSource.includes('SessionHistoryModal'), 'the history modal is gone')
  assert.ok(sidebarSource.includes("active={activeTab === 'history' && !collapsed}"), 'polling follows tab visibility and stops while the sidebar is collapsed')
  assert.ok(sidebarSource.includes('onClick={handleAddWorkspace}'), 'adding a workspace returns to the Workspaces tab')
  assert.ok(sidebarSource.includes('onDragEnter={handleSidebarDragEnter}'), 'dragging files over the sidebar returns to the Workspaces tab')
  assert.ok(!panelSource.includes('settings-overlay'), 'the panel itself is not a modal overlay')

  // Trash dialog — every close path goes through the guard, and Escape is only captured while it is open.
  assert.ok(panelSource.includes('trashDialogOpen ? listenForDialogEscape(window, cancelTrash) : undefined'))
  assert.equal([...panelSource.matchAll(/listenForDialogEscape\(/g)].length, 1)
  assert.equal([...panelSource.matchAll(/onClick=\{cancelTrash\}/g)].length, 2, 'backdrop and Cancel button')
  assert.ok(panelSource.includes('createPortal(renderTrashDialog(trashDialog), document.body)'),
    'the dialog stays visible over the whole window even when the tab is switched while a preview loads')

  // Locales — every panel string exists in all four locale files with identical keys.
  const usedKeys = new Set([...panelSource.matchAll(/t\('sessionHistory\.([A-Za-z]+)'/g)].map(match => match[1]))
  assert.ok(usedKeys.size > 20, 'panel should use sessionHistory.* strings')
  const binKeys = new Set([...panelSource.matchAll(/tBin\('([A-Za-z]+)'/g)].map(match => match[1]))
  assert.equal(binKeys.size, 10, 'Recycle Bin / Trash strings')
  const sidebarKeys = new Set([...sidebarSource.matchAll(/t\('sidebar\.([A-Za-z]+)'/g)].map(match => match[1]))
  for (const key of ['workspaces', 'history', 'views']) assert.ok(sidebarKeys.has(key), `sidebar uses sidebar.${key}`)
  let referenceTrashKeys: string[] | null = null
  const settingsSource = await readFile('renderer/src/components/SettingsPanel.tsx', 'utf8')
  const settingsKeys = new Set([...settingsSource.matchAll(/t\('settings\.(sessionHistory[A-Za-z]*)'/g)].map(match => match[1]))
  assert.equal(settingsKeys.size, 7, 'Settings → Agent → Session History strings')
  let referenceKeys: string[] | null = null
  for (const locale of ['en', 'zh-TW', 'zh-CN', 'ja']) {
    const translations = JSON.parse(await readFile(`renderer/src/locales/${locale}.json`, 'utf8')) as {
      sidebar: Record<string, string>
      settings: Record<string, string>
      sessionHistory: Record<string, string>
    }
    for (const key of sidebarKeys) assert.ok(translations.sidebar[key], `${locale} is missing sidebar.${key}`)
    for (const key of settingsKeys) assert.ok(translations.settings[key], `${locale} is missing settings.${key}`)
    const keys = Object.keys(translations.sessionHistory).sort()
    if (referenceKeys) assert.deepEqual(keys, referenceKeys, `${locale} sessionHistory keys must match en`)
    else referenceKeys = keys
    const hasKey = (table: Record<string, unknown>, key: string) => key in table
      || (`${key}_one` in table && `${key}_other` in table)
    for (const key of usedKeys) {
      assert.ok(hasKey(translations.sessionHistory, key), `${locale} is missing sessionHistory.${key}`)
    }
    const osTrash = translations.sessionHistory.osTrash as unknown as Record<string, string>
    assert.equal(typeof osTrash, 'object', `${locale} sessionHistory.osTrash`)
    for (const key of binKeys) {
      assert.ok(hasKey(translations.sessionHistory, key), `${locale} is missing sessionHistory.${key}`)
      assert.ok(hasKey(osTrash, key), `${locale} is missing sessionHistory.osTrash.${key}`)
    }
    const trashKeys = Object.keys(osTrash).sort()
    if (referenceTrashKeys) assert.deepEqual(trashKeys, referenceTrashKeys, `${locale} osTrash keys must match en`)
    else referenceTrashKeys = trashKeys
  }
  console.log('session history actions: passed')
}

main().catch(error => {
  console.error(error instanceof Error ? error.stack || error.message : String(error))
  process.exit(1)
})
