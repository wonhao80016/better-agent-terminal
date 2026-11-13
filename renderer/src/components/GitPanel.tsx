import { host } from '../host-api'
import { Fragment, useState, useEffect, useCallback, useMemo, useReducer, useRef, type CSSProperties, type KeyboardEvent } from 'react'
import { useTranslation } from 'react-i18next'
import i18next from 'i18next'
import { ResizeHandle } from './ResizeHandle'
import { GitGraphCell, GitGraphContinuation, GitGraphToggleIcon, GitRefBadges, graphWidthStyle } from './GitGraph'
import { useResizablePaneWidth } from '../hooks/useResizablePaneWidth'
import { buildInlineFileRows, changeBlockEnd, changeBlockStarts, parseUnifiedDiff, splitContentLines, type DiffRow } from '../utils/unified-diff'
import { buildGraphRows, computeGitGraph, findHeadHash, parseGraphEnabled, parseGraphScope, UNCOMMITTED_ROW_HASH, type GitGraphScope, type GraphRowLayout } from '../lib/git-graph'
import {
  CommitDetailsCache,
  committerDiffersFromAuthor,
  formatCommitDateTime,
  nextExpandedCommit,
  parentLinks,
  type CommitDetailsEntry,
} from '../lib/git-commit-details'
import { createGraphLogSource, type GraphLogResult } from '../lib/git-graph-source'
import { formatErrorMessage } from '../utils/error-message'

interface GitCommit {
  hash: string
  author: string
  date: string
  message: string
}

interface GitStatusEntry {
  status: string
  file: string
  oldFile?: string
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
  'md', 'txt', 'text', 'yml', 'yaml', 'toml', 'xml', 'svg', 'sh', 'bash', 'zsh',
  'py', 'rb', 'go', 'rs', 'java', 'c', 'cpp', 'h', 'hpp', 'cs',
  'cc', 'cxx', 'hxx', 'inf', 'rc', 'def', 'sln', 'vcxproj', 'filters', 'props', 'targets',
  'ps1', 'bat', 'cmd', 'mjs',
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
  switch (/^[RC]/.test(s) ? 'R' : s) {
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

const UNREADABLE_FILE = 'Unable to read file'

// A rename/copy pair is diffed blob to blob, which carries no rename lines; restore them.
function pairDiffHeader(entry: GitStatusEntry): string {
  if (!entry.oldFile) return ''
  const kind = entry.status.startsWith('C') ? 'copy' : 'rename'
  const score = /^[RC](\d+)$/.exec(entry.status)?.[1]
  return `${score ? `similarity index ${Number(score)}%\n` : ''}${kind} from ${entry.oldFile}\n${kind} to ${entry.file}\n`
}

// Only a commit's blob-to-blob pair lacks rename lines. A working-tree pair (-M) has its own,
// and a host that predates oldFilePath returns the new path as a whole new file.
async function fetchFileDiff(cwd: string, commit: string | undefined, filePath: string, entry?: GitStatusEntry): Promise<string> {
  const diff = await host.git.getDiff(cwd, commit, filePath, entry?.oldFile)
  const isPairDiff = !!entry?.oldFile && !/^((new|deleted) file mode |(rename|copy) from )/m.test(diff)
  return isPairDiff ? pairDiffHeader(entry) + diff : diff
}

// Error text from a host without git_show_file: the sidecar's JSON-RPC -32601 for a remote
// host built before git:show-file, or Tauri's unknown-command rejection.
function isMissingShowFileCommand(error: unknown): boolean {
  return /method not found: git\.showFile|command git_show_file not found/i.test(formatErrorMessage(error, ''))
}

// undefined when the host has no show-file command; null when the file cannot be shown.
function showFileAt(cwd: string, rev: string, filePath: string): Promise<string | null | undefined> {
  return host.git.showFile(cwd, rev, filePath).catch((error: unknown) => isMissingShowFileCommand(error) ? undefined : null)
}

interface CommitFileView {
  content: string
  fileDiff: string
  diff: string
}

// The content must be the commit's own version so it lines up with the commit's diff. A deleted
// file is shown as its deletion diff over empty content, but only if its last version passes the
// same size and text checks as any other shown file.
async function loadCommitFileView(
  cwd: string,
  hash: string,
  entry: GitStatusEntry,
  readWorkingTreeFile: () => Promise<string>,
): Promise<CommitFileView> {
  const deleted = entry.status === 'D'
  const [diff, content] = await Promise.all([
    fetchFileDiff(cwd, hash, entry.file, entry),
    showFileAt(cwd, deleted ? `${hash}~1` : hash, entry.file),
  ])
  if (content === undefined) return { content: await readWorkingTreeFile(), fileDiff: '', diff }
  if (content === null) return { content: UNREADABLE_FILE, fileDiff: '', diff }
  if (!deleted) return { content, fileDiff: diff, diff }
  if (buildInlineFileRows('', diff)?.length) return { content: '', fileDiff: diff, diff }
  return { content, fileDiff: '', diff }
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

function CommitDetailsFields({ entry, loadedHashes, onJump }: {
  entry: CommitDetailsEntry | undefined
  loadedHashes: ReadonlySet<string>
  onJump: (hash: string) => void
}) {
  const { t } = useTranslation()
  if (entry === undefined || entry === 'loading') {
    return <div className="git-commit-details-status">{t('common.loading')}</div>
  }
  if (entry === null) {
    return <div className="git-commit-details-status">{t('git.detailsUnavailable')}</div>
  }
  const author = `${entry.authorName} <${entry.authorEmail}>`
  const committer = `${entry.committerName} <${entry.committerEmail}>`
  return (
    <>
      <dl className="git-commit-details-fields">
        <dt>{t('git.detailsCommit')}</dt>
        <dd className="git-commit-details-hash" title={entry.hash}>{entry.hash}</dd>
        {entry.parents.length > 0 && (
          <>
            <dt>{t('git.detailsParents')}</dt>
            <dd>
              {parentLinks(entry.parents, loadedHashes).map(parent => parent.loaded ? (
                <button
                  key={parent.hash}
                  type="button"
                  className="git-commit-parent-link"
                  title={parent.hash}
                  onClick={() => onJump(parent.hash)}
                >
                  {parent.shortHash}
                </button>
              ) : (
                <span key={parent.hash} className="git-commit-parent-hash" title={t('git.parentNotLoaded', { hash: parent.hash })}>
                  {parent.shortHash}
                </span>
              ))}
            </dd>
          </>
        )}
        <dt>{t('git.detailsAuthor')}</dt>
        <dd title={author}>{author}</dd>
        <dt>{t('git.detailsDate')}</dt>
        <dd>{formatCommitDateTime(entry.authorDate)}</dd>
        {committerDiffersFromAuthor(entry) && (
          <>
            <dt>{t('git.detailsCommitter')}</dt>
            <dd title={committer}>{committer}</dd>
            <dt>{t('git.detailsDate')}</dt>
            <dd>{formatCommitDateTime(entry.committerDate)}</dd>
          </>
        )}
      </dl>
      <div className="git-commit-details-message">{entry.message}</div>
    </>
  )
}

function ChangedFileList({ files, loading, selectedFile, onSelect }: {
  files: readonly GitStatusEntry[]
  loading: boolean
  selectedFile: string | null
  onSelect: (file: string) => void
}) {
  const { t } = useTranslation()
  return (
    <div className="git-commit-details-files">
      <div className="git-commit-details-files-header">
        <span>{t('git.files')}</span>
        {files.length > 0 && <span className="git-file-count">{files.length}</span>}
      </div>
      {loading && <div className="git-col-placeholder">{t('common.loading')}</div>}
      {!loading && files.length === 0 && <div className="git-col-placeholder">{t('git.noChangedFiles')}</div>}
      {files.map(f => (
        <div
          key={f.file}
          className={`git-file-item ${selectedFile === f.file ? 'active' : ''}`}
          onClick={() => onSelect(f.file)}
        >
          <span className="git-file-status" style={{ color: statusColor(f.status) }}>
            {f.status}
          </span>
          <span className="git-file-name" title={f.file}>
            {f.file.split('/').pop()}
          </span>
          {f.oldFile ? (
            <span className="git-file-path" title={`${f.oldFile} → ${f.file}`}>
              {`${f.oldFile} → ${f.file}`}
            </span>
          ) : (
            <span className="git-file-path" title={f.file}>
              {f.file.includes('/') ? f.file.substring(0, f.file.lastIndexOf('/') + 1) : ''}
            </span>
          )}
        </div>
      ))}
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
  // Bumped per selection so a slower, superseded file-list, diff or file load is dropped.
  const commitRequestRef = useRef(0)
  const fileRequestRef = useRef(0)
  const columnsRef = useRef<HTMLDivElement>(null)
  const commitListRef = useRef<HTMLDivElement>(null)
  const commitPane = useResizablePaneWidth(columnsRef, 'better-terminal-git-commit-width', 40, 15, 70)
  const activePathRef = useRef(activePath)
  const [, refreshCommitDetails] = useReducer((tick: number) => tick + 1, 0)
  const [commitDetails] = useState(() => new CommitDetailsCache(
    hash => host.git.getCommitDetails(activePathRef.current, hash),
    refreshCommitDetails,
  ))
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

  useEffect(() => {
    activePathRef.current = activePath
    commitDetails.reset()
  }, [activePath, commitDetails])

  const collapseCommit = useCallback(() => {
    commitRequestRef.current++
    fileRequestRef.current++
    setSelectedCommit(null)
    setChangedFiles([])
    setFilesLoading(false)
    setDiffLoading(false)
    setSelectedFile(null)
    setFileContent(null)
    setViewMode('diff')
    setDiff('')
    setFileDiff('')
  }, [])

  const clearSelectionMissingFrom = useCallback((listed: readonly { hash: string }[]) => {
    const selected = selectedCommitRef.current
    if (!selected || selected === 'working' || listed.some(commit => commit.hash === selected)) return
    collapseCommit()
  }, [collapseCommit])

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
    collapseCommit()
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
  }, [activePath, applyGraph, collapseCommit])

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
    const request = ++commitRequestRef.current
    fileRequestRef.current++
    setSelectedCommit(hash)
    setSelectedFile(null)
    setChangedFiles([])
    setFileContent(null)
    setViewMode('diff')
    setDiff('')
    setFileDiff('')
    setDiffLoading(false)
    setFilesLoading(true)
    if (hash !== UNCOMMITTED_ROW_HASH) void commitDetails.request(hash)
    try {
      if (hash === UNCOMMITTED_ROW_HASH) {
        setChangedFiles(status)
      } else {
        const files = await host.git.getDiffFiles(activePath, hash)
        if (request !== commitRequestRef.current) return
        setChangedFiles(files)
      }
    } catch {
      if (request !== commitRequestRef.current) return
      setChangedFiles([])
    }
    setFilesLoading(false)
  }, [activePath, status, commitDetails])

  const handleToggleCommit = useCallback((hash: string) => {
    if (nextExpandedCommit(selectedCommit, hash) === null) collapseCommit()
    else void handleSelectCommit(hash)
  }, [selectedCommit, collapseCommit, handleSelectCommit])

  const loadedCommitHashes = useMemo(
    () => new Set((graph ? graph.commits : commits).map(commit => commit.hash)),
    [graph, commits],
  )

  const handleJumpToCommit = useCallback((hash: string) => {
    if (selectedCommit !== hash) void handleSelectCommit(hash)
    requestAnimationFrame(() => {
      const row = commitListRef.current?.querySelector<HTMLElement>(`[data-commit-hash="${hash}"]`)
      row?.scrollIntoView({ block: 'nearest' })
      row?.focus({ preventScroll: true })
    })
  }, [selectedCommit, handleSelectCommit])

  const commitRowProps = (hash: string) => ({
    role: 'button',
    tabIndex: 0,
    'aria-expanded': selectedCommit === hash,
    'data-commit-hash': hash,
    onClick: () => handleToggleCommit(hash),
    onKeyDown: (event: KeyboardEvent<HTMLDivElement>) => {
      if (event.target !== event.currentTarget || (event.key !== 'Enter' && event.key !== ' ')) return
      event.preventDefault()
      handleToggleCommit(hash)
    },
  })

  const renderCommitExpansion = (hash: string, row: GraphRowLayout | undefined) => (
    <div className={`git-commit-details${row ? ' git-commit-details-graph' : ''}`}>
      {row && graphLayout && <GitGraphContinuation row={row} laneCount={graphLayout.laneCount} />}
      {hash !== UNCOMMITTED_ROW_HASH && (
        <CommitDetailsFields entry={commitDetails.get(hash)} loadedHashes={loadedCommitHashes} onJump={handleJumpToCommit} />
      )}
      <ChangedFileList files={changedFiles} loading={filesLoading} selectedFile={selectedFile} onSelect={handleSelectFile} />
    </div>
  )

  const handleSelectFile = useCallback(async (filePath: string) => {
    const request = ++fileRequestRef.current
    setSelectedFile(filePath)
    setViewMode('diff')
    setDiff('')
    setFileContent(null)
    setFileDiff('')
    setDiffLoading(true)
    const fileEntry = changedFiles.find(f => f.file === filePath)
    try {
      const d = await fetchFileDiff(activePath, selectedCommit || undefined, filePath, fileEntry)
      if (request !== fileRequestRef.current) return
      if (d.trim()) {
        setDiff(d)
      } else {
        // For untracked/new files, git diff returns empty - read file content directly
        if (fileEntry && (fileEntry.status === '??' || fileEntry.status === 'A')) {
          const base = gitRoot || activePath
          const sep = host.platform === 'win32' ? '\\' : '/'
          const fullPath = base + sep + filePath.replace(/[/\\]/g, sep)
          const result = await host.fs.readFile(fullPath)
          if (request !== fileRequestRef.current) return
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
      if (request !== fileRequestRef.current) return
      setDiff('')
    }
    setDiffLoading(false)
  }, [activePath, selectedCommit, changedFiles, gitRoot])

  const handleViewFile = useCallback(async () => {
    if (!selectedFile) return
    const request = ++fileRequestRef.current
    setViewMode('file')
    setDiffLoading(true)
    setFileContent(null)
    setFileDiff('')
    const base = gitRoot || activePath
    const sep = host.platform === 'win32' ? '\\' : '/'
    const fullPath = base + sep + selectedFile.replace(/[/\\]/g, sep)
    const fileEntry = changedFiles.find(file => file.file === selectedFile)
    try {
      if (selectedCommit && selectedCommit !== 'working') {
        const readWorkingTreeFile = async () => {
          const result = await host.fs.readFile(fullPath)
          return result.content ?? result.error ?? UNREADABLE_FILE
        }
        const view = await loadCommitFileView(activePath, selectedCommit, fileEntry ?? { status: '', file: selectedFile }, readWorkingTreeFile)
        if (request !== fileRequestRef.current) return
        setDiff(view.diff)
        setFileDiff(view.fileDiff)
        setFileContent(view.content)
      } else {
        const result = await host.fs.readFile(fullPath)
        if (selectedCommit === 'working' && result.content === undefined) {
          const latestDiff = await fetchFileDiff(activePath, selectedCommit, selectedFile, fileEntry)
          if (request !== fileRequestRef.current) return
          setDiff(latestDiff)
          setFileContent(result.error ?? UNREADABLE_FILE)
        } else if (selectedCommit === 'working' && result.content !== undefined) {
          const latestDiff = await fetchFileDiff(activePath, selectedCommit, selectedFile, fileEntry)
          const recheck = await host.fs.readFile(fullPath)
          if (request !== fileRequestRef.current) return
          const currentDiff = latestDiff.trim() ? latestDiff : fileEntry && (fileEntry.status === '??' || fileEntry.status === 'A') && recheck.content !== undefined
            ? newFileDiff(selectedFile, recheck.content) : ''
          setDiff(currentDiff)
          setFileDiff(result.content === recheck.content ? currentDiff : '')
          setFileContent(recheck.content ?? recheck.error ?? UNREADABLE_FILE)
        } else {
          if (request !== fileRequestRef.current) return
          setFileContent(result.content ?? result.error ?? UNREADABLE_FILE)
        }
      }
    } catch {
      if (request !== fileRequestRef.current) return
      setFileContent(UNREADABLE_FILE)
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
          ref={commitListRef}
          className={`git-commit-list-items${graphLayout ? ' git-graph-list' : ''}`}
          style={graphLayout ? graphWidthStyle(graphLayout.laneCount) : undefined}
        >
          {status.length > 0 && (
            <>
              <div
                className={`git-commit-item${graphLayout ? ' git-commit-item-graph' : ''} ${selectedCommit === UNCOMMITTED_ROW_HASH ? 'active' : ''}`}
                {...commitRowProps(UNCOMMITTED_ROW_HASH)}
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
              {selectedCommit === UNCOMMITTED_ROW_HASH && renderCommitExpansion(UNCOMMITTED_ROW_HASH, graphLayout?.rows[0])}
            </>
          )}
          {graph && graphLayout && graph.commits.map((commit, index) => {
            const row = graphLayout.rows[index + graphRowOffset]
            return (
              <Fragment key={commit.hash}>
                <div
                  className={`git-commit-item git-commit-item-graph ${selectedCommit === commit.hash ? 'active' : ''}`}
                  {...commitRowProps(commit.hash)}
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
                {selectedCommit === commit.hash && renderCommitExpansion(commit.hash, row)}
              </Fragment>
            )
          })}
          {canLoadMoreCommits && (
            <button type="button" className="git-load-more-btn" onClick={handleLoadMore} disabled={graphBusy}>
              {graphBusy ? t('common.loading') : t('git.loadMore')}
            </button>
          )}
          {!graph && commits.map(commit => (
            <Fragment key={commit.hash}>
              <div
                className={`git-commit-item ${selectedCommit === commit.hash ? 'active' : ''}`}
                {...commitRowProps(commit.hash)}
              >
                <div className="git-commit-message">{commit.message}</div>
                <div className="git-commit-meta">
                  <span className="git-commit-hash">{commit.hash.substring(0, 7)}</span>
                  <span className="git-commit-author">{commit.author}</span>
                  <span className="git-commit-date">{formatDate(commit.date)}</span>
                </div>
              </div>
              {selectedCommit === commit.hash && renderCommitExpansion(commit.hash, undefined)}
            </Fragment>
          ))}
          {commits.length === 0 && status.length === 0 && (
            <div className="git-panel-empty">{t('git.noCommitsYet')}</div>
          )}
        </div>
      </div>

      <ResizeHandle direction="horizontal" onResize={commitPane.resize} onDoubleClick={commitPane.reset} />

      {/* Column 2: Diff / File preview */}
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
