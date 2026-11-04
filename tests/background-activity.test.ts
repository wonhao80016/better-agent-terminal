// Background tasks that outlive the main turn: indicator state, the background
// count badge and tooltip, the workspace-store mirror of the host list, and the Active Tasks bar.

import * as assert from 'node:assert/strict'
import { createElement } from 'react'
import { renderToStaticMarkup } from 'react-dom/server'
import { readFileSync } from 'node:fs'
import i18next from 'i18next'

;(globalThis as { window?: unknown }).window = {
  __TAURI_INTERNALS__: { invoke: async () => undefined },
  batAppAPI: { debug: { isDebugMode: false } },
  location: { search: '' },
}

async function main() {
  const {
    backgroundActivityTooltip,
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
  const fakeT = (key: string, options: { count: number; names: string }) => `${key}|${options.count}|${options.names}`

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

  const emptyBackground = {
    backgroundCount: 0, backgroundAgentCount: 0, backgroundAgentNames: '', backgroundCommandNames: '',
  }
  // Without background tasks the snapshot is the latest activity time.
  const idle = summarizeActivity([
    { lastActivityTime: 100 },
    { lastActivityTime: 300 },
    { lastActivityTime: undefined },
  ])
  assert.deepEqual(idle, { lastActivityTime: 300, agentRunning: false, ...emptyBackground })
  assert.deepEqual(summarizeActivity([]), { lastActivityTime: null, agentRunning: false, ...emptyBackground })
  assert.equal(backgroundActivityTooltip(idle, fakeT), undefined)
  assert.equal(resolveActivityDotState({ isActive: false, backgroundCount: idle.backgroundCount, hasUnread: true }), 'unread')

  // A terminal with background tasks ignores its own activity clock (background
  // messages keep refreshing it) and counts as active only while its main agent runs.
  const busy = summarizeActivity([
    { lastActivityTime: 900, isAgentRunning: false, backgroundTasks: tasks },
    { lastActivityTime: 200 },
  ])
  assert.equal(busy.lastActivityTime, 200)
  assert.equal(busy.agentRunning, false)
  assert.equal(busy.backgroundCount, 2, 'agents and shells both count')
  assert.equal(busy.backgroundAgentCount, 1)
  assert.equal(busy.backgroundAgentNames, 'research')
  assert.equal(busy.backgroundCommandNames, 'local_bash')
  assert.equal(backgroundActivityTooltip(busy, fakeT),
    'notifications.backgroundAgentsTooltip|1|research\nnotifications.backgroundCommandsTooltip|1|local_bash')
  assert.equal(resolveActivityDotState({ isActive: busy.agentRunning, backgroundCount: busy.backgroundCount, hasUnread: true }), 'background')
  const mainRunning = summarizeActivity([{ backgroundTasks: tasks, isAgentRunning: true }])
  assert.equal(mainRunning.agentRunning, true)
  assert.equal(resolveActivityDotState({ isActive: mainRunning.agentRunning, backgroundCount: mainRunning.backgroundCount, hasUnread: false }), 'active',
    'a running main agent stays yellow over background tasks')

  // A background shell alone (release build, dev server) turns the dot blue with a count of 1.
  const build = { id: 'b-2', type: 'local_bash', description: 'cargo build --release', toolUseId: 'tu-b', startedAt: 30, isAgent: false }
  const shellOnly = summarizeActivity([{ lastActivityTime: 700, isAgentRunning: false, backgroundTasks: [build] }])
  assert.deepEqual(shellOnly, {
    lastActivityTime: null, agentRunning: false,
    backgroundCount: 1, backgroundAgentCount: 0, backgroundAgentNames: '', backgroundCommandNames: 'cargo build --release',
  })
  assert.equal(resolveActivityDotState({ isActive: false, backgroundCount: shellOnly.backgroundCount, hasUnread: true }), 'background')
  assert.equal(backgroundActivityTooltip(shellOnly, fakeT), 'notifications.backgroundCommandsTooltip|1|cargo build --release')

  // Workspace totals add up across terminals; long name lists are cut with +K.
  const shells = ['a', 'b', 'c', 'd', 'e'].map((name, index) => ({ ...build, id: `s-${name}`, description: name, startedAt: index }))
  const workspace = summarizeActivity([
    { backgroundTasks: shells.slice(0, 2) },
    { backgroundTasks: [...shells.slice(2), tasks[0]] },
  ])
  assert.equal(workspace.backgroundCount, 6)
  assert.equal(workspace.backgroundAgentCount, 1)
  assert.equal(workspace.backgroundCommandNames, 'a, b, c +2')

  const locales = Object.fromEntries(['en', 'zh-TW', 'zh-CN', 'ja'].map(lng => [
    lng,
    JSON.parse(readFileSync(new URL(`../renderer/src/locales/${lng}.json`, import.meta.url), 'utf8')),
  ]))
  const localeI18n = i18next.createInstance()
  await localeI18n.init({
    lng: 'en',
    resources: Object.fromEntries(Object.entries(locales).map(([lng, translation]) => [lng, { translation }])),
    interpolation: { escapeValue: false },
  })
  for (const lng of Object.keys(locales)) {
    const tooltip = backgroundActivityTooltip(summarizeActivity([{ backgroundTasks: [tasks[0], build] }]), localeI18n.getFixedT(lng))
    const lines = tooltip?.split('\n') ?? []
    assert.equal(lines.length, 2, `${lng}: one line per group`)
    assert.match(lines[0], /×1.*research$/, `${lng}: agent group`)
    assert.match(lines[1], /×1.*cargo build --release$/, `${lng}: command group`)
    assert.ok(!tooltip?.includes('notifications.'), `${lng}: keys resolve`)
  }

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
