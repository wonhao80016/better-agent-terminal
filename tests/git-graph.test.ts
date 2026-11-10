import * as assert from 'node:assert/strict'
import {
  buildGraphRows,
  computeGitGraph,
  findHeadHash,
  sortGraphRefs,
  splitGraphRefs,
  UNCOMMITTED_ROW_HASH,
  VIRTUAL_COLOR,
  type GitGraphCommit,
  type GitGraphRef,
  type GraphInputRow,
  type GraphLine,
} from '../renderer/src/lib/git-graph'
import { createGraphLogSource } from '../renderer/src/lib/git-graph-source'

function row(hash: string, ...parents: string[]): GraphInputRow {
  return { hash, parents }
}

function commit(hash: string, parents: string[] = [], refs: GitGraphRef[] = []): GitGraphCommit {
  return { hash, parents, refs, author: 'a', date: '2026-11-10T00:00:00Z', message: hash }
}

function ref(kind: string, name: string, current = false): GitGraphRef {
  return { kind, name, current }
}

function line(fromLane: number, toLane: number, from: GraphLine['from'], to: GraphLine['to'], color: number, dashed = false): GraphLine {
  return { fromLane, toLane, from, to, color, dashed }
}

function sortedLines(lines: GraphLine[]): GraphLine[] {
  return [...lines].sort((a, b) =>
    a.from.localeCompare(b.from) || a.to.localeCompare(b.to) || a.fromLane - b.fromLane || a.toLane - b.toLane)
}

function assertLines(actual: GraphLine[], expected: GraphLine[], label: string) {
  assert.deepEqual(sortedLines(actual), sortedLines(expected), label)
}

// Linear history keeps a single lane and ends at the root.
{
  const layout = computeGitGraph([row('c', 'b'), row('b', 'a'), row('a')])
  assert.equal(layout.laneCount, 1)
  assert.deepEqual(layout.rows.map(r => [r.lane, r.color]), [[0, 0], [0, 0], [0, 0]])
  assertLines(layout.rows[0].lines, [line(0, 0, 'node', 'bottom', 0)], 'linear head')
  assertLines(layout.rows[1].lines, [line(0, 0, 'top', 'node', 0), line(0, 0, 'node', 'bottom', 0)], 'linear middle')
  assertLines(layout.rows[2].lines, [line(0, 0, 'top', 'node', 0)], 'linear root')
}

// A merge opens a lane for the second parent; both branches run in parallel
// and converge on their shared parent.
{
  const layout = computeGitGraph([row('m', 'b', 'f'), row('f', 'a'), row('b', 'a'), row('a')])
  assert.equal(layout.laneCount, 2)
  assert.deepEqual(layout.rows.map(r => [r.lane, r.color]), [[0, 0], [1, 1], [0, 0], [0, 0]])
  assertLines(layout.rows[0].lines, [line(0, 0, 'node', 'bottom', 0), line(0, 1, 'node', 'bottom', 1)], 'merge')
  assertLines(layout.rows[1].lines, [
    line(1, 1, 'top', 'node', 1),
    line(0, 0, 'top', 'bottom', 0),
    line(1, 1, 'node', 'bottom', 1),
  ], 'feature commit')
  assertLines(layout.rows[2].lines, [
    line(0, 0, 'top', 'node', 0),
    line(1, 1, 'top', 'bottom', 1),
    line(0, 0, 'node', 'bottom', 0),
  ], 'main commit keeps a parallel lane')
  assertLines(layout.rows[3].lines, [line(0, 0, 'top', 'node', 0), line(1, 0, 'top', 'node', 1)], 'convergence')
}

// An octopus merge opens one lane per extra parent.
{
  const layout = computeGitGraph([row('o', 'p1', 'p2', 'p3'), row('p3'), row('p2'), row('p1')])
  assert.equal(layout.laneCount, 3)
  assertLines(layout.rows[0].lines, [
    line(0, 0, 'node', 'bottom', 0),
    line(0, 1, 'node', 'bottom', 1),
    line(0, 2, 'node', 'bottom', 2),
  ], 'octopus fan-out')
  assert.deepEqual(layout.rows.slice(1).map(r => [r.lane, r.color]), [[2, 2], [1, 1], [0, 0]])
}

// A merge parent that a lane already expects joins that lane instead of opening one.
{
  const layout = computeGitGraph([row('x', 'a'), row('y', 'b', 'a'), row('b', 'a'), row('a')])
  assert.equal(layout.laneCount, 2)
  assert.deepEqual([layout.rows[1].lane, layout.rows[1].color], [1, 1])
  assertLines(layout.rows[1].lines, [
    line(0, 0, 'top', 'bottom', 0),
    line(1, 1, 'node', 'bottom', 1),
    line(1, 0, 'node', 'bottom', 0),
  ], 'merge into an existing lane')
}

// When several lanes expect the same merge parent, the leftmost one is joined.
{
  const layout = computeGitGraph([row('x', 'a'), row('y', 'a'), row('z', 'c', 'a')])
  assertLines(layout.rows[2].lines, [
    line(0, 0, 'top', 'bottom', 0),
    line(1, 1, 'top', 'bottom', 1),
    line(2, 2, 'node', 'bottom', 2),
    line(2, 0, 'node', 'bottom', 0),
  ], 'leftmost lane wins')
}

// Parents outside the loaded rows keep their lane open to the bottom.
{
  const layout = computeGitGraph([row('x', 'missing'), row('y', 'other')])
  assert.equal(layout.laneCount, 2)
  assertLines(layout.rows[1].lines, [line(0, 0, 'top', 'bottom', 0), line(1, 1, 'node', 'bottom', 1)], 'open lanes')
}

// An orphan root's node lane counts towards the width even though it closes at once.
{
  const layout = computeGitGraph([row('x', 'a'), row('orphan'), row('a')])
  assert.equal(layout.rows[1].lane, 1)
  assert.equal(layout.laneCount, 2)
  assertLines(layout.rows[1].lines, [line(0, 0, 'top', 'bottom', 0)], 'orphan root')
}

// Closing a lane shifts the lanes to its right one column left, keeping colors.
{
  const layout = computeGitGraph([row('t1', 'r1'), row('t2', 'r2'), row('r1'), row('r2')])
  assertLines(layout.rows[2].lines, [line(0, 0, 'top', 'node', 0), line(1, 0, 'top', 'bottom', 1)], 'shift left')
  assert.deepEqual([layout.rows[3].lane, layout.rows[3].color], [0, 1])
  assert.equal(layout.laneCount, 2)
}

// The uncommitted row draws a dashed neutral line into HEAD; HEAD gets a real color.
{
  const rows = buildGraphRows([commit('h', ['g']), commit('g')], { uncommitted: true, headHash: 'h' })
  assert.deepEqual(rows[0], { hash: UNCOMMITTED_ROW_HASH, parents: ['h'], virtual: true })
  const layout = computeGitGraph(rows)
  assert.deepEqual([layout.rows[0].lane, layout.rows[0].color, layout.rows[0].virtual], [0, VIRTUAL_COLOR, true])
  assertLines(layout.rows[0].lines, [line(0, 0, 'node', 'bottom', VIRTUAL_COLOR, true)], 'virtual row')
  assert.deepEqual([layout.rows[1].lane, layout.rows[1].color], [0, 0])
  assertLines(layout.rows[1].lines, [
    line(0, 0, 'top', 'node', VIRTUAL_COLOR, true),
    line(0, 0, 'node', 'bottom', 0),
  ], 'HEAD below the virtual row')
}

// A real lane and the virtual lane both expecting HEAD: HEAD takes the real lane
// and color, and the line below HEAD is solid.
{
  const rows = buildGraphRows([commit('o1', ['h']), commit('h', ['g']), commit('g')], { uncommitted: true, headHash: 'h' })
  const layout = computeGitGraph(rows)
  assert.deepEqual([layout.rows[1].lane, layout.rows[1].color], [1, 0])
  assertLines(layout.rows[1].lines, [line(0, 0, 'top', 'bottom', VIRTUAL_COLOR, true), line(1, 1, 'node', 'bottom', 0)], 'remote ahead')
  const head = layout.rows[2]
  assert.deepEqual([head.lane, head.color], [1, 0])
  assertLines(head.lines, [
    line(0, 1, 'top', 'node', VIRTUAL_COLOR, true),
    line(1, 1, 'top', 'node', 0),
    line(1, 0, 'node', 'bottom', 0),
  ], 'HEAD with virtual and real lanes')
  assert.equal(layout.rows[3].color, 0)
}

// Without a known HEAD the uncommitted row is a standalone node.
{
  const rows = buildGraphRows([commit('x')], { uncommitted: true, headHash: null })
  const layout = computeGitGraph(rows)
  assert.deepEqual(layout.rows[0].lines, [])
  assert.deepEqual([layout.rows[1].lane, layout.rows[1].lines], [0, []])
  assert.deepEqual(computeGitGraph([]), { rows: [], laneCount: 0 })
}

// HEAD detection prefers decorations and falls back to the first commit for scope "current".
{
  const commits = [commit('a'), commit('b', [], [ref('branch', 'main', true)])]
  assert.equal(findHeadHash(commits, 'all'), 'b')
  assert.equal(findHeadHash([commit('a'), commit('b', [], [ref('head', 'HEAD')])], 'all'), 'b')
  assert.equal(findHeadHash([commit('a'), commit('b')], 'current'), 'a')
  assert.equal(findHeadHash([commit('a'), commit('b')], 'all'), null)
  assert.equal(findHeadHash([], 'current'), null)
}

// Ref labels: detached HEAD, current branch, branches, remotes, tags, others.
{
  const refs = [
    ref('tag', 'v1'),
    ref('remote', 'origin/main'),
    ref('other', 'stash'),
    ref('branch', 'dev'),
    ref('branch', 'main', true),
    ref('head', 'HEAD'),
    ref('tag', 'v0'),
  ]
  assert.deepEqual(sortGraphRefs(refs).map(r => r.name), ['HEAD', 'main', 'dev', 'origin/main', 'v1', 'v0', 'stash'])
  const split = splitGraphRefs(refs, 3)
  assert.deepEqual(split.visible.map(r => r.name), ['HEAD', 'main', 'dev'])
  assert.deepEqual(split.hidden.map(r => r.name), ['origin/main', 'v1', 'v0', 'stash'])
}

// Graph log source: falls back from "all" to "current", remembers failures,
// logs each failing path + scope once, and retries after forget().
async function testGraphLogSource() {
  const calls: string[] = []
  const logs: string[] = []
  let allWorks = false
  const source = createGraphLogSource(async (cwd, count, scope) => {
    calls.push(`${cwd}:${scope}:${count}`)
    if (scope === 'all' && !allWorks) return null
    if (cwd === '/broken') throw new Error('boom')
    return [commit(`${scope}-head`)]
  }, message => logs.push(message))

  assert.deepEqual(await source.load('/repo', 'current', 50), { commits: [commit('current-head')], scope: 'current' })
  assert.deepEqual(await source.load('/repo', 'all', 150), { commits: [commit('current-head')], scope: 'current' })
  assert.deepEqual(calls, ['/repo:current:50', '/repo:all:150', '/repo:current:150'])
  assert.equal(logs.length, 1)

  calls.length = 0
  await source.load('/repo', 'all', 50)
  assert.deepEqual(calls, ['/repo:current:50'], 'a failed scope is not asked again')

  allWorks = true
  source.forget('/repo', 'all')
  assert.deepEqual(await source.load('/repo', 'all', 50), { commits: [commit('all-head')], scope: 'all' })

  allWorks = false
  source.forget('/repo')
  await source.load('/repo', 'all', 50)
  assert.equal(logs.length, 1, 'each path + scope is logged once')

  assert.equal(await source.load('/broken', 'current', 50), null)
  assert.equal(await source.load('/broken', 'all', 50), null)
  assert.equal(logs.length, 3)
  assert.ok(logs[1].includes('failed: boom'))
}

testGraphLogSource().then(() => {
  console.log('git-graph: passed')
}).catch(err => {
  console.error(err)
  process.exitCode = 1
})
