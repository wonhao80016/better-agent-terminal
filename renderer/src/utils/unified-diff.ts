export interface DiffSegment {
  text: string
  changed: boolean
}

export type DiffRow =
  | { kind: 'meta' | 'hunk'; text: string }
  | {
    kind: 'context' | 'add' | 'del'
    text: string
    oldLine?: number
    newLine?: number
    segments?: DiffSegment[]
  }

type CodeRow = Extract<DiffRow, { kind: 'context' | 'add' | 'del' }>

const HUNK_HEADER = /^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@/
const FILE_HEADER = /^(diff --git |index |--- |\+\+\+ )/
const TOKEN = /\w+|\s+|[^\w\s]/gu
// Word diff is an O(n*m) LCS; skip it for pathologically long lines.
const MAX_WORD_DIFF_CELLS = 250_000
const MIN_COMMON_RATIO = 0.3

function tokenize(text: string): string[] {
  return text.match(TOKEN) ?? []
}

function mergeSegments(segments: DiffSegment[]): DiffSegment[] {
  const merged: DiffSegment[] = []
  for (const segment of segments) {
    const last = merged[merged.length - 1]
    if (last && last.changed === segment.changed) last.text += segment.text
    else merged.push({ ...segment })
  }
  return merged
}

/** Token-level diff of a removed/added line pair, or null when the lines share too little to be worth highlighting. */
export function diffWords(oldText: string, newText: string): { oldSegments: DiffSegment[]; newSegments: DiffSegment[] } | null {
  const a = tokenize(oldText)
  const b = tokenize(newText)
  if (a.length === 0 || b.length === 0 || a.length * b.length > MAX_WORD_DIFF_CELLS) return null

  const cols = b.length + 1
  const lcs = new Uint32Array((a.length + 1) * cols)
  for (let i = a.length - 1; i >= 0; i--) {
    for (let j = b.length - 1; j >= 0; j--) {
      lcs[i * cols + j] = a[i] === b[j]
        ? lcs[(i + 1) * cols + j + 1] + 1
        : Math.max(lcs[(i + 1) * cols + j], lcs[i * cols + j + 1])
    }
  }

  const oldSegments: DiffSegment[] = []
  const newSegments: DiffSegment[] = []
  let commonChars = 0
  let i = 0
  let j = 0
  while (i < a.length && j < b.length) {
    if (a[i] === b[j]) {
      if (a[i].trim()) commonChars += a[i].length
      oldSegments.push({ text: a[i++], changed: false })
      newSegments.push({ text: b[j++], changed: false })
    } else if (lcs[(i + 1) * cols + j] >= lcs[i * cols + j + 1]) {
      oldSegments.push({ text: a[i++], changed: true })
    } else {
      newSegments.push({ text: b[j++], changed: true })
    }
  }
  while (i < a.length) oldSegments.push({ text: a[i++], changed: true })
  while (j < b.length) newSegments.push({ text: b[j++], changed: true })

  const longest = Math.max(oldText.trim().length, newText.trim().length)
  if (longest === 0 || commonChars / longest < MIN_COMMON_RATIO) return null
  return { oldSegments: mergeSegments(oldSegments), newSegments: mergeSegments(newSegments) }
}

function pairChangedLines(dels: CodeRow[], adds: CodeRow[]): void {
  const pairs = Math.min(dels.length, adds.length)
  for (let k = 0; k < pairs; k++) {
    const words = diffWords(dels[k].text, adds[k].text)
    if (!words) continue
    dels[k].segments = words.oldSegments
    adds[k].segments = words.newSegments
  }
}

/** File header lines (diff --git, index, ---, +++) are dropped. */
export function parseUnifiedDiff(diff: string): DiffRow[] {
  const lines = diff.split('\n')
  if (lines[lines.length - 1] === '') lines.pop()

  const rows: DiffRow[] = []
  let inHunk = false
  let oldLine = 0
  let newLine = 0
  let dels: CodeRow[] = []
  let adds: CodeRow[] = []

  const flushChangeBlock = () => {
    pairChangedLines(dels, adds)
    dels = []
    adds = []
  }

  for (const rawLine of lines) {
    const line = rawLine.endsWith('\r') ? rawLine.slice(0, -1) : rawLine
    const hunk = HUNK_HEADER.exec(line)
    if (hunk) {
      flushChangeBlock()
      inHunk = true
      oldLine = Number(hunk[1])
      newLine = Number(hunk[3])
      rows.push({ kind: 'hunk', text: line })
      continue
    }
    if (line.startsWith('diff ')) inHunk = false
    if (!inHunk) {
      flushChangeBlock()
      if (!FILE_HEADER.test(line)) rows.push({ kind: 'meta', text: line })
      continue
    }

    const marker = line[0]
    const text = line.slice(1)
    if (marker === '-') {
      if (adds.length > 0) flushChangeBlock()
      const row: CodeRow = { kind: 'del', text, oldLine: oldLine++ }
      dels.push(row)
      rows.push(row)
    } else if (marker === '+') {
      const row: CodeRow = { kind: 'add', text, newLine: newLine++ }
      adds.push(row)
      rows.push(row)
    } else if (marker === '\\') {
      rows.push({ kind: 'meta', text: line })
    } else {
      flushChangeBlock()
      rows.push({ kind: 'context', text, oldLine: oldLine++, newLine: newLine++ })
    }
  }
  flushChangeBlock()
  return rows
}

/** Lines without terminators; a single trailing newline does not start an extra empty line. */
export function splitContentLines(content: string): string[] {
  if (content === '') return []
  return content.replace(/\r?\n$/, '').split('\n').map(line => line.replace(/\r$/, ''))
}

function isCodeRow(row: DiffRow): row is CodeRow {
  return row.kind === 'context' || row.kind === 'add' || row.kind === 'del'
}

function isChangeRow(row: DiffRow): boolean {
  return row.kind === 'add' || row.kind === 'del'
}

/** First line a hunk side covers; for an empty side the header number is the line before it. */
function hunkStart(start: string, count: string | undefined): number {
  return Number(start) + (count === '0' ? 1 : 0)
}

/**
 * Every line of `content` plus the diff's deleted lines at their old positions, without hunk/meta rows.
 * Returns null when the diff does not describe `content` (stale content, another file, overlapping hunks).
 */
export function buildInlineFileRows(content: string, diff: string): DiffRow[] | null {
  const lines = splitContentLines(content)
  const rows: DiffRow[] = []
  let oldNext = 1
  let newNext = 1

  const pushUnchanged = (untilNewLine: number) => {
    while (newNext < untilNewLine) rows.push({ kind: 'context', text: lines[newNext - 1], oldLine: oldNext++, newLine: newNext++ })
  }

  for (const row of parseUnifiedDiff(diff)) {
    if (row.kind === 'hunk') {
      const header = HUNK_HEADER.exec(row.text)
      if (!header) return null
      const oldStart = hunkStart(header[1], header[2])
      const newStart = hunkStart(header[3], header[4])
      if (newStart < newNext || newStart - newNext !== oldStart - oldNext || newStart - 1 > lines.length) return null
      pushUnchanged(newStart)
      continue
    }
    if (!isCodeRow(row)) continue
    if (row.kind !== 'del' && (row.newLine !== newNext || lines[newNext - 1] !== row.text)) return null
    if (row.kind !== 'add' && row.oldLine !== oldNext) return null
    if (row.kind !== 'del') newNext++
    if (row.kind !== 'add') oldNext++
    rows.push(row)
  }
  pushUnchanged(lines.length + 1)
  return rows
}

/** Index of the first row of each maximal run of add/del rows. */
export function changeBlockStarts(rows: DiffRow[]): number[] {
  const starts: number[] = []
  rows.forEach((row, i) => {
    if (isChangeRow(row) && (i === 0 || !isChangeRow(rows[i - 1]))) starts.push(i)
  })
  return starts
}

/** Index just past the run of add/del rows that begins at `start`. */
export function changeBlockEnd(rows: DiffRow[], start: number): number {
  let end = start
  while (end < rows.length && isChangeRow(rows[end])) end++
  return end
}
