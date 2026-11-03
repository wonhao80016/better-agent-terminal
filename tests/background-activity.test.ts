// Background tasks that outlive the main turn: indicator state, the agent count
// badge, the workspace-store mirror of the host list, and the Active Tasks bar.

import * as assert from 'node:assert/strict'
import { createElement } from 'react'
import { renderToStaticMarkup } from 'react-dom/server'
import i18next from 'i18next'

;(globalThis as { window?: unknown }).window = {
  __TAURI_INTERNALS__: { invoke: async () => undefined },
  batAppAPI: { debug: { isDebugMode: false } },
  location: { search: '' },
}

async function main() {
  const {
    normalizeBackgroundTasks,
    resolveActivityDotState,
    sameBackgroundTasks,
    summarizeActivity,
  } = await import('../renderer/src/lib/background-activity.ts')
  const {
    backgroundTasksOutsideTree,
    buildAgentTaskTree,
    countForegroundRunning,
    liveBackgroundIds,
  } = await import('../renderer/src/lib/agent-task-tree.ts')

  // --- indicator precedence: active (yellow) > background (blue) > unread > inactive
  assert.equal(resolveActivityDotState({ isActive: true, backgroundCount: 2, hasUnread: true }), 'active')
  assert.equal(resolveActivityDotState({ isActive: false, backgroundCount: 2, hasUnread: true }), 'background')
  assert.equal(resolveActivityDotState({ isActive: false, backgroundCount: 0, hasUnread: true }), 'unread')
  assert.equal(resolveActivityDotState({ isActive: false, backgroundCount: 0, hasUnread: false }), 'inactive')

  const tasks = normalizeBackgroundTasks([
    { id: 't-1', type: 'local_agent', description: 'research', toolUseId: 'tu-1', startedAt: 10, isAgent: true },
    { id: 'b-1', type: 'local_bash', description: '', startedAt: 20, isAgent: false },
    { id: '', description: 'dropped: no id' },
    'junk',
  ])
  assert.deepEqual(tasks.map(t => t.id), ['t-1', 'b-1'])
  assert.equal(tasks[1].toolUseId, null)
  assert.deepEqual(tasks.map(t => t.isAgent), [true, false])
  assert.equal(normalizeBackgroundTasks([{ id: 'x' }])[0].isAgent, false, 'a missing flag is not an agent')

  // Without background agents the snapshot is the latest activity time.
  const idle = summarizeActivity([
    { lastActivityTime: 100 },
    { lastActivityTime: 300 },
    { lastActivityTime: undefined },
  ])
  assert.deepEqual(idle, { lastActivityTime: 300, agentRunning: false, backgroundCount: 0, backgroundLabels: '' })
  assert.deepEqual(summarizeActivity([]), { lastActivityTime: null, agentRunning: false, backgroundCount: 0, backgroundLabels: '' })

  // A terminal with background agents ignores its own activity clock (subagent
  // output keeps refreshing it) and counts as active only while its main agent runs.
  const busy = summarizeActivity([
    { lastActivityTime: 900, isAgentRunning: false, backgroundTasks: tasks },
    { lastActivityTime: 200 },
  ])
  assert.equal(busy.lastActivityTime, 200)
  assert.equal(busy.agentRunning, false)
  assert.equal(busy.backgroundCount, 1, 'only agents count')
  assert.equal(busy.backgroundLabels, 'research')
  assert.equal(summarizeActivity([{ backgroundTasks: tasks, isAgentRunning: true }]).agentRunning, true)

  // A dev server alone neither turns the dot blue nor hides the unread state.
  const shellOnly = summarizeActivity([{ lastActivityTime: 700, backgroundTasks: [tasks[1]] }])
  assert.deepEqual(shellOnly, { lastActivityTime: 700, agentRunning: false, backgroundCount: 0, backgroundLabels: '' })
  assert.equal(resolveActivityDotState({ isActive: false, backgroundCount: shellOnly.backgroundCount, hasUnread: true }), 'unread')

  assert.equal(sameBackgroundTasks(undefined, []), true)
  assert.equal(sameBackgroundTasks(tasks, normalizeBackgroundTasks(JSON.parse(JSON.stringify(tasks)))), true)
  assert.equal(sameBackgroundTasks(tasks, tasks.slice(1)), false)
  assert.equal(sameBackgroundTasks(tasks, tasks.map(t => ({ ...t, isAgent: !t.isAgent }))), false)

  // --- store mirror of the host list
  const { workspaceStore } = await import('../renderer/src/stores/workspace-store.ts')
  ;(workspaceStore as unknown as { applySerializedData(data: string): void }).applySerializedData(JSON.stringify({
    workspaces: [{ id: 'ws', name: 'ws', folderPath: '/ws', createdAt: 1 }],
    activeWorkspaceId: 'ws',
    activeTerminalId: 'agent',
    terminals: [{ id: 'agent', workspaceId: 'ws', type: 'terminal', agentPreset: 'claude-code', title: 'agent', cwd: '/ws' }],
  }))
  let notified = 0
  const unsubscribe = workspaceStore.subscribe(() => { notified += 1 })
  workspaceStore.setTerminalBackgroundTasks('agent', tasks)
  const stored = () => workspaceStore.getState().terminals.find(t => t.id === 'agent')
  assert.deepEqual(stored()?.backgroundTasks?.map(t => t.id), ['t-1', 'b-1'])
  assert.equal(notified, 1)
  workspaceStore.setTerminalBackgroundTasks('agent', normalizeBackgroundTasks(JSON.parse(JSON.stringify(tasks))))
  assert.equal(notified, 1, 'an identical list must not re-render every indicator')
  workspaceStore.setTerminalBackgroundTasks('agent', [])
  assert.equal(stored()?.backgroundTasks, undefined)
  assert.equal(notified, 2)
  workspaceStore.setTerminalBackgroundTasks('unknown-terminal', tasks)
  assert.equal(notified, 2)
  unsubscribe()

  // A repaired duplicate identity is a different session: it must not inherit the list.
  workspaceStore.setTerminalBackgroundTasks('agent', tasks)
  const replacementId = workspaceStore.repairTerminalIdentityCollision('agent')
  assert.ok(replacementId)
  const replacement = workspaceStore.getState().terminals.find(t => t.id === replacementId)
  assert.equal(replacement?.backgroundTasks, undefined)

  // --- Active Tasks bar: background agents stay running, others get a section
  const messages = [{
    id: 'tu-1', sessionId: 's', toolName: 'Agent', status: 'completed' as const, timestamp: 1,
    input: { description: 'research', run_in_background: true },
  }]
  const live = liveBackgroundIds(tasks)
  const roots = buildAgentTaskTree(messages, new Map(), new Map(), live)
  assert.equal(roots[0].status, 'running', 'a launched background agent is still running')
  assert.equal(roots[0].isBackground, true)
  assert.equal(buildAgentTaskTree(messages, new Map(), new Map())[0].status, 'completed',
    'without a live background entry the finished tool block decides, as before')
  assert.deepEqual(backgroundTasksOutsideTree(roots, tasks).map(t => t.id), ['b-1'])
  assert.equal(countForegroundRunning(roots), 0, 'a background agent does not tick the whole panel')
  const foreground = buildAgentTaskTree([{ ...messages[0], id: 'tu-f', status: 'running' as const, input: { description: 'fg' } }], new Map())
  assert.equal(countForegroundRunning(foreground), 1)
  const lifecycle = new Map([['t-2', { id: 't-2', toolUseId: 'tu-1' }]])
  assert.deepEqual(
    backgroundTasksOutsideTree(roots, [{ id: 't-2', toolUseId: null }], lifecycle),
    [],
    'a level entry bound only through the lifecycle map is still the agent node',
  )

  const { AgentActivityTree } = await import('../renderer/src/components/AgentActivityTree.tsx')
  const i18n = i18next.createInstance()
  await i18n.init({ lng: 'en', resources: { en: { translation: { claude: {
    bg: 'bg', backgroundRunning: 'Background running {{count}}', backgroundTasksTitle: 'Background',
    agentTreeTitle: 'Agents', agentsRunning: '{{count}} running',
  } } } } })
  const props = {
    roots,
    summary: { running: 1, completed: 0, error: 0, total: 1 },
    onToggleExpanded: () => {},
    onHide: () => {},
    streamingText: new Map<string, string>(),
    onOpenTask: () => {},
    onStopTask: () => {},
    t: i18n.t.bind(i18n),
    backgroundCount: 2,
    backgroundTasks: backgroundTasksOutsideTree(roots, tasks),
  }
  const bar = renderToStaticMarkup(createElement(AgentActivityTree, { ...props, expanded: false }))
  assert.match(bar, /Background running 2/)
  assert.match(bar, /research/)
  assert.match(bar, /local_bash/, 'a task without description falls back to its type')
  assert.match(bar, /Bash/)
  const tree = renderToStaticMarkup(createElement(AgentActivityTree, { ...props, expanded: true }))
  assert.match(tree, /claude-agent-tree-section">Background</)
  assert.match(tree, /Background running 2/)
  const noBackground = renderToStaticMarkup(createElement(AgentActivityTree, {
    ...props, expanded: false, backgroundCount: 0, backgroundTasks: [],
  }))
  assert.doesNotMatch(noBackground, /is-background/)

  console.log('background activity: passed')
}

main().catch(error => {
  console.error(error instanceof Error ? error.stack || error.message : String(error))
  process.exit(1)
})
