import type { CSSProperties } from 'react'
import {
  GRAPH_COLOR_COUNT,
  VIRTUAL_COLOR,
  graphContinuations,
  splitGraphRefs,
  type GitGraphRef,
  type GraphLine,
  type GraphRowLayout,
} from '../lib/git-graph'

const LANE_WIDTH = 12
const NODE_Y = 50
const ROW_BOTTOM = 100
const MAX_VISIBLE_REFS = 3

export function graphWidthStyle(laneCount: number): CSSProperties {
  return { '--git-graph-width': `${Math.max(1, laneCount) * LANE_WIDTH}px` } as CSSProperties
}

function laneX(lane: number): number {
  return lane * LANE_WIDTH + LANE_WIDTH / 2
}

function laneColor(color: number): string {
  return color === VIRTUAL_COLOR ? 'var(--text-secondary)' : `var(--git-graph-color-${color % GRAPH_COLOR_COUNT})`
}

function linePath(line: GraphLine): string {
  const y1 = line.from === 'top' ? 0 : NODE_Y
  const y2 = line.to === 'node' ? NODE_Y : ROW_BOTTOM
  const x1 = laneX(line.fromLane)
  const x2 = laneX(line.toLane)
  if (x1 === x2) return `M${x1} ${y1}V${y2}`
  const yMid = (y1 + y2) / 2
  return `M${x1} ${y1}C${x1} ${yMid} ${x2} ${yMid} ${x2} ${y2}`
}

// The SVG keeps a fixed pixel width and stretches only vertically, so lanes
// line up across rows of different heights; the node is HTML to stay round.
export function GitGraphCell({ row, laneCount }: { row: GraphRowLayout; laneCount: number }) {
  const width = Math.max(1, laneCount) * LANE_WIDTH
  return (
    <span className="git-graph-cell" aria-hidden="true">
      <svg className="git-graph-svg" width={width} viewBox={`0 0 ${width} ${ROW_BOTTOM}`} preserveAspectRatio="none">
        {row.lines.map((line, index) => (
          <path
            key={index}
            d={linePath(line)}
            fill="none"
            strokeWidth={2}
            strokeDasharray={line.dashed ? '3 3' : undefined}
            vectorEffect="non-scaling-stroke"
            style={{ stroke: laneColor(line.color) }}
          />
        ))}
      </svg>
      <span
        className={`git-graph-node${row.virtual ? ' git-graph-node-uncommitted' : ''}`}
        style={{ left: laneX(row.lane) - 4, '--git-graph-node-color': laneColor(row.color) } as CSSProperties}
      />
    </span>
  )
}

export function GitGraphContinuation({ row, laneCount }: { row: GraphRowLayout; laneCount: number }) {
  const width = Math.max(1, laneCount) * LANE_WIDTH
  return (
    <span className="git-graph-cell" aria-hidden="true">
      <svg className="git-graph-svg" width={width} viewBox={`0 0 ${width} ${ROW_BOTTOM}`} preserveAspectRatio="none">
        {graphContinuations(row).map(lane => (
          <path
            key={lane.lane}
            d={`M${laneX(lane.lane)} 0V${ROW_BOTTOM}`}
            fill="none"
            strokeWidth={2}
            strokeDasharray={lane.dashed ? '3 3' : undefined}
            vectorEffect="non-scaling-stroke"
            style={{ stroke: laneColor(lane.color) }}
          />
        ))}
      </svg>
    </span>
  )
}

export function GitGraphToggleIcon() {
  return (
    <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
      <circle cx="4.5" cy="3.5" r="1.5" />
      <circle cx="4.5" cy="12.5" r="1.5" />
      <circle cx="11.5" cy="6" r="1.5" />
      <path d="M4.5 5v6M11.5 7.5c0 2.5-7 1.5-7 3.5" />
    </svg>
  )
}

function refClassName(ref: GitGraphRef): string {
  return `git-ref-badge git-ref-${ref.kind}${ref.current ? ' git-ref-current' : ''}`
}

export function GitRefBadges({ refs, color }: { refs: GitGraphRef[]; color: number }) {
  if (refs.length === 0) return null
  const { visible, hidden } = splitGraphRefs(refs, MAX_VISIBLE_REFS)
  return (
    <span className="git-ref-badges" style={{ '--git-ref-color': laneColor(color) } as CSSProperties}>
      {visible.map(ref => (
        <span key={`${ref.kind}:${ref.name}`} className={refClassName(ref)} title={ref.name}>
          {ref.name}
        </span>
      ))}
      {hidden.length > 0 && (
        <span className="git-ref-badge git-ref-more" title={hidden.map(ref => ref.name).join('\n')}>
          +{hidden.length}
        </span>
      )}
    </span>
  )
}
