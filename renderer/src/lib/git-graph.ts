export type GitGraphScope = 'current' | 'all'

export interface GitGraphRef {
  kind: string
  name: string
  current: boolean
}

export interface GitGraphCommit {
  hash: string
  parents: string[]
  refs: GitGraphRef[]
  author: string
  date: string
  message: string
}

export interface GraphInputRow {
  hash: string
  parents: readonly string[]
  virtual?: boolean
}

export interface GraphLine {
  fromLane: number
  toLane: number
  from: 'top' | 'node'
  to: 'node' | 'bottom'
  color: number
  dashed: boolean
}

export interface GraphRowLayout {
  lane: number
  color: number
  virtual: boolean
  lines: GraphLine[]
}

export interface GraphLayout {
  rows: GraphRowLayout[]
  laneCount: number
}

export const GRAPH_COLOR_COUNT = 8
export const VIRTUAL_COLOR = -1
export const UNCOMMITTED_ROW_HASH = 'working'

interface Track {
  hash: string
  color: number
  virtual: boolean
}

/**
 * Lays out commit lanes for rows in display order, where every child precedes
 * its parents (`git log --date-order`). See docs/git-graph/plan.md, section 5.4.
 */
export function computeGitGraph(rows: readonly GraphInputRow[]): GraphLayout {
  let lanes: Track[] = []
  let colorCounter = 0
  let laneCount = 0
  const nextColor = () => colorCounter++ % GRAPH_COLOR_COUNT
  const layout: GraphRowLayout[] = []

  for (const row of rows) {
    const virtual = row.virtual === true
    const matches: number[] = []
    lanes.forEach((track, index) => {
      if (track.hash === row.hash) matches.push(index)
    })
    const realMatch = matches.find(index => !lanes[index].virtual)
    const lane = realMatch ?? matches[0] ?? lanes.length
    const color = virtual ? VIRTUAL_COLOR : realMatch !== undefined ? lanes[realMatch].color : nextColor()

    const lines: GraphLine[] = []
    for (const index of matches) {
      const track = lanes[index]
      lines.push({ fromLane: index, toLane: lane, from: 'top', to: 'node', color: track.color, dashed: track.virtual })
    }

    const matched = new Set(matches)
    const slots: (Track | null)[] = lanes.map((track, index) => (matched.has(index) ? null : track))
    if (lane === lanes.length) slots.push(null)

    const outgoing: { slot: number; color: number; dashed: boolean }[] = []
    const [firstParent, ...otherParents] = row.parents
    if (firstParent !== undefined) {
      slots[lane] = { hash: firstParent, color, virtual }
      outgoing.push({ slot: lane, color, dashed: virtual })
    }
    for (const parent of otherParents) {
      let slot = slots.findIndex(track => track !== null && track.hash === parent)
      if (slot < 0) {
        slot = slots.indexOf(null)
        if (slot < 0) slot = slots.push(null) - 1
        slots[slot] = { hash: parent, color: nextColor(), virtual: false }
      }
      outgoing.push({ slot, color: slots[slot]!.color, dashed: false })
    }

    const compactIndex: number[] = []
    let kept = 0
    slots.forEach((track, index) => {
      compactIndex[index] = kept
      if (track !== null) kept++
    })

    lanes.forEach((track, index) => {
      if (matched.has(index)) return
      lines.push({ fromLane: index, toLane: compactIndex[index], from: 'top', to: 'bottom', color: track.color, dashed: track.virtual })
    })
    for (const edge of outgoing) {
      lines.push({ fromLane: lane, toLane: compactIndex[edge.slot], from: 'node', to: 'bottom', color: edge.color, dashed: edge.dashed })
    }

    laneCount = Math.max(laneCount, lane + 1)
    for (const line of lines) laneCount = Math.max(laneCount, line.fromLane + 1, line.toLane + 1)
    lanes = slots.filter((track): track is Track => track !== null)
    layout.push({ lane, color, virtual, lines })
  }

  return { rows: layout, laneCount }
}

/** HEAD from the decorations; with scope "current" the log starts at HEAD. */
export function findHeadHash(commits: readonly GitGraphCommit[], scope: GitGraphScope): string | null {
  const decorated = commits.find(commit => commit.refs.some(ref => ref.kind === 'head' || ref.current))
  if (decorated) return decorated.hash
  return scope === 'current' && commits.length > 0 ? commits[0].hash : null
}

export function buildGraphRows(
  commits: readonly GitGraphCommit[],
  options: { uncommitted: boolean; headHash: string | null },
): GraphInputRow[] {
  const rows: GraphInputRow[] = commits.map(commit => ({ hash: commit.hash, parents: commit.parents }))
  if (options.uncommitted) {
    rows.unshift({
      hash: UNCOMMITTED_ROW_HASH,
      parents: options.headHash ? [options.headHash] : [],
      virtual: true,
    })
  }
  return rows
}

const REF_KIND_ORDER = ['head', 'current', 'branch', 'remote', 'tag'] as const

function refRank(ref: GitGraphRef): number {
  const kind = ref.current ? 'current' : ref.kind
  const rank = REF_KIND_ORDER.indexOf(kind as (typeof REF_KIND_ORDER)[number])
  return rank < 0 ? REF_KIND_ORDER.length : rank
}

export function sortGraphRefs(refs: readonly GitGraphRef[]): GitGraphRef[] {
  return refs
    .map((ref, index) => ({ ref, index }))
    .sort((a, b) => refRank(a.ref) - refRank(b.ref) || a.index - b.index)
    .map(entry => entry.ref)
}

export function splitGraphRefs(refs: readonly GitGraphRef[], maxVisible: number): { visible: GitGraphRef[]; hidden: GitGraphRef[] } {
  const sorted = sortGraphRefs(refs)
  return { visible: sorted.slice(0, maxVisible), hidden: sorted.slice(maxVisible) }
}
