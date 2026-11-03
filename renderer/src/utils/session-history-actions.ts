import type { AppSettings, TerminalInstance, Workspace } from '../types'
import type { AgentPresetId } from '../types/agent-presets'
import type {
  HostPlatform,
  SessionAgent,
  SessionHistoryEntry,
  SessionHistoryGroup,
  SessionHistoryListOptions,
  SessionHistoryResult,
  SessionRef,
  SessionTrashPreview,
  SessionTrashResult,
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

export type SessionHistoryScanSettings = Pick<
  AppSettings,
  'sessionHistoryIncludeNonInteractive' | 'sessionHistoryIncludeCodexArchived' | 'sessionHistoryMaxLineMiB'
>

export interface IntervalTimers {
  setInterval(handler: () => void, ms: number): unknown
  clearInterval(handle: unknown): void
}

export interface SessionHistoryPollingOptions {
  poll(): void
  isBusy(): boolean
  isHidden(): boolean
  intervalMs?: number
  timers?: IntervalTimers
}

export interface SessionTrashSelection {
  sessions: SessionRef[]
  skippedOpen: number
}

export interface SessionTrashSummary {
  trashed: number
  blocked: number
  failed: number
  tone: 'ok' | 'warn' | 'error'
}

export const SESSION_HISTORY_POLL_INTERVAL_MS = 10_000
export const SESSION_HISTORY_DEFAULT_MAX_LINE_MIB = 10
export const SESSION_HISTORY_MIN_MAX_LINE_MIB = 1
// Same [1 MiB, 1 GiB] range the host clamps to.
export const SESSION_HISTORY_MAX_MAX_LINE_MIB = 1024
// Must match MAX_TRASH_SESSIONS_PER_REQUEST in bat-session-index; the host rejects larger requests.
export const SESSION_TRASH_MAX_PER_REQUEST = 200
const BYTES_PER_MIB = 1024 * 1024
const SIZE_UNITS = ['B', 'KB', 'MB', 'GB', 'TB'] as const

const DEFAULT_INTERVAL_TIMERS: IntervalTimers = {
  setInterval: (handler, ms) => setInterval(handler, ms),
  clearInterval: handle => clearInterval(handle as ReturnType<typeof setInterval>),
}

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

/** Whole MiB within [1, 1024]; anything unparseable falls back to the default. */
export function normalizeSessionHistoryMaxLineMiB(value: unknown): number {
  if (typeof value !== 'number' || !Number.isFinite(value)) return SESSION_HISTORY_DEFAULT_MAX_LINE_MIB
  return Math.min(SESSION_HISTORY_MAX_MAX_LINE_MIB, Math.max(SESSION_HISTORY_MIN_MAX_LINE_MIB, Math.floor(value)))
}

export function sessionHistoryListOptions(
  settings: SessionHistoryScanSettings,
  refresh: boolean,
): SessionHistoryListOptions {
  return {
    refresh,
    includeNonInteractive: settings.sessionHistoryIncludeNonInteractive === true,
    includeCodexArchived: settings.sessionHistoryIncludeCodexArchived === true,
    maxLineBytes: normalizeSessionHistoryMaxLineMiB(settings.sessionHistoryMaxLineMiB) * BYTES_PER_MIB,
  }
}

/** Skips a tick while a request is in flight or the document is hidden. Returns the stopper. */
export function startSessionHistoryPolling(options: SessionHistoryPollingOptions): () => void {
  const timers = options.timers ?? DEFAULT_INTERVAL_TIMERS
  let stopped = false
  const handle = timers.setInterval(() => {
    if (stopped || options.isBusy() || options.isHidden()) return
    options.poll()
  }, options.intervalMs ?? SESSION_HISTORY_POLL_INTERVAL_MS)
  return () => {
    if (stopped) return
    stopped = true
    timers.clearInterval(handle)
  }
}

export function sessionRefKey(ref: SessionRef): string {
  return `${ref.agent}:${ref.id}`
}

export function isSessionOpenInTerminal(
  store: Pick<SessionHistoryWorkspaceStore, 'findSdkSessionOwner'>,
  entry: Pick<SessionHistoryEntry, 'agent' | 'id'>,
): boolean {
  return !!store.findSdkSessionOwner(entry.id, sessionAgentPreset(entry.agent))
}

/** Sessions open in a terminal are left out because their transcripts are still being written. */
export function selectTrashableSessions(
  visible: readonly SessionHistoryEntry[],
  store: Pick<SessionHistoryWorkspaceStore, 'findSdkSessionOwner'>,
): SessionTrashSelection {
  const sessions: SessionRef[] = []
  let skippedOpen = 0
  for (const entry of visible) {
    if (isSessionOpenInTerminal(store, entry)) skippedOpen++
    else sessions.push({ agent: entry.agent, id: entry.id })
  }
  return { sessions, skippedOpen }
}

export function trashableRefsFromPreview(preview: SessionTrashPreview): SessionRef[] {
  return preview.items
    .filter(item => item.blockedReason === null)
    .map(item => ({ agent: item.agent, id: item.id }))
}

/** Splits into host-sized batches, dropping repeated sessions. */
export function chunkSessionRefs(
  refs: readonly SessionRef[],
  size: number = SESSION_TRASH_MAX_PER_REQUEST,
): SessionRef[][] {
  const seen = new Set<string>()
  const unique = refs.filter(ref => {
    const key = sessionRefKey(ref)
    if (seen.has(key)) return false
    seen.add(key)
    return true
  })
  const batches: SessionRef[][] = []
  for (let start = 0; start < unique.length; start += size) batches.push(unique.slice(start, start + size))
  return batches
}

/** Previews batch by batch and merges the results; any rejected batch rejects the whole preview. */
export async function previewTrashInBatches(
  sessions: readonly SessionRef[],
  preview: (batch: SessionRef[]) => Promise<SessionTrashPreview>,
  size: number = SESSION_TRASH_MAX_PER_REQUEST,
): Promise<SessionTrashPreview> {
  const merged: SessionTrashPreview = { items: [], fileCount: 0, totalBytes: 0, blockedCount: 0 }
  for (const batch of chunkSessionRefs(sessions, size)) {
    const part = await preview(batch)
    merged.items.push(...part.items)
    merged.fileCount += part.fileCount
    merged.totalBytes += part.totalBytes
    merged.blockedCount += part.blockedCount
  }
  return merged
}

export interface BatchedTrashOutcome {
  result: SessionTrashResult
  batches: number
  rejected: boolean
}

/**
 * Moves batch by batch and never rejects. A rejected batch counts all of its sessions as failed with the rejection
 * message and stops: the host may still be working on a request that timed out. Later sessions fail as `notAttempted`.
 */
export async function trashInBatches(
  sessions: readonly SessionRef[],
  trash: (batch: SessionRef[]) => Promise<SessionTrashResult>,
  notAttempted: string,
  size: number = SESSION_TRASH_MAX_PER_REQUEST,
): Promise<BatchedTrashOutcome> {
  const result: SessionTrashResult = { trashed: [], blocked: [], failed: [] }
  const batches = chunkSessionRefs(sessions, size)
  for (let index = 0; index < batches.length; index++) {
    try {
      const part = await trash(batches[index])
      result.trashed.push(...part.trashed)
      result.blocked.push(...part.blocked)
      result.failed.push(...part.failed)
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error)
      result.failed.push(...batches[index].map(ref => ({ ...ref, error: message })))
      for (const later of batches.slice(index + 1).flat()) result.failed.push({ ...later, error: notAttempted })
      return { result, batches: index + 1, rejected: true }
    }
  }
  return { result, batches: batches.length, rejected: false }
}

export type TrashBinKind = 'recycle-bin' | 'trash'

/** Hosts that predate `hostPlatform` are treated as Windows, the wording those builds already used. */
export function trashBinKind(hostPlatform: HostPlatform | null | undefined): TrashBinKind {
  return hostPlatform === 'macos' || hostPlatform === 'linux' ? 'trash' : 'recycle-bin'
}

/** Trash wording lives under `sessionHistory.osTrash.*` with the same key names as the Recycle Bin wording. */
export function trashTextKey(key: string, kind: TrashBinKind): string {
  return kind === 'trash' ? `sessionHistory.osTrash.${key}` : `sessionHistory.${key}`
}

export type TrashDialogDismissAction = 'close-dialog' | 'ignore'

/** A running move keeps the dialog open so its result is not lost. */
export function trashDialogDismissAction(dialog: { confirming: boolean } | null): TrashDialogDismissAction {
  return dialog && !dialog.confirming ? 'close-dialog' : 'ignore'
}

export type SessionHistoryActivationLoad = 'initial' | 'background' | 'skip'

/** The first activation shows the loading state; later ones refresh quietly unless a request is still running. */
export function sessionHistoryActivationLoad(state: { activatedBefore: boolean; busy: boolean }): SessionHistoryActivationLoad {
  if (!state.activatedBefore) return 'initial'
  return state.busy ? 'skip' : 'background'
}

export function summarizeTrashResult(result: SessionTrashResult): SessionTrashSummary {
  const trashed = result.trashed.length
  const blocked = result.blocked.length
  const failed = result.failed.length
  let tone: SessionTrashSummary['tone'] = 'ok'
  if (failed > 0 && trashed === 0) tone = 'error'
  else if (failed > 0 || blocked > 0) tone = 'warn'
  return { trashed, blocked, failed, tone }
}

export function formatByteSize(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes <= 0) return '0 B'
  let value = bytes
  let unit = 0
  while (value >= 1024 && unit < SIZE_UNITS.length - 1) {
    value /= 1024
    unit++
  }
  const digits = unit === 0 || value >= 100 ? 0 : 1
  return `${value.toFixed(digits)} ${SIZE_UNITS[unit]}`
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
 * Handles Escape in the capture phase so the key never reaches the agent panels' window keydown listeners,
 * where it would deny a pending permission or interrupt a streaming turn behind the dialog. Returns the unbinder.
 */
export function listenForDialogEscape(
  target: Pick<EventTarget, 'addEventListener' | 'removeEventListener'>,
  onEscape: () => void,
): () => void {
  const handler = (event: Event) => {
    if ((event as KeyboardEvent).key !== 'Escape') return
    event.preventDefault()
    event.stopImmediatePropagation()
    onEscape()
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
