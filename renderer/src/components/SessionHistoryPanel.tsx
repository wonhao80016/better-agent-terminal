import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import type { KeyboardEvent as ReactKeyboardEvent, ReactNode } from 'react'
import { createPortal } from 'react-dom'
import { useTranslation } from 'react-i18next'
import type { TFunction } from 'i18next'
import { host } from '../host-api'
import { settingsStore } from '../stores/settings-store'
import { useWorkspace, workspaceStore } from '../stores/workspace-store'
import type { AppState, Workspace } from '../types'
import type {
  SessionHistoryEntry,
  SessionHistoryGroup,
  SessionHistoryResult,
  SessionRef,
  SessionTrashPreview,
  SessionTrashResult,
  TrashBlockReason,
} from '../types/session-history'
import {
  buildSessionHistoryView,
  canAddGroupAsWorkspace,
  ensureWorkspace,
  formatByteSize,
  listenForDialogEscape,
  previewTrashInBatches,
  resumeInWorkspace,
  resumeUnavailableReason,
  selectTrashableSessions,
  sessionAgentPreset,
  sessionHistoryActivationLoad,
  sessionHistoryListOptions,
  sessionRefKey,
  sessionResumeCommand,
  startSessionHistoryPolling,
  summarizeTrashResult,
  trashableRefsFromPreview,
  trashBinKind,
  trashDialogDismissAction,
  trashInBatches,
  trashTextKey,
  type ResumeUnavailableReason,
  type SessionHistoryAgentFilter,
  type SessionHistorySort,
  type SessionTrashSummary,
} from '../utils/session-history-actions'
import '../styles/session-history.css'

interface SessionHistoryPanelProps {
  /** Polling runs only while the panel is visible. */
  active: boolean
  /** Called after a resume or "go to workspace" changed the main area. */
  onWorkspaceShown: () => void
  remoteHostLabel?: string
  remoteOrigin?: string | null
  /** Workspaces shown in their own window; the main window does not render them. */
  detachedWorkspaceIds?: ReadonlySet<string>
}

const NO_DETACHED_WORKSPACES: ReadonlySet<string> = new Set()

const COPY_FEEDBACK_MS = 1500

function logHistory(...args: unknown[]): void {
  void host.debug.log?.('[SessionHistory]', ...args)?.catch?.(() => {})
}

function errorText(error: unknown): string {
  return error instanceof Error ? error.message : String(error)
}

function formatRelativeTime(iso: string | null, now: number, t: TFunction): string {
  const parsed = iso ? Date.parse(iso) : Number.NaN
  if (Number.isNaN(parsed)) return t('sessionHistory.unknownTime')
  const minutes = Math.floor(Math.max(0, now - parsed) / 60_000)
  if (minutes < 1) return t('sessionHistory.justNow')
  if (minutes < 60) return t('sessionHistory.minutesAgo', { count: minutes })
  const hours = Math.floor(minutes / 60)
  if (hours < 24) return t('sessionHistory.hoursAgo', { count: hours })
  return t('sessionHistory.daysAgo', { count: Math.floor(hours / 24) })
}

function formatAbsoluteTime(iso: string | null, t: TFunction): string {
  const parsed = iso ? Date.parse(iso) : Number.NaN
  return Number.isNaN(parsed) ? t('sessionHistory.unknownTime') : new Date(parsed).toLocaleString()
}

// Re-render "already open" badges only when the set of bound agent sessions changes,
// not on every terminal activity tick.
function selectOpenSessionsKey(state: AppState): string {
  return state.terminals
    .filter(terminal => terminal.sdkSessionId)
    .map(terminal => `${terminal.agentPreset ?? ''}:${terminal.sdkSessionId}`)
    .sort()
    .join('\n')
}

function resumeReasonText(
  reason: ResumeUnavailableReason,
  entry: SessionHistoryEntry,
  t: TFunction,
): string {
  if (reason === 'no-cwd') return t('sessionHistory.resumeNoCwd')
  if (reason === 'cwd-missing') return t('sessionHistory.resumeCwdMissing', { path: entry.cwd ?? '' })
  if (reason === 'cwd-unverified') return t('sessionHistory.resumeCwdUnverified', { path: entry.cwd ?? '' })
  if (reason === 'detached') return t('sessionHistory.resumeDetached')
  return t('sessionHistory.resumeNoWorkspace')
}

function resumeCommandHint(entry: SessionHistoryEntry, t: TFunction): string {
  return entry.cwd
    ? t('sessionHistory.resumeCommandHint', { path: entry.cwd })
    : t('sessionHistory.resumeNoCwd')
}

// Groups start collapsed; while searching they open so matches are visible.
function isDefaultCollapsed(group: SessionHistoryGroup, searching: boolean): boolean {
  return !searching || group.kind === 'missing'
}

function trashBlockReasonText(reason: TrashBlockReason, t: TFunction): string {
  if (reason === 'recently-updated') return t('sessionHistory.trashBlockedRecentlyUpdated')
  if (reason === 'not-found') return t('sessionHistory.trashBlockedNotFound')
  return t('sessionHistory.trashBlockedOutsideDataDir')
}

type TrashText = (key: string, options?: Record<string, unknown>) => string

function trashSummaryText(summary: SessionTrashSummary, t: TFunction, tBin: TrashText): string {
  const parts = [tBin('trashResultTrashed', { count: summary.trashed })]
  if (summary.blocked > 0) parts.push(t('sessionHistory.trashResultBlocked', { count: summary.blocked }))
  if (summary.failed > 0) parts.push(t('sessionHistory.trashResultFailed', { count: summary.failed }))
  return parts.join(' · ')
}

function Icon({ children }: Readonly<{ children: ReactNode }>) {
  return (
    <svg
      width="13"
      height="13"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      {children}
    </svg>
  )
}

const ICON_PLUS = <Icon><path d="M12 5v14M5 12h14" /></Icon>
const ICON_GO = <Icon><path d="M5 12h14M13 6l6 6-6 6" /></Icon>
const ICON_WINDOW = <Icon><rect x="3" y="4" width="18" height="14" rx="2" /><path d="M3 8h18" /></Icon>
const ICON_TRASH = (
  <Icon><path d="M3 6h18M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6M8 6V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2" /></Icon>
)
const ICON_FILTER = <Icon><path d="M22 3H2l8 9.46V19l4 2v-8.54L22 3z" /></Icon>

interface TrashDialogState {
  preview: SessionTrashPreview
  skippedOpen: number
  confirming: boolean
}

interface TrashOutcome {
  summary: SessionTrashSummary
  result: SessionTrashResult
}

export function SessionHistoryPanel({
  active,
  onWorkspaceShown,
  remoteHostLabel,
  remoteOrigin,
  detachedWorkspaceIds = NO_DETACHED_WORKSPACES,
}: Readonly<SessionHistoryPanelProps>) {
  const { t } = useTranslation()
  const [result, setResult] = useState<SessionHistoryResult | null>(null)
  const [loading, setLoading] = useState(true)
  const [loadError, setLoadError] = useState<string | null>(null)
  const [actionError, setActionError] = useState<string | null>(null)
  const [query, setQuery] = useState('')
  const [agentFilter, setAgentFilter] = useState<SessionHistoryAgentFilter>('all')
  const [sort, setSort] = useState<SessionHistorySort>('updated')
  const [onlyNotAdded, setOnlyNotAdded] = useState(false)
  const [toggledGroups, setToggledGroups] = useState<ReadonlySet<string>>(() => new Set())
  const [expandedId, setExpandedId] = useState<string | null>(null)
  const [copiedKey, setCopiedKey] = useState<string | null>(null)
  const [now, setNow] = useState(() => Date.now())
  const [trashBusy, setTrashBusy] = useState(false)
  const [trashDialog, setTrashDialog] = useState<TrashDialogState | null>(null)
  const [trashOutcome, setTrashOutcome] = useState<TrashOutcome | null>(null)
  const requestSeq = useRef(0)
  const listsInFlight = useRef(0)
  const activatedBefore = useRef(false)
  const trashBusyRef = useRef(false)
  const trashDialogRef = useRef<TrashDialogState | null>(null)
  const trashDialogElement = useRef<HTMLDivElement | null>(null)
  const copyTimer = useRef<ReturnType<typeof setTimeout> | null>(null)
  const mounted = useRef(false)

  const workspaces = useWorkspace(state => state.workspaces)
  useWorkspace(selectOpenSessionsKey)
  const binKind = trashBinKind(result?.hostPlatform)
  const tBin = useCallback<TrashText>(
    (key, options) => t(trashTextKey(key, binKind), options),
    [binKind, t],
  )

  const load = useCallback(async (refresh: boolean, background = false) => {
    const seq = ++requestSeq.current
    listsInFlight.current++
    if (!background) {
      setLoading(true)
      setLoadError(null)
    }
    try {
      const next = await host.sessionHistory.list(sessionHistoryListOptions(settingsStore.getSettings(), refresh))
      if (seq !== requestSeq.current) return
      setResult(next)
      setNow(Date.now())
      if (background) {
        setLoadError(null)
        return
      }
      logHistory('list ok', {
        refresh,
        sessions: next.sessions.length,
        groups: next.groups.length,
        fileCount: next.scan.fileCount,
        durationMs: next.scan.durationMs,
      })
    } catch (error) {
      if (seq !== requestSeq.current) return
      const message = errorText(error)
      setLoadError(message)
      logHistory('list failed', { refresh, background, error: message })
    } finally {
      listsInFlight.current--
      if (!background && seq === requestSeq.current) setLoading(false)
    }
  }, [])

  const isBusy = useCallback(() => listsInFlight.current > 0 || trashBusyRef.current, [])

  useEffect(() => {
    mounted.current = true
    return () => {
      mounted.current = false
      requestSeq.current++
      // A remount discards in-flight results, so the next activation must load in the foreground again.
      activatedBefore.current = false
    }
  }, [])

  useEffect(() => {
    if (!active) return undefined
    const action = sessionHistoryActivationLoad({ activatedBefore: activatedBefore.current, busy: isBusy() })
    activatedBefore.current = true
    if (action === 'initial') void load(false)
    else if (action === 'background') void load(false, true)
    return startSessionHistoryPolling({
      poll: () => { void load(false, true) },
      isBusy,
      isHidden: () => document.hidden,
    })
  }, [active, isBusy, load])

  useEffect(() => () => {
    if (copyTimer.current) clearTimeout(copyTimer.current)
  }, [])

  useEffect(() => {
    trashDialogRef.current = trashDialog
  }, [trashDialog])

  const cancelTrash = useCallback(() => {
    if (trashDialogDismissAction(trashDialogRef.current) === 'close-dialog') setTrashDialog(null)
  }, [])

  const trashDialogOpen = trashDialog !== null
  useEffect(() => (trashDialogOpen ? listenForDialogEscape(window, cancelTrash) : undefined), [trashDialogOpen, cancelTrash])

  // Confirming disables both dialog buttons; without this, focus falls back to the page behind the dialog.
  const trashConfirming = trashDialog?.confirming ?? false
  useEffect(() => {
    if (trashConfirming) trashDialogElement.current?.focus()
  }, [trashConfirming])

  const keepFocusInDialog = useCallback((event: ReactKeyboardEvent<HTMLDivElement>) => {
    if (event.key !== 'Tab') return
    const buttons = Array.from(event.currentTarget.querySelectorAll<HTMLButtonElement>('button:not(:disabled)'))
    const focused = document.activeElement
    if (buttons.length === 0) {
      event.preventDefault()
    } else if (event.shiftKey && (focused === buttons[0] || focused === event.currentTarget)) {
      event.preventDefault()
      buttons[buttons.length - 1].focus()
    } else if (!event.shiftKey && focused === buttons[buttons.length - 1]) {
      event.preventDefault()
      buttons[0].focus()
    }
  }, [])

  const setTrashBusyState = useCallback((busy: boolean) => {
    trashBusyRef.current = busy
    setTrashBusy(busy)
  }, [])

  const requestTrash = useCallback(async (visible: readonly SessionHistoryEntry[]) => {
    if (trashBusyRef.current) return
    const selection = selectTrashableSessions(visible, workspaceStore)
    if (selection.sessions.length === 0) return
    setActionError(null)
    setTrashOutcome(null)
    setTrashBusyState(true)
    try {
      const preview = await previewTrashInBatches(selection.sessions, batch => host.sessionHistory.trashPreview(batch))
      logHistory('trash preview', {
        requested: selection.sessions.length,
        skippedOpen: selection.skippedOpen,
        blocked: preview.blockedCount,
        fileCount: preview.fileCount,
        totalBytes: preview.totalBytes,
      })
      setTrashDialog({ preview, skippedOpen: selection.skippedOpen, confirming: false })
    } catch (error) {
      const message = errorText(error)
      setActionError(tBin('trashFailed', { error: message }))
      logHistory('trash preview failed', { requested: selection.sessions.length, error: message })
    } finally {
      setTrashBusyState(false)
    }
  }, [setTrashBusyState, tBin])

  const confirmTrash = useCallback(async () => {
    const dialog = trashDialogRef.current
    if (!dialog || dialog.confirming) return
    const sessions: SessionRef[] = trashableRefsFromPreview(dialog.preview)
    if (sessions.length === 0) return
    const confirming = { ...dialog, confirming: true }
    trashDialogRef.current = confirming
    setTrashDialog(confirming)
    setTrashBusyState(true)
    try {
      const { result: merged, batches, rejected } = await trashInBatches(
        sessions,
        batch => host.sessionHistory.trash(batch),
        t('sessionHistory.trashNotAttempted'),
      )
      const summary = summarizeTrashResult(merged)
      setTrashOutcome({ summary, result: merged })
      logHistory('trash done', {
        requested: sessions.length,
        batches,
        rejected,
        trashed: summary.trashed,
        blocked: summary.blocked,
        failed: summary.failed,
      })
    } catch (error) {
      const message = errorText(error)
      setActionError(tBin('trashFailed', { error: message }))
      logHistory('trash failed', { requested: sessions.length, error: message })
    } finally {
      trashDialogRef.current = null
      setTrashDialog(null)
      setTrashBusyState(false)
      if (mounted.current) void load(false)
    }
  }, [load, setTrashBusyState, t, tBin])

  const caseInsensitive = result?.caseInsensitivePaths ?? true
  const view = useMemo(() => (
    result
      ? buildSessionHistoryView(result, { query, agent: agentFilter, sort, onlyNotAdded }, workspaces)
      : []
  ), [result, query, agentFilter, sort, onlyNotAdded, workspaces])
  const sessionTitles = useMemo(() => new Map(
    (result?.sessions ?? []).map(entry => [sessionRefKey(entry), entry.title] as const),
  ), [result])
  const sessionLabel = (ref: SessionRef) => sessionTitles.get(sessionRefKey(ref)) ?? ref.id

  const toggleGroup = useCallback((key: string) => {
    setToggledGroups(prev => {
      const next = new Set(prev)
      if (next.has(key)) next.delete(key)
      else next.add(key)
      return next
    })
  }, [])

  const handleAddWorkspace = useCallback((group: SessionHistoryGroup) => {
    setActionError(null)
    const workspace = ensureWorkspace(workspaceStore, group, caseInsensitive, detachedWorkspaceIds)
    if (!workspace) {
      setActionError(t('sessionHistory.resumeNoWorkspace'))
      logHistory('add workspace rejected', { groupKey: group.key, kind: group.kind })
      return
    }
    logHistory('workspace ensured', { groupKey: group.key, workspaceId: workspace.id })
  }, [caseInsensitive, detachedWorkspaceIds, t])

  const handleGoToWorkspace = useCallback((workspace: Workspace) => {
    if (trashBusyRef.current || detachedWorkspaceIds.has(workspace.id)) return
    workspaceStore.setActiveWorkspace(workspace.id)
    onWorkspaceShown()
  }, [detachedWorkspaceIds, onWorkspaceShown])

  const handleResume = useCallback((entry: SessionHistoryEntry, group: SessionHistoryGroup) => {
    if (trashBusyRef.current) return
    setActionError(null)
    const outcome = resumeInWorkspace(workspaceStore, entry, group, caseInsensitive, detachedWorkspaceIds)
    logHistory('resume', { sessionId: entry.id, agent: entry.agent, outcome: outcome.kind })
    if (outcome.kind === 'unavailable') {
      setActionError(resumeReasonText(outcome.reason, entry, t))
      return
    }
    onWorkspaceShown()
  }, [caseInsensitive, detachedWorkspaceIds, onWorkspaceShown, t])

  const handleCopy = useCallback((key: string, text: string) => {
    navigator.clipboard.writeText(text).then(() => {
      setCopiedKey(key)
      if (copyTimer.current) clearTimeout(copyTimer.current)
      copyTimer.current = setTimeout(() => setCopiedKey(null), COPY_FEEDBACK_MS)
    }).catch(error => {
      logHistory('copy failed', { error: errorText(error) })
    })
  }, [])

  // The active agent panel handles Escape on window (deny permission, interrupt the turn); keep it in the search box.
  const handleSearchKeyDown = useCallback((event: ReactKeyboardEvent<HTMLInputElement>) => {
    if (event.key !== 'Escape') return
    event.stopPropagation()
    setQuery('')
  }, [])

  const handleCardKeyDown = useCallback((event: ReactKeyboardEvent<HTMLDivElement>, id: string) => {
    if (event.target !== event.currentTarget) return
    if (event.key !== 'Enter' && event.key !== ' ') return
    event.preventDefault()
    setExpandedId(prev => (prev === id ? null : id))
  }, [])

  const scan = result?.scan
  const hasSessions = (result?.sessions.length ?? 0) > 0
  const trimmedQuery = query.trim()
  let scanText = ''
  if (loading && result) {
    scanText = t('sessionHistory.scanning')
  } else if (!loading && scan) {
    const parts = [t('sessionHistory.scanSummary', { count: scan.fileCount, time: formatRelativeTime(scan.scannedAt, now, t) })]
    if (scan.skippedLines > 0) parts.push(t('sessionHistory.skippedLines', { count: scan.skippedLines }))
    if (scan.failedCount > 0) parts.push(t('sessionHistory.failedFiles', { count: scan.failedCount }))
    scanText = parts.join(' · ')
  }

  const renderCard = (entry: SessionHistoryEntry, group: SessionHistoryGroup, workspace: Workspace | undefined) => {
    const expanded = expandedId === entry.id
    const owner = workspaceStore.findSdkSessionOwner(entry.id, sessionAgentPreset(entry.agent))
    const detached = owner
      ? detachedWorkspaceIds.has(owner.workspaceId)
      : !!workspace && detachedWorkspaceIds.has(workspace.id)
    const unavailable = resumeUnavailableReason(entry, group) ?? (detached ? 'detached' : null)
    const resumeCommand = sessionResumeCommand(entry)
    const idCopyKey = `id:${entry.id}`
    const commandCopyKey = `cmd:${entry.id}`
    const hasExtraMeta = entry.subagentCount > 0 || !!entry.model || !!entry.gitBranch
    return (
      <div
        key={`${entry.agent}:${entry.id}`}
        className={`session-history-card${expanded ? ' expanded' : ''}${entry.cwdExists ? '' : ' missing'}`}
      >
        <div
          className="session-history-card-summary"
          role="button"
          tabIndex={0}
          aria-expanded={expanded}
          onClick={() => setExpandedId(expanded ? null : entry.id)}
          onKeyDown={event => handleCardKeyDown(event, entry.id)}
        >
          <div className="session-history-card-top">
            <span className={`session-history-agent ${entry.agent}`}>
              {entry.agent === 'claude' ? t('sessionHistory.agentClaude') : t('sessionHistory.agentCodex')}
            </span>
            <span className="session-history-card-title" title={entry.title}>{entry.title}</span>
          </div>
          <div className="session-history-card-meta">
            <span title={formatAbsoluteTime(entry.updatedAt, t)}>{formatRelativeTime(entry.updatedAt, now, t)}</span>
            <span>{t('sessionHistory.messageCount', { count: entry.messageCount })}</span>
            {entry.interactive === false && (
              <span className="session-history-tag">{t('sessionHistory.tagNonInteractive')}</span>
            )}
            {entry.archived === true && (
              <span className="session-history-tag">{t('sessionHistory.tagArchived')}</span>
            )}
            {owner && <span className="session-history-open-badge">{t('sessionHistory.alreadyOpen')}</span>}
          </div>
        </div>
        {expanded && (
          <div className="session-history-card-detail">
            {entry.lastReply && <div className="session-history-card-reply">{entry.lastReply}</div>}
            {hasExtraMeta && (
              <div className="session-history-card-meta">
                {entry.subagentCount > 0 && (
                  <span>{t('sessionHistory.subagentCount', { count: entry.subagentCount })}</span>
                )}
                {entry.model && <span>{entry.model}</span>}
                {entry.gitBranch && <span>⎇ {entry.gitBranch}</span>}
              </div>
            )}
            <dl>
              {entry.firstPrompt && (
                <>
                  <dt>{t('sessionHistory.firstPrompt')}</dt>
                  <dd>{entry.firstPrompt}</dd>
                </>
              )}
              <dt>{t('sessionHistory.cwd')}</dt>
              <dd>{entry.cwd ?? '—'}</dd>
              <dt>{t('sessionHistory.sessionId')}</dt>
              <dd>{entry.id}</dd>
              <dt>{t('sessionHistory.startedUpdated')}</dt>
              <dd>{formatAbsoluteTime(entry.startedAt, t)} → {formatAbsoluteTime(entry.updatedAt, t)}</dd>
            </dl>
            <div className="session-history-card-actions">
              <button
                type="button"
                className="session-history-btn primary"
                disabled={!!unavailable}
                title={unavailable ? resumeReasonText(unavailable, entry, t) : undefined}
                onClick={() => handleResume(entry, group)}
              >
                {t('sessionHistory.resume')}
              </button>
              <button type="button" className="session-history-btn" onClick={() => handleCopy(idCopyKey, entry.id)}>
                {copiedKey === idCopyKey ? t('sessionHistory.copied') : t('sessionHistory.copyId')}
              </button>
              <button
                type="button"
                className="session-history-btn"
                disabled={!resumeCommand}
                title={resumeCommand ? resumeCommandHint(entry, t) : t('sessionHistory.resumeCommandInvalidId')}
                onClick={() => { if (resumeCommand) handleCopy(commandCopyKey, resumeCommand) }}
              >
                {copiedKey === commandCopyKey ? t('sessionHistory.copied') : t('sessionHistory.copyResumeCommand')}
              </button>
              <button
                type="button"
                className="session-history-btn danger"
                disabled={!!owner || trashBusy}
                title={owner ? tBin('trashOpenInTerminal') : undefined}
                onClick={() => void requestTrash([entry])}
              >
                {tBin('moveToTrash')}
              </button>
            </div>
            {unavailable && (
              <div className="session-history-card-hint">{resumeReasonText(unavailable, entry, t)}</div>
            )}
            {copiedKey === commandCopyKey && (
              <div className="session-history-card-hint">{resumeCommandHint(entry, t)}</div>
            )}
          </div>
        )}
      </div>
    )
  }

  const renderGroupStatus = (group: SessionHistoryGroup, workspace: Workspace | undefined) => {
    if (workspace) {
      if (detachedWorkspaceIds.has(workspace.id)) {
        return (
          <span
            className="session-history-status-icon"
            role="img"
            aria-label={t('sessionHistory.inDetachedWindow')}
            title={t('sessionHistory.resumeDetached')}
          >
            {ICON_WINDOW}
          </span>
        )
      }
      const goLabel = `${t('sessionHistory.inWorkspace')} · ${t('sessionHistory.goToWorkspace')}`
      return (
        <button
          type="button"
          className="session-history-icon-btn ok"
          title={goLabel}
          aria-label={goLabel}
          onClick={() => handleGoToWorkspace(workspace)}
        >
          {ICON_GO}
        </button>
      )
    }
    if (group.kind === 'missing') return null
    const addable = canAddGroupAsWorkspace(group)
    return (
      <button
        type="button"
        className="session-history-icon-btn primary"
        disabled={!addable}
        title={addable ? t('sessionHistory.addWorkspace') : t('sessionHistory.cannotAddWorkspace')}
        aria-label={t('sessionHistory.addWorkspace')}
        onClick={() => handleAddWorkspace(group)}
      >
        {ICON_PLUS}
      </button>
    )
  }

  const renderGroup = (group: SessionHistoryGroup, sessions: SessionHistoryEntry[], workspace: Workspace | undefined) => {
    const collapsed = isDefaultCollapsed(group, query.trim() !== '') !== toggledGroups.has(group.key)
    const trashable = group.kind === 'missing' ? selectTrashableSessions(sessions, workspaceStore).sessions.length : 0
    const countText = group.worktreeCount > 0
      ? `${t('sessionHistory.sessionCount', { count: sessions.length })} · ${t('sessionHistory.worktreeCount', { count: group.worktreeCount })}`
      : t('sessionHistory.sessionCount', { count: sessions.length })
    return (
      <div key={group.key} className={`session-history-group${group.kind === 'missing' ? ' missing' : ''}`}>
        <div className="session-history-group-head">
          <button
            type="button"
            className="session-history-group-toggle"
            aria-expanded={!collapsed}
            title={group.rootPath ?? undefined}
            onClick={() => toggleGroup(group.key)}
          >
            <span className="session-history-caret">{collapsed ? '▸' : '▾'}</span>
            <span className="session-history-group-text">
              <span className="session-history-group-name">
                {group.kind === 'unknown' ? t('sessionHistory.unknownFolder') : group.name}
              </span>
              {group.rootPath && <span className="session-history-group-path">{group.rootPath}</span>}
            </span>
          </button>
          <span className="session-history-count" title={countText} aria-label={countText}>{sessions.length}</span>
          {group.kind === 'missing' && (
            <>
              <span
                className="session-history-status-icon warn"
                role="img"
                aria-label={t('sessionHistory.folderMissing')}
                title={t('sessionHistory.folderMissing')}
              >
                ⚠
              </span>
              <button
                type="button"
                className="session-history-icon-btn danger"
                disabled={trashable === 0 || trashBusy}
                title={trashable === 0
                  ? t('sessionHistory.trashNothingVisible')
                  : `${tBin('moveAllToTrash')} ${t('sessionHistory.moveAllToTrashHint', { count: trashable })}`}
                aria-label={tBin('moveAllToTrash')}
                onClick={() => void requestTrash(sessions)}
              >
                {ICON_TRASH}
              </button>
            </>
          )}
          {renderGroupStatus(group, workspace)}
        </div>
        {!collapsed && (
          <div className="session-history-cards">
            {sessions.map(entry => renderCard(entry, group, workspace))}
          </div>
        )}
      </div>
    )
  }

  const renderBody = () => {
    if (!result) {
      if (loadError) {
        return (
          <div className="session-history-state error">
            <p>{t('sessionHistory.loadFailed', { error: loadError })}</p>
            <button type="button" className="session-history-btn" onClick={() => void load(false)}>
              {t('common.retry')}
            </button>
          </div>
        )
      }
      return (
        <div className="session-history-state">
          <p>{t('sessionHistory.scanning')}</p>
          <div className="session-history-skeleton" />
          <div className="session-history-skeleton" />
          <div className="session-history-skeleton short" />
        </div>
      )
    }
    if (!hasSessions) {
      return <div className="session-history-state"><p>{t('sessionHistory.emptyHistory')}</p></div>
    }
    if (view.length === 0) {
      return (
        <div className="session-history-state">
          {trimmedQuery ? (
            <>
              <p>{t('sessionHistory.emptySearch', { query: trimmedQuery })}</p>
              <p className="session-history-muted">{t('sessionHistory.emptySearchHint')}</p>
            </>
          ) : (
            <p>{t('sessionHistory.emptyFiltered')}</p>
          )}
        </div>
      )
    }
    return view.map(item => renderGroup(item.group, item.sessions, item.workspace))
  }

  const renderTrashDialog = (dialog: TrashDialogState) => {
    const { preview, skippedOpen, confirming } = dialog
    const trashableCount = trashableRefsFromPreview(preview).length
    const blockedItems = preview.items.filter(item => item.blockedReason !== null)
    return (
      <div
        className="session-history-dialog-backdrop"
        onMouseDown={event => { if (event.target === event.currentTarget) event.preventDefault() }}
        onClick={cancelTrash}
      >
        <div
          ref={trashDialogElement}
          className="session-history-dialog"
          role="alertdialog"
          aria-modal="true"
          aria-labelledby="session-history-trash-title"
          tabIndex={-1}
          onKeyDown={keepFocusInDialog}
          onClick={event => event.stopPropagation()}
        >
          <h3 id="session-history-trash-title">{tBin('trashDialogTitle')}</h3>
          <p>
            {trashableCount > 0
              ? tBin('trashDialogSummary', {
                count: trashableCount,
                files: preview.fileCount,
                size: formatByteSize(preview.totalBytes),
              })
              : t('sessionHistory.trashDialogNothing')}
          </p>
          <p className="session-history-muted">
            {remoteHostLabel
              ? tBin('trashDialogRemote', { host: remoteHostLabel })
              : tBin('trashDialogLocal')}
          </p>
          {skippedOpen > 0 && (
            <p className="session-history-muted">{t('sessionHistory.trashDialogSkippedOpen', { count: skippedOpen })}</p>
          )}
          {blockedItems.length > 0 && (
            <>
              <p>{t('sessionHistory.trashDialogBlocked', { count: blockedItems.length })}</p>
              <ul className="session-history-dialog-list">
                {blockedItems.map(item => (
                  <li key={sessionRefKey(item)}>
                    <span className="session-history-dialog-item-title">{sessionLabel(item)}</span>
                    <span className="session-history-dialog-item-reason">
                      {item.blockedReason && trashBlockReasonText(item.blockedReason, t)}
                    </span>
                  </li>
                ))}
              </ul>
            </>
          )}
          <div className="session-history-dialog-actions">
            <button type="button" className="session-history-btn" autoFocus disabled={confirming} onClick={cancelTrash}>
              {t('common.cancel')}
            </button>
            <button
              type="button"
              className="session-history-btn danger"
              disabled={confirming || trashableCount === 0}
              onClick={() => void confirmTrash()}
            >
              {confirming ? t('sessionHistory.trashing') : tBin('trashConfirm')}
            </button>
          </div>
        </div>
      </div>
    )
  }

  const renderTrashOutcome = ({ summary, result: outcome }: TrashOutcome) => (
    <div className={`session-history-banner ${summary.tone}`}>
      <div className="session-history-banner-row">
        <span>{trashSummaryText(summary, t, tBin)}</span>
        <button
          type="button"
          className="session-history-banner-dismiss"
          onClick={() => setTrashOutcome(null)}
          title={t('sessionHistory.dismiss')}
          aria-label={t('sessionHistory.dismiss')}
        >
          &times;
        </button>
      </div>
      {(outcome.blocked.length > 0 || outcome.failed.length > 0) && (
        <ul className="session-history-banner-list">
          {outcome.blocked.map(item => (
            <li key={`blocked:${sessionRefKey(item)}`}>
              {sessionLabel(item)}: {trashBlockReasonText(item.reason, t)}
            </li>
          ))}
          {outcome.failed.map(item => (
            <li key={`failed:${sessionRefKey(item)}`}>{sessionLabel(item)}: {item.error}</li>
          ))}
        </ul>
      )}
    </div>
  )

  return (
    <div className="session-history-sidebar" role="region" hidden={!active} aria-label={t('sessionHistory.title')}>
      <div className="session-history-toolbar">
        <input
          className="session-history-search"
          type="search"
          value={query}
          placeholder={t('sessionHistory.searchPlaceholder')}
          title={t('sessionHistory.searchPlaceholder')}
          onChange={event => setQuery(event.target.value)}
          onKeyDown={handleSearchKeyDown}
        />
        <div className="session-history-filters">
          <select
            className="session-history-select"
            value={agentFilter}
            aria-label={t('sessionHistory.agentFilter')}
            title={t('sessionHistory.agentFilter')}
            onChange={event => setAgentFilter(event.target.value as SessionHistoryAgentFilter)}
          >
            <option value="all">{t('sessionHistory.agentAll')}</option>
            <option value="claude">{t('sessionHistory.agentClaude')}</option>
            <option value="codex">{t('sessionHistory.agentCodex')}</option>
          </select>
          <select
            className="session-history-select"
            value={sort}
            aria-label={t('sessionHistory.sortLabel')}
            title={t('sessionHistory.sortLabel')}
            onChange={event => setSort(event.target.value as SessionHistorySort)}
          >
            <option value="updated">{t('sessionHistory.sortUpdated')}</option>
            <option value="started">{t('sessionHistory.sortStarted')}</option>
            <option value="messages">{t('sessionHistory.sortMessages')}</option>
          </select>
          <button
            type="button"
            className={`session-history-icon-btn${onlyNotAdded ? ' active' : ''}`}
            aria-pressed={onlyNotAdded}
            title={t('sessionHistory.onlyNotAdded')}
            aria-label={t('sessionHistory.onlyNotAdded')}
            onClick={() => setOnlyNotAdded(value => !value)}
          >
            {ICON_FILTER}
          </button>
        </div>
        <div className="session-history-status">
          {remoteHostLabel && (
            <span className="session-history-chip remote" title={remoteOrigin ?? remoteHostLabel}>
              {t('sessionHistory.remoteHost', { host: remoteHostLabel })}
            </span>
          )}
          <span className="session-history-scan" title={scanText || undefined}>{scanText}</span>
          <button
            type="button"
            className={`session-history-icon-btn${loading ? ' spinning' : ''}`}
            onClick={() => void load(true)}
            disabled={loading}
            title={t('sessionHistory.refresh')}
            aria-label={t('sessionHistory.refresh')}
          >
            ⟳
          </button>
        </div>
      </div>
      {result && loadError && (
        <div className="session-history-banner error">{t('sessionHistory.loadFailed', { error: loadError })}</div>
      )}
      {scan && !scan.gitAvailable && (
        <div className="session-history-banner warn">⚠ {t('sessionHistory.gitUnavailable')}</div>
      )}
      {actionError && <div className="session-history-banner error">{actionError}</div>}
      {trashOutcome && renderTrashOutcome(trashOutcome)}
      <div className="session-history-list">
        {renderBody()}
      </div>
      {trashDialog && createPortal(renderTrashDialog(trashDialog), document.body)}
    </div>
  )
}
