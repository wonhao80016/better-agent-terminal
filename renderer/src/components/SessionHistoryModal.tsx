import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import type { KeyboardEvent as ReactKeyboardEvent } from 'react'
import { useTranslation } from 'react-i18next'
import type { TFunction } from 'i18next'
import { host } from '../host-api'
import { useWorkspace, workspaceStore } from '../stores/workspace-store'
import type { AppState, Workspace } from '../types'
import type {
  SessionHistoryEntry,
  SessionHistoryGroup,
  SessionHistoryResult,
} from '../types/session-history'
import {
  buildSessionHistoryView,
  canAddGroupAsWorkspace,
  ensureWorkspace,
  listenForModalEscape,
  resumeInWorkspace,
  resumeUnavailableReason,
  sessionAgentPreset,
  sessionResumeCommand,
  type ResumeUnavailableReason,
  type SessionHistoryAgentFilter,
  type SessionHistorySort,
} from '../utils/session-history-actions'
import '../styles/session-history.css'

interface SessionHistoryModalProps {
  onClose: () => void
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

function isDefaultCollapsed(group: SessionHistoryGroup): boolean {
  return group.kind === 'missing'
}

export function SessionHistoryModal({
  onClose,
  remoteHostLabel,
  remoteOrigin,
  detachedWorkspaceIds = NO_DETACHED_WORKSPACES,
}: Readonly<SessionHistoryModalProps>) {
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
  const requestSeq = useRef(0)
  const copyTimer = useRef<ReturnType<typeof setTimeout> | null>(null)

  const workspaces = useWorkspace(state => state.workspaces)
  useWorkspace(selectOpenSessionsKey)

  const load = useCallback(async (refresh: boolean) => {
    const seq = ++requestSeq.current
    setLoading(true)
    setLoadError(null)
    try {
      const next = await host.sessionHistory.list(refresh ? { refresh: true } : undefined)
      if (seq !== requestSeq.current) return
      setResult(next)
      setNow(Date.now())
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
      logHistory('list failed', { refresh, error: message })
    } finally {
      if (seq === requestSeq.current) setLoading(false)
    }
  }, [])

  useEffect(() => {
    void load(false)
    return () => { requestSeq.current++ }
  }, [load])

  useEffect(() => () => {
    if (copyTimer.current) clearTimeout(copyTimer.current)
  }, [])

  useEffect(() => listenForModalEscape(window, onClose), [onClose])

  const caseInsensitive = result?.caseInsensitivePaths ?? true
  const view = useMemo(() => (
    result
      ? buildSessionHistoryView(result, { query, agent: agentFilter, sort, onlyNotAdded }, workspaces)
      : []
  ), [result, query, agentFilter, sort, onlyNotAdded, workspaces])

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
    if (detachedWorkspaceIds.has(workspace.id)) return
    workspaceStore.setActiveWorkspace(workspace.id)
    onClose()
  }, [detachedWorkspaceIds, onClose])

  const handleResume = useCallback((entry: SessionHistoryEntry, group: SessionHistoryGroup) => {
    setActionError(null)
    const outcome = resumeInWorkspace(workspaceStore, entry, group, caseInsensitive, detachedWorkspaceIds)
    logHistory('resume', { sessionId: entry.id, agent: entry.agent, outcome: outcome.kind })
    if (outcome.kind === 'unavailable') {
      setActionError(resumeReasonText(outcome.reason, entry, t))
      return
    }
    onClose()
  }, [caseInsensitive, detachedWorkspaceIds, onClose, t])

  const handleCopy = useCallback((key: string, text: string) => {
    navigator.clipboard.writeText(text).then(() => {
      setCopiedKey(key)
      if (copyTimer.current) clearTimeout(copyTimer.current)
      copyTimer.current = setTimeout(() => setCopiedKey(null), COPY_FEEDBACK_MS)
    }).catch(error => {
      logHistory('copy failed', { error: errorText(error) })
    })
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
            {owner && <span className="session-history-open-badge">{t('sessionHistory.alreadyOpen')}</span>}
            <span className="session-history-card-time" title={formatAbsoluteTime(entry.updatedAt, t)}>
              {formatRelativeTime(entry.updatedAt, now, t)}
            </span>
          </div>
          {entry.lastReply && <div className="session-history-card-reply">{entry.lastReply}</div>}
          <div className="session-history-card-meta">
            <span>{t('sessionHistory.messageCount', { count: entry.messageCount })}</span>
            {entry.subagentCount > 0 && (
              <span>{t('sessionHistory.subagentCount', { count: entry.subagentCount })}</span>
            )}
            {entry.model && <span>{entry.model}</span>}
            {entry.gitBranch && <span>⎇ {entry.gitBranch}</span>}
          </div>
        </div>
        {expanded && (
          <div className="session-history-card-detail">
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

  const renderGroup = (group: SessionHistoryGroup, sessions: SessionHistoryEntry[], workspace: Workspace | undefined) => {
    const collapsed = isDefaultCollapsed(group) !== toggledGroups.has(group.key)
    const addable = canAddGroupAsWorkspace(group)
    const detached = !!workspace && detachedWorkspaceIds.has(workspace.id)
    return (
      <div key={group.key} className={`session-history-group${group.kind === 'missing' ? ' missing' : ''}`}>
        <div className="session-history-group-head">
          <button
            type="button"
            className="session-history-group-toggle"
            aria-expanded={!collapsed}
            onClick={() => toggleGroup(group.key)}
          >
            <span className="session-history-caret">{collapsed ? '▸' : '▾'}</span>
            <span className="session-history-group-name">
              {group.kind === 'unknown' ? t('sessionHistory.unknownFolder') : group.name}
            </span>
            {group.rootPath && (
              <span className="session-history-group-path" title={group.rootPath}>{group.rootPath}</span>
            )}
          </button>
          <span className="session-history-count">
            {t('sessionHistory.sessionCount', { count: sessions.length })}
            {group.worktreeCount > 0 && ` · ${t('sessionHistory.worktreeCount', { count: group.worktreeCount })}`}
          </span>
          {group.kind === 'missing' && (
            <span className="session-history-badge warn">⚠ {t('sessionHistory.folderMissing')}</span>
          )}
          {workspace ? (
            <>
              <span className="session-history-badge ok">
                ✓ {detached ? t('sessionHistory.inDetachedWindow') : t('sessionHistory.inWorkspace')}
              </span>
              <button
                type="button"
                className="session-history-btn"
                disabled={detached}
                title={detached ? t('sessionHistory.resumeDetached') : undefined}
                onClick={() => handleGoToWorkspace(workspace)}
              >
                {t('sessionHistory.goToWorkspace')}
              </button>
            </>
          ) : (
            <button
              type="button"
              className="session-history-btn primary"
              disabled={!addable}
              title={addable ? undefined : t('sessionHistory.cannotAddWorkspace')}
              onClick={() => handleAddWorkspace(group)}
            >
              {t('sessionHistory.addWorkspace')}
            </button>
          )}
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

  return (
    <div className="settings-overlay" onClick={onClose}>
      <div
        className="settings-panel session-history-panel"
        role="dialog"
        aria-modal="true"
        aria-label={t('sessionHistory.title')}
        onClick={event => event.stopPropagation()}
      >
        <div className="settings-header session-history-header">
          <h2>{t('sessionHistory.title')}</h2>
          {remoteHostLabel && (
            <span className="session-history-chip remote" title={remoteOrigin ?? undefined}>
              {t('sessionHistory.remoteHost', { host: remoteHostLabel })}
            </span>
          )}
          <span className="session-history-scan">
            {loading && result && t('sessionHistory.scanning')}
            {!loading && scan && (
              <>
                {t('sessionHistory.scanSummary', { count: scan.fileCount, time: formatRelativeTime(scan.scannedAt, now, t) })}
                {scan.skippedLines > 0 && ` · ${t('sessionHistory.skippedLines', { count: scan.skippedLines })}`}
                {scan.failedCount > 0 && ` · ${t('sessionHistory.failedFiles', { count: scan.failedCount })}`}
              </>
            )}
          </span>
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
          <button
            type="button"
            className="settings-close"
            onClick={onClose}
            title={t('sessionHistory.close')}
            aria-label={t('sessionHistory.close')}
          >
            &times;
          </button>
        </div>
        <div className="session-history-toolbar">
          <input
            className="session-history-search"
            type="search"
            value={query}
            autoFocus
            placeholder={t('sessionHistory.searchPlaceholder')}
            onChange={event => setQuery(event.target.value)}
          />
          <div className="session-history-segment" role="group" aria-label={t('sessionHistory.agentFilter')}>
            {(['all', 'claude', 'codex'] as const).map(value => (
              <button
                key={value}
                type="button"
                className={agentFilter === value ? 'active' : ''}
                aria-pressed={agentFilter === value}
                onClick={() => setAgentFilter(value)}
              >
                {value === 'all'
                  ? t('sessionHistory.agentAll')
                  : value === 'claude' ? t('sessionHistory.agentClaude') : t('sessionHistory.agentCodex')}
              </button>
            ))}
          </div>
          <label className="session-history-sort">
            <span>{t('sessionHistory.sortLabel')}</span>
            <select value={sort} onChange={event => setSort(event.target.value as SessionHistorySort)}>
              <option value="updated">{t('sessionHistory.sortUpdated')}</option>
              <option value="started">{t('sessionHistory.sortStarted')}</option>
              <option value="messages">{t('sessionHistory.sortMessages')}</option>
            </select>
          </label>
          <label className="session-history-toggle">
            <input
              type="checkbox"
              checked={onlyNotAdded}
              onChange={event => setOnlyNotAdded(event.target.checked)}
            />
            <span>{t('sessionHistory.onlyNotAdded')}</span>
          </label>
        </div>
        {result && loadError && (
          <div className="session-history-banner error">{t('sessionHistory.loadFailed', { error: loadError })}</div>
        )}
        {scan && !scan.gitAvailable && (
          <div className="session-history-banner warn">⚠ {t('sessionHistory.gitUnavailable')}</div>
        )}
        {actionError && <div className="session-history-banner error">{actionError}</div>}
        <div className="session-history-list">
          {renderBody()}
        </div>
      </div>
    </div>
  )
}
