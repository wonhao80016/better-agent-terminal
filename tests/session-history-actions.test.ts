// Run with: pnpm run test:session-history
import * as assert from 'node:assert/strict'

type TauriInvoke = <T>(cmd: string, args?: Record<string, unknown>) => Promise<T>

const invokedCommands: string[] = []
;(globalThis as { window?: unknown }).window = {
  __TAURI_INTERNALS__: {
    invoke: (async <T>(cmd: string) => {
      invokedCommands.push(cmd)
      if (cmd === 'workspace_save') return true as T
      if (cmd === 'app_set_dock_badge') return undefined as T
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

  // Escape — handled in the capture phase and stopped before window bubble listeners (agent panels) run.
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
  const unbindEscape = actions.listenForModalEscape(fakeWindow, () => { closed++ })
  assert.equal(registered.filter(r => r.capture).length, 1, 'modal listens in the capture phase')
  const escape = dispatchKey('Escape')
  assert.equal(closed, 1)
  assert.equal(escape.defaultPrevented, true)
  assert.deepEqual(panelKeys, [], 'Escape must not reach the agent panel while the modal is open')
  dispatchKey('a')
  assert.deepEqual(panelKeys, ['a'], 'other keys still reach window listeners')
  unbindEscape()
  dispatchKey('Escape')
  assert.equal(closed, 1)
  assert.deepEqual(panelKeys, ['a', 'Escape'], 'closed modal no longer intercepts Escape')
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

  // Locales — every modal string exists in all four locale files with identical keys.
  const { readFile } = await import('node:fs/promises')
  const modalSource = await readFile('renderer/src/components/SessionHistoryModal.tsx', 'utf8')
  const usedKeys = new Set([...modalSource.matchAll(/t\('sessionHistory\.([A-Za-z]+)'/g)].map(match => match[1]))
  assert.ok(usedKeys.size > 20, 'modal should use sessionHistory.* strings')
  let referenceKeys: string[] | null = null
  for (const locale of ['en', 'zh-TW', 'zh-CN', 'ja']) {
    const translations = JSON.parse(await readFile(`renderer/src/locales/${locale}.json`, 'utf8')) as {
      sidebar: Record<string, string>
      sessionHistory: Record<string, string>
    }
    assert.ok(translations.sidebar.history, `${locale} sidebar.history`)
    const keys = Object.keys(translations.sessionHistory).sort()
    if (referenceKeys) assert.deepEqual(keys, referenceKeys, `${locale} sessionHistory keys must match en`)
    else referenceKeys = keys
    for (const key of usedKeys) {
      const present = key in translations.sessionHistory
        || (`${key}_one` in translations.sessionHistory && `${key}_other` in translations.sessionHistory)
      assert.ok(present, `${locale} is missing sessionHistory.${key}`)
    }
  }
  console.log('session history actions: passed')
}

main().catch(error => {
  console.error(error instanceof Error ? error.stack || error.message : String(error))
  process.exit(1)
})
