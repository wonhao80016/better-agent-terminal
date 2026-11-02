// Mirrors the camelCase JSON returned by the `session_history_list` Tauri command.

export type SessionAgent = 'claude' | 'codex'

export type SessionGroupKind = 'repo' | 'dir' | 'missing' | 'unknown'

export type HostPlatform = 'windows' | 'macos' | 'linux'

export interface SessionHistoryEntry {
  agent: SessionAgent
  id: string
  title: string
  firstPrompt: string | null
  lastReply: string | null
  messageCount: number
  subagentCount: number
  model: string | null
  gitBranch: string | null
  cwd: string | null
  cwdExists: boolean
  startedAt: string | null
  updatedAt: string | null
  groupKey: string
  skippedLines: number
  /** False for Claude entrypoint sdk-cli and Codex source exec sessions. */
  interactive: boolean
  /** True when the transcript lives under Codex archived_sessions/. */
  archived: boolean
}

export interface SessionHistoryGroup {
  /** "repo:<normalized common dir>" | "dir:<normalized cwd>" | "unknown" */
  key: string
  kind: SessionGroupKind
  name: string
  rootPath: string | null
  normalizedRootPath: string | null
  /** Distinct session cwds in a repo group that are not the repo root. */
  worktreeCount: number
  sessionCount: number
  latestUpdatedAt: string | null
}

export interface SessionHistoryScanStatus {
  fileCount: number
  /** Files parsed this run (cache misses). */
  parsedCount: number
  cacheHits: number
  failedCount: number
  skippedLines: number
  durationMs: number
  scannedAt: string
  gitAvailable: boolean
}

export interface SessionHistoryResult {
  /** Non-subagent sessions sorted by updatedAt desc; non-interactive and archived ones only when requested. */
  sessions: SessionHistoryEntry[]
  /** Ordered by latest session desc; "missing" and "unknown" groups last. */
  groups: SessionHistoryGroup[]
  scan: SessionHistoryScanStatus
  caseInsensitivePaths: boolean
  /** OS whose trash receives moved sessions; null or absent from hosts that predate the field. */
  hostPlatform?: HostPlatform | null
}

export interface SessionHistoryListOptions {
  refresh?: boolean
  includeNonInteractive?: boolean
  includeCodexArchived?: boolean
  /** The host clamps this to 1 MiB ..= 1 GiB. */
  maxLineBytes?: number
}

/** Identity only: the host resolves the owned files from its own index, never from client paths. */
export interface SessionRef {
  agent: SessionAgent
  id: string
}

export type TrashBlockReason = 'recently-updated' | 'not-found' | 'outside-data-dir'

export interface SessionTrashPreviewItem {
  agent: SessionAgent
  id: string
  fileCount: number
  totalBytes: number
  newestModifiedAt: string | null
  blockedReason: TrashBlockReason | null
}

export interface SessionTrashPreview {
  items: SessionTrashPreviewItem[]
  /** Sum over non-blocked items. */
  fileCount: number
  /** Sum over non-blocked items. */
  totalBytes: number
  blockedCount: number
}

export interface SessionTrashResult {
  trashed: SessionRef[]
  blocked: Array<SessionRef & { reason: TrashBlockReason }>
  failed: Array<SessionRef & { error: string }>
}
