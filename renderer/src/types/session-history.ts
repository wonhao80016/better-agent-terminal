// Mirrors the camelCase JSON returned by the `session_history_list` Tauri command.

export type SessionAgent = 'claude' | 'codex'

export type SessionGroupKind = 'repo' | 'dir' | 'missing' | 'unknown'

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
  /** Interactive, non-subagent sessions sorted by updatedAt desc. */
  sessions: SessionHistoryEntry[]
  /** Ordered by latest session desc; "missing" and "unknown" groups last. */
  groups: SessionHistoryGroup[]
  scan: SessionHistoryScanStatus
  caseInsensitivePaths: boolean
}

export interface SessionHistoryListOptions {
  refresh?: boolean
}
