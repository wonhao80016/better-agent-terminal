import type { TerminalInstance, Workspace } from '../types'
import type { AgentPresetId } from '../types/agent-presets'
import type {
  SessionAgent,
  SessionHistoryEntry,
  SessionHistoryGroup,
  SessionHistoryResult,
} from '../types/session-history'

export type SessionHistoryAgentFilter = 'all' | SessionAgent
export type SessionHistorySort = 'updated' | 'started' | 'messages'

export interface SessionHistoryFilters {
  query: string
  agent: SessionHistoryAgentFilter
  sort: SessionHistorySort
  onlyNotAdded: boolean
}

export interface SessionHistoryGroupView {
  group: SessionHistoryGroup
  sessions: SessionHistoryEntry[]
  workspace: Workspace | undefined
}

export interface SessionHistoryWorkspaceStore {
  getState(): { workspaces: Workspace[] }
  addWorkspace(name: string, folderPath: string): Workspace
  setActiveWorkspace(id: string): void
  getActiveGroup(): string | null
  setWorkspaceGroup(id: string, group: string | undefined): void
  save(): Promise<void>
  findSdkSessionOwner(sdkSessionId: string | undefined, agentPreset: AgentPresetId | undefined): TerminalInstance | undefined
  setFocusedTerminal(id: string | null): void
  addTerminal(workspaceId: string, agentPreset?: AgentPresetId, init?: { cwd?: string; focus?: boolean }): TerminalInstance
  setTerminalSdkSessionId(terminalId: string, sdkSessionId: string | undefined): void
}

export type ResumeUnavailableReason = 'no-cwd' | 'cwd-missing' | 'cwd-unverified' | 'no-workspace' | 'detached'

export type ResumeOutcome =
  | { kind: 'focused'; terminalId: string }
  | { kind: 'created'; terminalId: string; workspaceId: string }
  | { kind: 'unavailable'; reason: ResumeUnavailableReason }

const NO_DETACHED_WORKSPACES: ReadonlySet<string> = new Set()
// Must match is_valid_session_id in bat-session-index; the id is pasted into a shell.
const SESSION_ID_PATTERN = /^[A-Za-z0-9][A-Za-z0-9._-]*$/
const WINDOWS_DRIVE_PATH =/^[A-Za-z]:([\\/]|$)/
const WINDOWS_UNC_PATH = /^\\\\/

function isWindowsHostPath(path: string): boolean {
  return WINDOWS_DRIVE_PATH.test(path) || WINDOWS_UNC_PATH.test(path)
}

// Must stay byte-identical to the host-side normalizer in bat-session-index:
// workspace dedupe compares this output with SessionHistoryGroup.normalizedRootPath.
export function normalizeHostPath(path: string, caseInsensitive: boolean): string {
  if (isWindowsHostPath(path)) {
    const unc = WINDOWS_UNC_PATH.test(path)
    let normalized = path.replace(/\//g, '\\').replace(/\\{2,}/g, '\\')
    if (unc) normalized = `\\${normalized}`
    normalized = normalized.replace(/\\+$/, '')
    if (/^[A-Za-z]:$/.test(normalized)) normalized += '\\'
    return normalized.toLowerCase()
  }
  let normalized = path.replace(/\/{2,}/g, '/')
  if (normalized.length > 1) normalized = normalized.replace(/\/+$/, '') || '/'
  return caseInsensitive ? normalized.toLowerCase() : normalized
}

export function sessionAgentPreset(agent: SessionAgent): AgentPresetId {
  return agent === 'claude' ? 'claude-code' : 'codex-agent'
}

export function isValidSessionId(id: string): boolean {
  return SESSION_ID_PATTERN.test(id)
}

/** Null when the id is not safe to paste into a shell. The command only works from the session's cwd. */
export function sessionResumeCommand(entry: Pick<SessionHistoryEntry, 'agent' | 'id'>): string | null {
  if (!isValidSessionId(entry.id)) return null
  return entry.agent === 'claude' ? `claude --resume ${entry.id}` : `codex resume ${entry.id}`
}

/**
 * Closes on Escape in the capture phase so the key never reaches the agent panels' window keydown listeners,
 * where it would deny a pending permission or interrupt a streaming turn behind the modal. Returns the unbinder.
 */
export function listenForModalEscape(
  target: Pick<EventTarget, 'addEventListener' | 'removeEventListener'>,
  onClose: () => void,
): () => void {
  const handler = (event: Event) => {
    if ((event as KeyboardEvent).key !== 'Escape') return
    event.preventDefault()
    event.stopImmediatePropagation()
    onClose()
  }
  target.addEventListener('keydown', handler, true)
  return () => target.removeEventListener('keydown', handler, true)
}

export function canAddGroupAsWorkspace(group: SessionHistoryGroup): boolean {
  return !!group.rootPath && group.kind !== 'missing' && group.kind !== 'unknown'
}

/**
 * A `dir` group is keyed by the cwd itself, so an unconfirmed cwd there is one the host did not report missing
 * (permission error or probe timeout, e.g. an offline share).
 */
export function resumeUnavailableReason(
  entry: SessionHistoryEntry,
  group?: Pick<SessionHistoryGroup, 'kind'>,
): ResumeUnavailableReason | null {
  if (!entry.cwd) return 'no-cwd'
  if (!entry.cwdExists) return group?.kind === 'dir' ? 'cwd-unverified' : 'cwd-missing'
  return null
}

export function matchesSessionQuery(entry: SessionHistoryEntry, query: string): boolean {
  const terms = query.toLowerCase().split(/\s+/).filter(Boolean)
  if (terms.length === 0) return true
  const haystack = [
    entry.title,
    entry.firstPrompt,
    entry.lastReply,
    entry.gitBranch,
    entry.model,
    entry.cwd,
    entry.id,
  ].filter((value): value is string => !!value).join('\n').toLowerCase()
  return terms.every(term => haystack.includes(term))
}

export function findWorkspaceForGroup(
  workspaces: readonly Workspace[],
  group: SessionHistoryGroup,
  caseInsensitive: boolean,
): Workspace | undefined {
  const target = group.normalizedRootPath
    ?? (group.rootPath ? normalizeHostPath(group.rootPath, caseInsensitive) : null)
  if (!target) return undefined
  return workspaces.find(w => normalizeHostPath(w.folderPath, caseInsensitive) === target)
}

function timestampValue(value: string | null): number {
  if (!value) return Number.NEGATIVE_INFINITY
  const parsed = Date.parse(value)
  return Number.isNaN(parsed) ? Number.NEGATIVE_INFINITY : parsed
}

function sortValue(entry: SessionHistoryEntry, sort: SessionHistorySort): number {
  if (sort === 'messages') return entry.messageCount
  if (sort === 'started') return timestampValue(entry.startedAt ?? entry.updatedAt)
  return timestampValue(entry.updatedAt ?? entry.startedAt)
}

function isTrailingGroupKind(group: SessionHistoryGroup): boolean {
  return group.kind === 'missing' || group.kind === 'unknown'
}

export function buildSessionHistoryView(
  result: SessionHistoryResult,
  filters: SessionHistoryFilters,
  workspaces: readonly Workspace[],
): SessionHistoryGroupView[] {
  const sessionsByGroup = new Map<string, SessionHistoryEntry[]>()
  for (const entry of result.sessions) {
    if (filters.agent !== 'all' && entry.agent !== filters.agent) continue
    if (!matchesSessionQuery(entry, filters.query)) continue
    const bucket = sessionsByGroup.get(entry.groupKey)
    if (bucket) bucket.push(entry)
    else sessionsByGroup.set(entry.groupKey, [entry])
  }

  const views: Array<SessionHistoryGroupView & { order: number; best: number }> = []
  result.groups.forEach((group, order) => {
    const sessions = sessionsByGroup.get(group.key)
    if (!sessions || sessions.length === 0) return
    const workspace = findWorkspaceForGroup(workspaces, group, result.caseInsensitivePaths)
    if (filters.onlyNotAdded && workspace) return
    sessions.sort((a, b) => sortValue(b, filters.sort) - sortValue(a, filters.sort))
    views.push({ group, sessions, workspace, order, best: sortValue(sessions[0], filters.sort) })
  })

  views.sort((a, b) => {
    const trailing = Number(isTrailingGroupKind(a.group)) - Number(isTrailingGroupKind(b.group))
    if (trailing !== 0) return trailing
    if (b.best !== a.best) return b.best > a.best ? 1 : -1
    return a.order - b.order
  })
  return views.map(({ group, sessions, workspace }) => ({ group, sessions, workspace }))
}

/**
 * Returns the workspace for the group, adding it to the active sidebar group when new. Null when the group cannot
 * be added or its workspace is in a detached window, which the main window does not render.
 */
export function ensureWorkspace(
  store: SessionHistoryWorkspaceStore,
  group: SessionHistoryGroup,
  caseInsensitive: boolean,
  detachedWorkspaceIds: ReadonlySet<string> = NO_DETACHED_WORKSPACES,
): Workspace | null {
  if (!group.rootPath) return null
  const existing = findWorkspaceForGroup(store.getState().workspaces, group, caseInsensitive)
  if (existing) {
    if (detachedWorkspaceIds.has(existing.id)) return null
    store.setActiveWorkspace(existing.id)
    return existing
  }
  if (!canAddGroupAsWorkspace(group)) return null
  const workspace = store.addWorkspace(group.name, group.rootPath)
  // A new workspace outside the active group filter would be hidden from the sidebar.
  const activeGroup = store.getActiveGroup()
  if (activeGroup) store.setWorkspaceGroup(workspace.id, activeGroup)
  void store.save()
  return workspace
}

export function resumeInWorkspace(
  store: SessionHistoryWorkspaceStore,
  entry: SessionHistoryEntry,
  group: SessionHistoryGroup,
  caseInsensitive: boolean,
  detachedWorkspaceIds: ReadonlySet<string> = NO_DETACHED_WORKSPACES,
): ResumeOutcome {
  const unavailable = resumeUnavailableReason(entry, group)
  if (unavailable || !entry.cwd) return { kind: 'unavailable', reason: unavailable ?? 'no-cwd' }

  // findSdkSessionOwner needs a concrete preset; undefined never matches any terminal.
  const preset = sessionAgentPreset(entry.agent)
  const owner = store.findSdkSessionOwner(entry.id, preset)
  if (owner && detachedWorkspaceIds.has(owner.workspaceId)) return { kind: 'unavailable', reason: 'detached' }
  if (owner) {
    store.setFocusedTerminal(owner.id)
    return { kind: 'focused', terminalId: owner.id }
  }

  const existing = findWorkspaceForGroup(store.getState().workspaces, group, caseInsensitive)
  if (existing && detachedWorkspaceIds.has(existing.id)) return { kind: 'unavailable', reason: 'detached' }
  const workspace = ensureWorkspace(store, group, caseInsensitive, detachedWorkspaceIds)
  if (!workspace) return { kind: 'unavailable', reason: 'no-workspace' }
  // The agent resolves transcripts by cwd, so the terminal must start in the
  // session's original cwd; the sdkSessionId must be set before the panel mounts.
  const terminal = store.addTerminal(workspace.id, preset, { cwd: entry.cwd, focus: true })
  store.setTerminalSdkSessionId(terminal.id, entry.id)
  return { kind: 'created', terminalId: terminal.id, workspaceId: workspace.id }
}
