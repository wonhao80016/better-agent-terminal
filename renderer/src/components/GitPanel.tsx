import { host } from '../host-api'
import { Fragment, useState, useEffect, useCallback, useMemo, useRef, type CSSProperties } from 'react'
import { useTranslation } from 'react-i18next'
import i18next from 'i18next'
import { ResizeHandle } from './ResizeHandle'
import { useResizablePaneWidth } from '../hooks/useResizablePaneWidth'
import { getFileChangeMarkers, parseUnifiedDiff, type DiffRow } from '../utils/unified-diff'

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

function DiffView({ diff }: { diff: string }) {
  const { t } = useTranslation()
  const rows = useMemo(() => parseUnifiedDiff(diff), [diff])
  if (!diff) {
    return <div className="git-diff-empty">{t('git.selectFileToViewDiff')}</div>
  }

  const maxLine = rows.reduce((max, row) => 'oldLine' in row || 'newLine' in row
    ? Math.max(max, row.oldLine ?? 0, row.newLine ?? 0)
    : max, 0)

  return (
    <div className="git-diff-content" style={lineNumberGutter(maxLine)}>
      <div className="git-diff-rows">
        {rows.map((row, i) => (
          <div key={i} className={`git-diff-row git-diff-row-${row.kind}`}>
            <span className="git-diff-ln">{'oldLine' in row ? row.oldLine : ''}</span>
            <span className="git-diff-ln">{'newLine' in row ? row.newLine : ''}</span>
            <span className="git-diff-sign">{DIFF_SIGN[row.kind]}</span>
            <span className="git-diff-code"><DiffCode row={row} /></span>
          </div>
        ))}
      </div>
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

function FileContentView({ content, diff }: { content: string; diff: string }) {
  const lines = useMemo(() => content.replace(/\r?\n$/, '').split('\n').map(line => line.replace(/\r$/, '')), [content])
  const markers = useMemo(() => getFileChangeMarkers(content, diff), [content, diff])
  return (
    <div className="git-file-content" style={lineNumberGutter(lines.length)}>
      <div className="git-diff-rows">
        {lines.map((line, i) => (
          <div key={i} className={`git-diff-row${markers.has(i + 1) ? ` git-file-row-${markers.get(i + 1)}` : ''}`}>
            <span className="git-diff-ln">{i + 1}</span>
            <span className="git-diff-code">{line || ' '}</span>
          </div>
        ))}
      </div>
    </div>
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
  const columnsRef = useRef<HTMLDivElement>(null)
  const commitPane = useResizablePaneWidth(columnsRef, 'better-terminal-git-commit-width', 28, 10, 50)
  const filePane = useResizablePaneWidth(columnsRef, 'better-terminal-git-file-width', 22, 10, 40)

  // Build path options: main + worktrees
  const pathOptions = [
    { path: workspaceFolderPath, label: 'Main' },
    ...worktreePaths.map(w => ({ path: w.path, label: `🌳 ${w.branch}` })),
  ]
  const hasWorktrees = worktreePaths.length > 0

  const loadData = useCallback(async () => {
    setLoading(true)
    setSelectedCommit(null)
    setChangedFiles([])
    setSelectedFile(null)
    setDiff('')
    setFileDiff('')
    try {
      const [logResult, statusResult, branch, root] = await Promise.all([
        host.git.getLog(activePath),
        host.git.getStatus(activePath),
        host.git.getBranch(activePath),
        host.git.getRoot(activePath),
      ])
      setIsGitRepo(branch !== null)
      setGitRoot(root)
      setCommits(logResult)
      setStatus(statusResult)
    } catch {
      setIsGitRepo(false)
    }
    setLoading(false)
  }, [activePath])

  useEffect(() => {
    loadData()
  }, [loadData])

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
    return <div className="git-panel-empty">{t('git.notAGitRepo')}</div>
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
          <button className="git-refresh-btn" onClick={loadData} title={t('git.refresh')}>↻</button>
        </div>
        <div className="git-commit-list-items">
          {status.length > 0 && (
            <div
              className={`git-commit-item ${selectedCommit === 'working' ? 'active' : ''}`}
              onClick={() => handleSelectCommit('working')}
            >
              <div className="git-commit-message">
                <span className="git-uncommitted-badge">●</span>
                {t('git.uncommittedChanges')}
              </div>
              <div className="git-commit-meta">
                {t('git.filesChanged', { count: status.length })}
              </div>
            </div>
          )}
          {commits.map(commit => (
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
        )}
        {diffLoading ? (
          <div className="git-diff-empty">{t('common.loading')}</div>
        ) : viewMode === 'file' && fileContent !== null ? (
          <FileContentView content={fileContent} diff={fileDiff} />
        ) : (
          <DiffView diff={diff} />
        )}
      </div>
      </div>
    </div>
  )
}
