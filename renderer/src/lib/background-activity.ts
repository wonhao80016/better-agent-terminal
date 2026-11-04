import type { BackgroundTaskInfo, TerminalInstance } from '../types'

export const ACTIVITY_WINDOW_MS = 10_000

export type ActivityDotState = 'active' | 'background' | 'unread' | 'inactive'

export interface ActivitySnapshot {
  lastActivityTime: number | null
  agentRunning: boolean
  backgroundCount: number
  backgroundAgentCount: number
  backgroundAgentNames: string
  backgroundCommandNames: string
}

export const BACKGROUND_TOOLTIP_MAX_NAMES = 3

type ActivityTerminal = Pick<TerminalInstance, 'lastActivityTime' | 'isAgentRunning' | 'backgroundTasks'>

export function normalizeBackgroundTasks(value: unknown): BackgroundTaskInfo[] {
  if (!Array.isArray(value)) return []
  const tasks: BackgroundTaskInfo[] = []
  for (const raw of value) {
    if (!raw || typeof raw !== 'object') continue
    const item = raw as Record<string, unknown>
    if (typeof item.id !== 'string' || !item.id) continue
    tasks.push({
      id: item.id,
      type: typeof item.type === 'string' ? item.type : null,
      description: typeof item.description === 'string' ? item.description : '',
      ...(typeof item.subagentType === 'string' && item.subagentType ? { subagentType: item.subagentType } : {}),
      toolUseId: typeof item.toolUseId === 'string' && item.toolUseId ? item.toolUseId : null,
      startedAt: typeof item.startedAt === 'number' ? item.startedAt : 0,
      isAgent: item.isAgent === true,
    })
  }
  return tasks
}

export function sameBackgroundTasks(
  a: readonly BackgroundTaskInfo[] | undefined,
  b: readonly BackgroundTaskInfo[] | undefined,
): boolean {
  const left = a ?? []
  const right = b ?? []
  if (left.length !== right.length) return false
  return left.every((task, index) => {
    const other = right[index]
    return task.id === other.id
      && task.description === other.description
      && task.toolUseId === other.toolUseId
      && task.type === other.type
      && task.subagentType === other.subagentType
      && task.startedAt === other.startedAt
      && task.isAgent === other.isAgent
  })
}

export function backgroundTaskLabel(task: BackgroundTaskInfo): string {
  return task.description.trim() || task.subagentType || task.type || task.id
}

const BACKGROUND_KIND_LABELS: Record<string, string> = {
  local_bash: 'Bash',
  monitor: 'Monitor',
  monitor_mcp: 'Monitor',
  monitor_ws: 'Monitor',
  local_agent: 'Agent',
  local_workflow: 'Workflow',
  mcp_task: 'MCP',
}

export function backgroundTaskKind(task: BackgroundTaskInfo): string | null {
  if (!task.type) return null
  return BACKGROUND_KIND_LABELS[task.type] ?? task.type
}

function formatNames(labels: readonly string[]): string {
  const shown = labels.slice(0, BACKGROUND_TOOLTIP_MAX_NAMES).join(', ')
  const hidden = labels.length - BACKGROUND_TOOLTIP_MAX_NAMES
  return hidden > 0 ? `${shown} +${hidden}` : shown
}

// Every background task counts, but only agents hold the completion notification.
// A terminal with background tasks reports the main agent only through
// isAgentRunning, since background messages keep refreshing lastActivityTime.
export function summarizeActivity(terminals: readonly ActivityTerminal[]): ActivitySnapshot {
  let lastActivityTime: number | null = null
  let agentRunning = false
  const agentLabels: string[] = []
  const commandLabels: string[] = []
  for (const terminal of terminals) {
    const tasks = terminal.backgroundTasks ?? []
    if (tasks.length > 0) {
      for (const task of tasks) (task.isAgent ? agentLabels : commandLabels).push(backgroundTaskLabel(task))
      if (terminal.isAgentRunning) agentRunning = true
      continue
    }
    if (terminal.lastActivityTime !== undefined && terminal.lastActivityTime > (lastActivityTime ?? -Infinity)) {
      lastActivityTime = terminal.lastActivityTime
    }
  }
  return {
    lastActivityTime,
    agentRunning,
    backgroundCount: agentLabels.length + commandLabels.length,
    backgroundAgentCount: agentLabels.length,
    backgroundAgentNames: formatNames(agentLabels),
    backgroundCommandNames: formatNames(commandLabels),
  }
}

export function backgroundActivityTooltip(
  snapshot: ActivitySnapshot,
  t: (key: string, options: { count: number; names: string }) => string,
): string | undefined {
  const commandCount = snapshot.backgroundCount - snapshot.backgroundAgentCount
  const lines: string[] = []
  if (snapshot.backgroundAgentCount > 0) {
    lines.push(t('notifications.backgroundAgentsTooltip', {
      count: snapshot.backgroundAgentCount,
      names: snapshot.backgroundAgentNames,
    }))
  }
  if (commandCount > 0) {
    lines.push(t('notifications.backgroundCommandsTooltip', {
      count: commandCount,
      names: snapshot.backgroundCommandNames,
    }))
  }
  return lines.length > 0 ? lines.join('\n') : undefined
}

export function resolveActivityDotState(input: {
  isActive: boolean
  backgroundCount: number
  hasUnread: boolean
}): ActivityDotState {
  if (input.isActive) return 'active'
  if (input.backgroundCount > 0) return 'background'
  return input.hasUnread ? 'unread' : 'inactive'
}
