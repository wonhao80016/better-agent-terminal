import { host } from '../host-api'
import { Fragment, useState, useEffect, useCallback, useMemo, useRef, type CSSProperties } from 'react'
import { useTranslation } from 'react-i18next'
import i18next from 'i18next'
import { ResizeHandle } from './ResizeHandle'
import { GitGraphCell, GitGraphToggleIcon, GitRefBadges, graphWidthStyle } from './GitGraph'
import { useResizablePaneWidth } from '../hooks/useResizablePaneWidth'
import { buildInlineFileRows, changeBlockEnd, changeBlockStarts, parseUnifiedDiff, splitContentLines, type DiffRow } from '../utils/unified-diff'
import { buildGraphRows, computeGitGraph, findHeadHash, parseGraphEnabled, parseGraphScope, type GitGraphScope } from '../lib/git-graph'
import { createGraphLogSource, type GraphLogResult } from '../lib/git-graph-source'

interface GitCommit {
  hash: string
  author: string
  date: string
  message: string
}

interface GitStatusEntry {
  status: string
  file: string
}

interface WorktreeEntry {
  path: string
  branch: string
}

interface GitPanelProps {
  workspaceFolderPath: string
  worktreePaths?: WorktreeEntry[]
}

const TEXT_EXTS = new Set([
  'ts', 'tsx', 'js', 'jsx', 'json', 'css', 'scss', 'less', 'html', 'htm',
  'md', 'txt', 'yml', 'yaml', 'toml', 'xml', 'svg', 'sh', 'bash', 'zsh',
  'py', 'rb', 'go', 'rs', 'java', 'c', 'cpp', 'h', 'hpp', 'cs',
  'env', 'gitignore', 'editorconfig', 'prettierrc', 'eslintrc',
  'dockerfile', 'makefile', 'cfg', 'ini', 'conf', 'log',
])

function isTextFile(name: string): boolean {
  const ext = name.split('.').pop()?.toLowerCase() || ''
  return TEXT_EXTS.has(ext)
}

const GRAPH_SCOPE_STORAGE_KEY = 'better-terminal-git-graph-scope'
const GRAPH_ENABLED_STORAGE_KEY = 'better-terminal-git-graph-enabled'
const GRAPH_INITIAL_COUNT = 50
const GRAPH_PAGE_SIZE = 100
const GRAPH_MAX_COUNT = 500
// How long the first paint waits for the graph after the plain list is ready.
const GRAPH_FIRST_PAINT_GRACE_MS = 300
const GRAPH_PENDING = Symbol('graph-pending')

const graphLogSource = createGraphLogSource(
  (cwd, count, scope) => host.git.getGraphLog(cwd, count, scope),
  message => {
    host.debug.log(message).catch(() => undefined)
  },
)

function readStoredGraphScope(): GitGraphScope {
  try {
    return parseGraphScope(localStorage.getItem(GRAPH_SCOPE_STORAGE_KEY))
  } catch {
    return 'current'
  }
}

function writeStoredGraphScope(scope: GitGraphScope): void {
  try {
    localStorage.setItem(GRAPH_SCOPE_STORAGE_KEY, scope)
  } catch {
    // The scope choice is presentation-only; losing it is harmless.
  }
}

function readStoredGraphEnabled(): boolean {
  try {
    return parseGraphEnabled(localStorage.getItem(GRAPH_ENABLED_STORAGE_KEY))
  } catch {
    return true
  }
}

function writeStoredGraphEnabled(enabled: boolean): void {
  try {
    localStorage.setItem(GRAPH_ENABLED_STORAGE_KEY, String(enabled))
  } catch {
    // The graph toggle is presentation-only; losing it is harmless.
  }
}

function settleWithin<T>(promise: Promise<T>, ms: number): Promise<T | typeof GRAPH_PENDING> {
  return Promise.race([promise, new Promise<typeof GRAPH_PENDING>(resolve => setTimeout(() => resolve(GRAPH_PENDING), ms))])
}

function formatDate(dateStr: string): string {
  const legacyGitDate = dateStr.match(/^(\d{4}-\d{2}-\d{2}) (\d{2}:\d{2}:\d{2}) ([+-]\d{2})(\d{2})$/)
  const normalizedDate = legacyGitDate
    ? `${legacyGitDate[1]}T${legacyGitDate[2]}${legacyGitDate[3]}:${legacyGitDate[4]}`
    : dateStr
  try {
    const d = new Date(normalizedDate)
    if (!Number.isFinite(d.getTime())) return dateStr
    const now = new Date()
    const diffMs = now.getTime() - d.getTime()
    const diffMins = Math.floor(diffMs / 60000)
    if (diffMins < 1) return i18next.t('git.justNow')
    if (diffMins < 60) return i18next.t('git.minutesAgo', { count: diffMins })
    const diffHours = Math.floor(diffMins / 60)
    if (diffHours < 24) return i18next.t('git.hoursAgo', { count: diffHours })
    const diffDays = Math.floor(diffHours / 24)
    if (diffDays < 30) return i18next.t('git.daysAgo', { count: diffDays })
    return d.toLocaleDateString()
  } catch {
    return dateStr
  }
}

function statusColor(s: string): string {
  switch (s) {
    case 'M': return '#d97706'
    case 'A': case '??': return '#4ec9b0'
    case 'D': return '#f44336'
    case 'R': return '#569cd6'
    default: return 'var(--text-secondary)'
  }
}

function DiffCode({ row }: { row: DiffRow }) {
  if ((row.kind !== 'add' && row.kind !== 'del') || !row.segments) return <>{row.text || ' '}</>
  const changedClass = row.kind === 'add' ? 'git-diff-word-add' : 'git-diff-word-del'
  return (
    <>
      {row.segments.map((segment, i) => segment.changed
        ? <span key={i} className={changedClass}>{segment.text}</span>
        : <Fragment key={i}>{segment.text}</Fragment>)}
    </>
  )
}

const DIFF_SIGN: Record<DiffRow['kind'], string> = { add: '+', del: '−', context: '', hunk: '', meta: '' }

interface ChangeFocus {
  start: number
  end: number
  seq: number
}

function DiffRowList({ rows, focus = null }: { rows: DiffRow[]; focus?: ChangeFocus | null }) {
  return (
    <div className="git-diff-rows">
      {rows.map((row, i) => {
        const focused = focus !== null && i >= focus.start && i < focus.end
        return (
          // Keying focused rows by seq remounts them so the flash restarts on every navigation.
          <div key={focused ? `${i}-${focus.seq}` : i} className={`git-diff-row git-diff-row-${row.kind}${focused ? ' git-diff-row-focus' : ''}`}>
            <span className="git-diff-ln">{'oldLine' in row ? row.oldLine : ''}</span>
            <span className="git-diff-ln">{'newLine' in row ? row.newLine : ''}</span>
            <span className="git-diff-sign">{DIFF_SIGN[row.kind]}</span>
            <span className="git-diff-code"><DiffCode row={row} /></span>
          </div>
        )
      })}
    </div>
  )
}

function maxLineNumber(rows: DiffRow[]): number {
  return rows.reduce((max, row) => 'oldLine' in row || 'newLine' in row
    ? Math.max(max, row.oldLine ?? 0, row.newLine ?? 0)
    : max, 0)
}

function DiffView({ diff }: { diff: string }) {
  const { t } = useTranslation()
  const rows = useMemo(() => parseUnifiedDiff(diff), [diff])
  if (!diff) {
    return <div className="git-diff-empty">{t('git.selectFileToViewDiff')}</div>
  }

  return (
    <div className="git-diff-content" style={lineNumberGutter(maxLineNumber(rows))}>
      <DiffRowList rows={rows} />
    </div>
  )
}

function lineNumberGutter(maxLine: number): CSSProperties {
  return { '--git-diff-ln-width': `${Math.max(2, String(maxLine).length)}ch` } as CSSProperties
}

function newFileDiff(filePath: string, content: string): string {
  const contentLines = content.replace(/\r?\n$/, '').split('\n')
  const lines = contentLines.map(line => '+' + line).join('\n')
  return `diff --git a/${filePath} b/${filePath}\nnew file\n--- /dev/null\n+++ b/${filePath}\n@@ -0,0 +1,${contentLines.length} @@\n${lines}`
}

function FileContentView({ content, rows, focus }: { content: string; rows: DiffRow[] | null; focus: ChangeFocus | null }) {
  const plainLines = useMemo(() => rows ? [] : splitContentLines(content), [rows, content])
  const containerRef = useRef<HTMLDivElement>(null)

  useEffect(() => {
    const container = containerRef.current
    const target = container?.querySelector<HTMLElement>('.git-diff-row-focus')
    if (!container || !target) return
    const top = target.getBoundingClientRect().top - container.getBoundingClientRect().top + container.scrollTop
    container.scrollTop = top - container.clientHeight / 3
  }, [focus])

  if (rows) {
    return (
      <div className="git-file-content" ref={containerRef} style={lineNumberGutter(maxLineNumber(rows))}>
        <DiffRowList rows={rows} focus={focus} />
      </div>
    )
  }

  return (
    <div className="git-file-content" style={lineNumberGutter(plainLines.length)}>
      <div className="git-diff-rows">
        {plainLines.map((line, i) => (
          <div key={i} className="git-diff-row">
            <span className="git-diff-ln">{i + 1}</span>
            <span className="git-diff-code">{line || ' '}</span>
          </div>
        ))}
      </div>
    </div>
  )
}

function ArrowIcon({ direction }: { direction: 'up' | 'down' }) {
  return (
    <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
      <path d={direction === 'up' ? 'M8 13V3M4 7l4-4 4 4' : 'M8 3v10M4 9l4 4 4-4'} />
    </svg>
  )
}

export function GitPanel({ workspaceFolderPath, worktreePaths = [] }: Readonly<GitPanelProps>) {
  const { t } = useTranslation()
  const [commits, setCommits] = useState<GitCommit[]>([])
  const [status, setStatus] = useState<GitStatusEntry[]>([])
  const [selectedCommit, setSelectedCommit] = useState<string | null>(null)
  const [changedFiles, setChangedFiles] = useState<GitStatusEntry[]>([])
  const [selectedFile, setSelectedFile] = useState<string | null>(null)
  const [diff, setDiff] = useState('')
  const [fileDiff, setFileDiff] = useState('')
  const [fileContent, setFileContent] = useState<string | null>(null)
  const [viewMode, setViewMode] = useState<'diff' | 'file'>('diff')
  const [loading, setLoading] = useState(true)
  const [filesLoading, setFilesLoading] = useState(false)
  const [diffLoading, setDiffLoading] = useState(false)
  const [isGitRepo, setIsGitRepo] = useState(true)
  const [gitRoot, setGitRoot] = useState<string | null>(null)
  const [activePath, setActivePath] = useState(workspaceFolderPath)
  const [graph, setGraph] = useState<GraphLogResult | null>(null)
  const [graphCount, setGraphCount] = useState(GRAPH_INITIAL_COUNT)
  const [graphBusy, setGraphBusy] = useState(false)
  const [initialGraphScope] = useState(readStoredGraphScope)
  const graphScopeRef = useRef<GitGraphScope>(initialGraphScope)
  const [graphEnabled, setGraphEnabled] = useState(readStoredGraphEnabled)
  const graphEnabledRef = useRef(graphEnabled)
  const graphCountRef = useRef(GRAPH_INITIAL_COUNT)
  const graphPathRef = useRef<string | null>(null)
  // loadData owns the plain list; graph-only reloads must not discard its result.
  const loadRequestRef = useRef(0)
  const graphRequestRef = useRef(0)
  const selectedCommitRef = useRef<string | null>(null)
  const columnsRef = useRef<HTMLDivElement>(null)
  const commitPane = useResizablePaneWidth(columnsRef, 'better-terminal-git-commit-width', 28, 10, 50)
  const filePane = useResizablePaneWidth(columnsRef, 'better-terminal-git-file-width', 22, 10, 40)
  const fileRows = useMemo(() => fileContent === null ? null : buildInlineFileRows(fileContent, fileDiff), [fileContent, fileDiff])
  const changeStarts = useMemo(() => fileRows ? changeBlockStarts(fileRows) : [], [fileRows])
  const [changeNav, setChangeNav] = useState<{ rows: DiffRow[]; index: number; seq: number } | null>(null)
  const activeChangeNav = changeNav && changeNav.rows === fileRows ? changeNav : null
  const changeFocus = useMemo<ChangeFocus | null>(() => {
    if (!activeChangeNav || !fileRows) return null
    const start = changeStarts[activeChangeNav.index]
    return { start, end: changeBlockEnd(fileRows, start), seq: activeChangeNav.seq }
  }, [activeChangeNav, fileRows, changeStarts])

  const stepChange = (delta: 1 | -1) => {
    const count = changeStarts.length
    if (!fileRows || count === 0) return
    const index = activeChangeNav ? (activeChangeNav.index + delta + count) % count : delta > 0 ? 0 : count - 1
    setChangeNav({ rows: fileRows, index, seq: (changeNav?.seq ?? 0) + 1 })
  }

  // Build path options: main + worktrees
  const pathOptions = [
    { path: workspaceFolderPath, label: 'Main' },
    ...worktreePaths.map(w => ({ path: w.path, label: `🌳 ${w.branch}` })),
  ]
  const hasWorktrees = worktreePaths.length > 0

  useEffect(() => {
    selectedCommitRef.current = selectedCommit
  }, [selectedCommit])

  const clearSelectionMissingFrom = useCallback((listed: readonly { hash: string }[]) => {
    const selected = selectedCommitRef.current
    if (!selected || selected === 'working' || listed.some(commit => commit.hash === selected)) return
    setSelectedCommit(null)
    setChangedFiles([])
    setSelectedFile(null)
    setDiff('')
    setFileDiff('')
  }, [])

  const applyGraph = useCallback((result: GraphLogResult | null) => {
    setGraph(result)
    if (!result) return
    graphScopeRef.current = result.scope
    clearSelectionMissingFrom(result.commits)
  }, [clearSelectionMissingFrom])

  const loadData = useCallback(async () => {
    const loadId = ++loadRequestRef.current
    const graphId = ++graphRequestRef.current
    if (graphPathRef.current !== activePath) {
      graphPathRef.current = activePath
      graphCountRef.current = GRAPH_INITIAL_COUNT
      setGraphCount(GRAPH_INITIAL_COUNT)
    }
    setLoading(true)
    setGraphBusy(false)
    setSelectedCommit(null)
    setChangedFiles([])
    setSelectedFile(null)
    setDiff('')
    setFileDiff('')
    const graphRequest = graphEnabledRef.current
      ? graphLogSource.load(activePath, graphScopeRef.current, graphCountRef.current)
      : Promise.resolve(null)
    try {
      const [logResult, statusResult, branch, root] = await Promise.all([
        host.git.getLog(activePath),
        host.git.getStatus(activePath),
        host.git.getBranch(activePath),
        host.git.getRoot(activePath),
      ])
      if (loadId !== loadRequestRef.current) return
      setIsGitRepo(branch !== null)
      setGitRoot(root)
      setCommits(logResult)
      setStatus(statusResult)
    } catch {
      if (loadId !== loadRequestRef.current) return
      setIsGitRepo(false)
    }
    const early = await settleWithin(graphRequest, GRAPH_FIRST_PAINT_GRACE_MS)
    if (loadId !== loadRequestRef.current) return
    if (graphId === graphRequestRef.current) applyGraph(early === GRAPH_PENDING ? null : early)
    setLoading(false)
    if (early !== GRAPH_PENDING) return
    const result = await graphRequest
    if (loadId === loadRequestRef.current && graphId === graphRequestRef.current) applyGraph(result)
  }, [activePath, applyGraph])

  useEffect(() => {
    loadData()
  }, [loadData])

  const reloadGraph = useCallback(async (scope: GitGraphScope, count: number, options: { persistScope: boolean; extendOnly: boolean }) => {
    const graphId = ++graphRequestRef.current
    setGraphBusy(true)
    // Loading more must neither switch scope nor mark a scope that works at the current count as failed.
    const result = await graphLogSource.load(activePath, scope, count, options.extendOnly
      ? { retryCurrent: false, rememberFailure: false }
      : undefined)
    if (graphId !== graphRequestRef.current) return
    setGraphBusy(false)
    if (!result) return
    if (options.persistScope && result.scope === scope) writeStoredGraphScope(scope)
    graphCountRef.current = count
    setGraphCount(count)
    applyGraph(result)
  }, [activePath, applyGraph])

  const handleRefresh = useCallback(() => {
    graphLogSource.forget(activePath)
    loadData()
  }, [activePath, loadData])

  const handleToggleGraph = useCallback(() => {
    const enabled = !graphEnabledRef.current
    graphEnabledRef.current = enabled
    setGraphEnabled(enabled)
    writeStoredGraphEnabled(enabled)
    if (enabled) {
      graphLogSource.forget(activePath)
      reloadGraph(graphScopeRef.current, graphCountRef.current, { persistScope: false, extendOnly: false })
      return
    }
    graphRequestRef.current++
    setGraphBusy(false)
    setGraph(null)
    clearSelectionMissingFrom(commits)
  }, [activePath, commits, reloadGraph, clearSelectionMissingFrom])

  const handleToggleScope = useCallback(() => {
    if (!graph) return
    const nextScope: GitGraphScope = graph.scope === 'all' ? 'current' : 'all'
    graphLogSource.forget(activePath, nextScope)
    reloadGraph(nextScope, graphCountRef.current, { persistScope: true, extendOnly: false })
  }, [graph, activePath, reloadGraph])

  const handleLoadMore = useCallback(() => {
    if (!graph) return
    const count = Math.min(graphCountRef.current + GRAPH_PAGE_SIZE, GRAPH_MAX_COUNT)
    reloadGraph(graph.scope, count, { persistScope: false, extendOnly: true })
  }, [graph, reloadGraph])

  const graphLayout = useMemo(() => {
    if (!graph) return null
    const headHash = findHeadHash(graph.commits, graph.scope)
    return computeGitGraph(buildGraphRows(graph.commits, { uncommitted: status.length > 0, headHash }))
  }, [graph, status.length])
  const graphRowOffset = status.length > 0 ? 1 : 0
  // Hidden while the graph is on but unavailable, so that fallback matches the original panel.
  const showGraphToggle = !graphEnabled || graph !== null || graphBusy
  const canLoadMoreCommits = graph !== null && graph.commits.length >= graphCount && graphCount < GRAPH_MAX_COUNT

  const handleSelectCommit = useCallback(async (hash: string) => {
    setSelectedCommit(hash)
    setSelectedFile(null)
    setDiff('')
    setFileDiff('')
    setFilesLoading(true)
    try {
      if (hash === 'working') {
        setChangedFiles(status)
      } else {
        const files = await host.git.getDiffFiles(activePath, hash)
        setChangedFiles(files)
      }
    } catch {
      setChangedFiles([])
    }
    setFilesLoading(false)
  }, [activePath, status])

  const handleSelectFile = useCallback(async (filePath: string) => {
    setSelectedFile(filePath)
    setViewMode('diff')
    setFileContent(null)
    setFileDiff('')
    setDiffLoading(true)
    try {
      const d = await host.git.getDiff(activePath, selectedCommit || undefined, filePath)
      if (d.trim()) {
        setDiff(d)
      } else {
        // For untracked/new files, git diff returns empty - read file content directly
        const fileEntry = changedFiles.find(f => f.file === filePath)
        if (fileEntry && (fileEntry.status === '??' || fileEntry.status === 'A')) {
          const base = gitRoot || activePath
          const sep = host.platform === 'win32' ? '\\' : '/'
          const fullPath = base + sep + filePath.replace(/[/\\]/g, sep)
          const result = await host.fs.readFile(fullPath)
          if (result.content) {
            setDiff(newFileDiff(filePath, result.content))
          } else {
            setDiff(result.error || '')
          }
        } else {
          setDiff('')
        }
      }
    } catch {
      setDiff('')
    }
    setDiffLoading(false)
  }, [activePath, selectedCommit, changedFiles, gitRoot])

  const handleViewFile = useCallback(async () => {
    if (!selectedFile) return
    setViewMode('file')
    setDiffLoading(true)
    setFileContent(null)
    setFileDiff('')
    const base = gitRoot || activePath
    const sep = host.platform === 'win32' ? '\\' : '/'
    const fullPath = base + sep + selectedFile.replace(/[/\\]/g, sep)
    try {
      const result = await host.fs.readFile(fullPath)
      if (selectedCommit === 'working' && result.content !== undefined) {
        const latestDiff = await host.git.getDiff(activePath, selectedCommit, selectedFile)
        const recheck = await host.fs.readFile(fullPath)
        const fileEntry = changedFiles.find(file => file.file === selectedFile)
        const currentDiff = latestDiff.trim() ? latestDiff : fileEntry && (fileEntry.status === '??' || fileEntry.status === 'A') && recheck.content !== undefined
          ? newFileDiff(selectedFile, recheck.content) : ''
        setDiff(currentDiff)
        setFileDiff(result.content === recheck.content ? currentDiff : '')
        setFileContent(recheck.content ?? recheck.error ?? 'Unable to read file')
      } else {
        setFileContent(result.content ?? result.error ?? 'Unable to read file')
      }
    } catch {
      setFileContent('Unable to read file')
      setDiff('')
    }
    setDiffLoading(false)
  }, [selectedFile, selectedCommit, changedFiles, activePath, gitRoot])

  if (loading) {
    return <div className="git-panel-empty">{t('common.loading')}</div>
  }

  if (!isGitRepo) {
    return (
      <div className="git-panel-empty git-panel-not-repo">
        <span>{t('git.notAGitRepo')}</span>
        <button type="button" className="git-panel-retry-btn" onClick={handleRefresh}>
          {t('git.refresh')}
        </button>
      </div>
    )
  }

  return (
    <div className="git-panel">
      {/* Worktree path selector */}
      {hasWorktrees && (
        <div className="git-path-selector">
          {pathOptions.map(opt => (
            <button
              key={opt.path}
              className={`git-path-btn${activePath === opt.path ? ' active' : ''}`}
              onClick={() => { setActivePath(opt.path); }}
              title={opt.path}
            >{opt.label}</button>
          ))}
        </div>
      )}
      <div className="git-panel-columns" ref={columnsRef}>
      {/* Column 1: Commit log */}
      <div className="git-commit-list" style={commitPane.style}>
        <div className="git-col-header">
          <span>{t('git.commits')}</span>
          {showGraphToggle ? (
            <span className="git-col-header-actions">
              {graph && (
                <button
                  type="button"
                  className="git-graph-scope-btn"
                  onClick={handleToggleScope}
                  disabled={graphBusy}
                  title={t('git.scopeToggleTitle')}
                >
                  {graph.scope === 'all' ? t('git.scopeAll') : t('git.scopeCurrent')}
                </button>
              )}
              <button
                type="button"
                className={`git-graph-toggle-btn${graphEnabled ? ' active' : ''}`}
                onClick={handleToggleGraph}
                disabled={graphBusy}
                title={graphEnabled ? t('git.hideGraph') : t('git.showGraph')}
                aria-label={graphEnabled ? t('git.hideGraph') : t('git.showGraph')}
                aria-pressed={graphEnabled}
              >
                <GitGraphToggleIcon />
              </button>
              <button className="git-refresh-btn" onClick={handleRefresh} title={t('git.refresh')}>↻</button>
            </span>
          ) : (
            <button className="git-refresh-btn" onClick={handleRefresh} title={t('git.refresh')}>↻</button>
          )}
        </div>
        <div
          className={`git-commit-list-items${graphLayout ? ' git-graph-list' : ''}`}
          style={graphLayout ? graphWidthStyle(graphLayout.laneCount) : undefined}
        >
          {status.length > 0 && (
            <div
              className={`git-commit-item${graphLayout ? ' git-commit-item-graph' : ''} ${selectedCommit === 'working' ? 'active' : ''}`}
              onClick={() => handleSelectCommit('working')}
            >
              {graphLayout && <GitGraphCell row={graphLayout.rows[0]} laneCount={graphLayout.laneCount} />}
              <div className="git-commit-message">
                {!graphLayout && <span className="git-uncommitted-badge">●</span>}
                {t('git.uncommittedChanges')}
              </div>
              <div className="git-commit-meta">
                {t('git.filesChanged', { count: status.length })}
              </div>
            </div>
          )}
          {graph && graphLayout && graph.commits.map((commit, index) => {
            const row = graphLayout.rows[index + graphRowOffset]
            return (
              <div
                key={commit.hash}
                className={`git-commit-item git-commit-item-graph ${selectedCommit === commit.hash ? 'active' : ''}`}
                onClick={() => handleSelectCommit(commit.hash)}
              >
                <GitGraphCell row={row} laneCount={graphLayout.laneCount} />
                <div className="git-commit-message">
                  <GitRefBadges refs={commit.refs} color={row.color} />
                  {commit.message}
                </div>
                <div className="git-commit-meta">
                  <span className="git-commit-hash">{commit.hash.substring(0, 7)}</span>
                  <span className="git-commit-author">{commit.author}</span>
                  <span className="git-commit-date">{formatDate(commit.date)}</span>
                </div>
              </div>
            )
          })}
          {canLoadMoreCommits && (
            <button type="button" className="git-load-more-btn" onClick={handleLoadMore} disabled={graphBusy}>
              {graphBusy ? t('common.loading') : t('git.loadMore')}
            </button>
          )}
          {!graph && commits.map(commit => (
            <div
              key={commit.hash}
              className={`git-commit-item ${selectedCommit === commit.hash ? 'active' : ''}`}
              onClick={() => handleSelectCommit(commit.hash)}
            >
              <div className="git-commit-message">{commit.message}</div>
              <div className="git-commit-meta">
                <span className="git-commit-hash">{commit.hash.substring(0, 7)}</span>
                <span className="git-commit-author">{commit.author}</span>
                <span className="git-commit-date">{formatDate(commit.date)}</span>
              </div>
            </div>
          ))}
          {commits.length === 0 && status.length === 0 && (
            <div className="git-panel-empty">{t('git.noCommitsYet')}</div>
          )}
        </div>
      </div>

      <ResizeHandle direction="horizontal" onResize={commitPane.resize} onDoubleClick={commitPane.reset} />

      {/* Column 2: Changed files */}
      <div className="git-file-list" style={filePane.style}>
        <div className="git-col-header">
          <span>{t('git.files')}</span>
          {changedFiles.length > 0 && (
            <span className="git-file-count">{changedFiles.length}</span>
          )}
        </div>
        <div className="git-file-list-items">
          {!selectedCommit && (
            <div className="git-col-placeholder">{t('git.selectACommit')}</div>
          )}
          {selectedCommit && filesLoading && (
            <div className="git-col-placeholder">{t('common.loading')}</div>
          )}
          {selectedCommit && !filesLoading && changedFiles.length === 0 && (
            <div className="git-col-placeholder">{t('git.noChangedFiles')}</div>
          )}
          {changedFiles.map(f => (
            <div
              key={f.file}
              className={`git-file-item ${selectedFile === f.file ? 'active' : ''}`}
              onClick={() => handleSelectFile(f.file)}
            >
              <span className="git-file-status" style={{ color: statusColor(f.status) }}>
                {f.status}
              </span>
              <span className="git-file-name" title={f.file}>
                {f.file.split('/').pop()}
              </span>
              <span className="git-file-path" title={f.file}>
                {f.file.includes('/') ? f.file.substring(0, f.file.lastIndexOf('/') + 1) : ''}
              </span>
            </div>
          ))}
        </div>
      </div>

      <ResizeHandle direction="horizontal" onResize={filePane.resize} onDoubleClick={filePane.reset} />

      {/* Column 3: Diff / File preview */}
      <div className="git-diff-view">
        {selectedFile && isTextFile(selectedFile.split('/').pop() || '') && (
          <div className="git-diff-mode-bar">
            <div className="git-diff-mode-toggle">
              <button
                className={`git-diff-mode-btn ${viewMode === 'diff' ? 'active' : ''}`}
                onClick={() => setViewMode('diff')}
              >
                {t('git.diff')}
              </button>
              <button
                className={`git-diff-mode-btn ${viewMode === 'file' ? 'active' : ''}`}
                onClick={handleViewFile}
              >
                {t('git.file')}
              </button>
            </div>
            {viewMode === 'file' && (
              <div className="git-change-nav">
                <span className="git-change-nav-count">
                  {activeChangeNav ? activeChangeNav.index + 1 : '–'} / {changeStarts.length}
                </span>
                <button
                  type="button"
                  className="git-change-nav-btn"
                  onClick={() => stepChange(-1)}
                  disabled={changeStarts.length === 0}
                  title={t('git.previousChange')}
                  aria-label={t('git.previousChange')}
                >
                  <ArrowIcon direction="up" />
                </button>
                <button
                  type="button"
                  className="git-change-nav-btn"
                  onClick={() => stepChange(1)}
                  disabled={changeStarts.length === 0}
                  title={t('git.nextChange')}
                  aria-label={t('git.nextChange')}
                >
                  <ArrowIcon direction="down" />
                </button>
              </div>
            )}
          </div>
        )}
        {diffLoading ? (
          <div className="git-diff-empty">{t('common.loading')}</div>
        ) : viewMode === 'file' && fileContent !== null ? (
          <FileContentView content={fileContent} rows={fileRows} focus={changeFocus} />
        ) : (
          <DiffView diff={diff} />
        )}
      </div>
      </div>
    </div>
  )
}
