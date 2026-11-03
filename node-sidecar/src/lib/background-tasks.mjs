// Live, non-ambient background tasks per session. The SDK's background_tasks_changed
// level frame is authoritative; edge events are a fallback until the CLI's first one.
// Only live agent tasks make a completed turn hold its notification.

import { sendEvent } from './protocol.mjs'

// Legacy CLIs: main-thread silence after the last agent ends before announcing.
export const BACKGROUND_SETTLE_GRACE_MS = 10_000
// A held completion whose session_state_changed 'idle' never arrives.
export const SESSION_IDLE_WATCHDOG_MS = 30_000
// A legacy self-turn starting this soon after an announcement reports the same work.
export const SELF_TURN_DEDUPE_MS = 120_000

const DEFAULT_TIMINGS = {
  graceMs: BACKGROUND_SETTLE_GRACE_MS,
  idleWatchdogMs: SESSION_IDLE_WATCHDOG_MS,
  selfTurnDedupeMs: SELF_TURN_DEDUPE_MS,
}
let timings = { ...DEFAULT_TIMINGS }

export function __setBackgroundTimingsForTests(overrides) {
  timings = { ...DEFAULT_TIMINGS, ...(overrides || {}) }
}

// SDK task types that run a subagent and report back to the main thread.
const AGENT_TASK_TYPES = new Set(['local_agent', 'local_workflow'])
const TERMINAL_STATUSES = new Set(['completed', 'failed', 'killed', 'stopped'])
const SESSION_STATES = new Set(['idle', 'running', 'requires_action'])

// The host notifies once per work id, so ids stay unique across trackers and sidecar restarts.
let lastWorkId = Date.now()
function nextWorkId() {
  lastWorkId += 1
  return lastWorkId
}

function trackerOf(s) {
  if (!s.backgroundTracker) {
    s.backgroundTracker = {
      tasks: new Map(),
      launchKeys: new Set(),
      levelSeen: false,
      // null until the first emit, so a fresh tracker always publishes its list.
      lastSentKey: null,
      awaitingSettle: false,
      lastResult: null,
      heldWorkId: 0,
      mainTurnActive: false,
      settleTimer: null,
      // null until the CLI sends session_state_changed; then the legacy grace path is off.
      sessionState: null,
      workId: nextWorkId(),
      settledWorkId: null,
      settledAt: 0,
    }
  }
  return s.backgroundTracker
}

function nonEmptyString(value) {
  return typeof value === 'string' && value ? value : null
}

function isAgentTask(type, subagentType) {
  return AGENT_TASK_TYPES.has(type) || (!type && !!subagentType)
}

function agentCount(tracker) {
  let count = 0
  for (const entry of tracker.tasks.values()) if (entry.isAgent) count += 1
  return count
}

export function backgroundTaskList(s) {
  const tracker = s?.backgroundTracker
  if (!tracker) return []
  return [...tracker.tasks.values()].sort((a, b) => a.startedAt - b.startedAt)
}

export function backgroundTaskCount(s) {
  return s?.backgroundTracker ? s.backgroundTracker.tasks.size : 0
}

export function isKnownBackgroundTask(s, taskId, toolUseId) {
  const tracker = s?.backgroundTracker
  if (!tracker) return false
  for (const [key, entry] of tracker.tasks) {
    if ((taskId && key === taskId) || (toolUseId && entry.toolUseId === toolUseId)) return true
  }
  return false
}

function emitIfChanged(s, sessionId) {
  const tracker = trackerOf(s)
  const tasks = backgroundTaskList(s)
  const key = JSON.stringify(tasks)
  if (key === tracker.lastSentKey) return
  tracker.lastSentKey = key
  sendEvent('claude:background-tasks', { sessionId, tasks })
}

// Publishes the list for a CLI process about to start; a new tracker sends an
// empty list so a renderer that outlived the previous sidecar drops stale rows.
export function syncBackgroundTasks(s, sessionId) {
  emitIfChanged(s, sessionId)
}

function cancelSettle(tracker) {
  if (tracker.settleTimer) {
    clearTimeout(tracker.settleTimer)
    tracker.settleTimer = null
  }
}

function announce(s, sessionId, tracker) {
  cancelSettle(tracker)
  tracker.awaitingSettle = false
  tracker.settledWorkId = tracker.heldWorkId
  tracker.settledAt = Date.now()
  const result = tracker.lastResult
  tracker.lastResult = null
  sendEvent('claude:background-settled', {
    sessionId,
    workId: tracker.heldWorkId,
    ...(result ? { result } : {}),
    ...(s.sdkSessionId ? { sdkSessionId: s.sdkSessionId } : {}),
  })
}

function armSettle(s, sessionId, tracker, ms) {
  cancelSettle(tracker)
  tracker.settleTimer = setTimeout(() => {
    tracker.settleTimer = null
    if (s.backgroundTracker !== tracker || !canSettle(tracker)) return
    announce(s, sessionId, tracker)
  }, ms)
  tracker.settleTimer.unref?.()
}

function canSettle(tracker) {
  return tracker.awaitingSettle && !tracker.mainTurnActive && agentCount(tracker) === 0
}

// With session_state_changed the held completion is announced on 'idle'; a
// timer only covers an agent list emptying while idle (a self-turn may still
// follow) and an 'idle' that never comes. Legacy CLIs rely on the grace timer.
function reviewSettle(s, sessionId, tracker, idleNow = false) {
  if (!canSettle(tracker)) {
    cancelSettle(tracker)
    return
  }
  switch (tracker.sessionState) {
    case null:
      armSettle(s, sessionId, tracker, timings.graceMs)
      return
    case 'idle':
      if (idleNow) announce(s, sessionId, tracker)
      else armSettle(s, sessionId, tracker, timings.graceMs)
      return
    case 'running':
      armSettle(s, sessionId, tracker, timings.idleWatchdogMs)
      return
    default:
      cancelSettle(tracker)
  }
}

function commit(s, sessionId, beforeAgents) {
  const tracker = trackerOf(s)
  emitIfChanged(s, sessionId)
  const afterAgents = agentCount(tracker)
  if (afterAgents > 0) cancelSettle(tracker)
  else if (beforeAgents > 0) reviewSettle(s, sessionId, tracker)
}

function entryFrom(fields, prev, tracked) {
  const subagentType = nonEmptyString(fields.subagentType) ?? prev?.subagentType ?? tracked?.subagentType ?? null
  const type = nonEmptyString(fields.type) ?? prev?.type ?? tracked?.type ?? null
  return {
    id: fields.id,
    type,
    description: nonEmptyString(fields.description) ?? prev?.description ?? nonEmptyString(tracked?.description) ?? '',
    ...(subagentType ? { subagentType } : {}),
    toolUseId: nonEmptyString(fields.toolUseId) ?? prev?.toolUseId ?? tracked?.toolUseId ?? null,
    startedAt: prev?.startedAt ?? tracked?.startedAt ?? Date.now(),
    isAgent: fields.launchOnly === true ? false : isAgentTask(type, subagentType),
  }
}

export function applyBackgroundLevel(s, sessionId, msg) {
  const tracker = trackerOf(s)
  const before = agentCount(tracker)
  tracker.levelSeen = true
  const next = new Map()
  for (const raw of Array.isArray(msg?.tasks) ? msg.tasks : []) {
    const id = nonEmptyString(raw?.task_id)
    // Ambient tasks (skip_transcript work, live-update watchers) are not
    // activity per the SDK contract and must not hold the indicator on.
    if (!id || raw.ambient === true) continue
    next.set(id, entryFrom({
      id,
      type: raw.task_type,
      description: raw.description,
      subagentType: raw.subagent_type,
    }, tracker.tasks.get(id), s.activeTasks?.get(id)))
  }
  tracker.tasks = next
  commit(s, sessionId, before)
}

function removeMatching(tracker, taskId, toolUseId) {
  for (const [key, entry] of tracker.tasks) {
    if ((taskId && key === taskId) || (toolUseId && entry.toolUseId === toolUseId)) tracker.tasks.delete(key)
  }
}

export function noteTaskStarted(s, sessionId, msg) {
  const tracker = trackerOf(s)
  const taskId = nonEmptyString(msg?.task_id)
  if (!taskId) return
  const toolUseId = nonEmptyString(msg.tool_use_id)
  if (tracker.levelSeen) {
    // The level frame carries no tool_use_id; bind it here so the renderer
    // can match the entry to its Agent/Bash tool row.
    const existing = tracker.tasks.get(taskId)
    if (existing && !existing.toolUseId && toolUseId) {
      tracker.tasks.set(taskId, { ...existing, toolUseId })
      emitIfChanged(s, sessionId)
    }
    return
  }
  // A task bound to a run_in_background launch is that background task, even
  // from CLIs that do not send is_backgrounded.
  const adoptsLaunch = !!toolUseId && tracker.launchKeys.has(toolUseId) && tracker.tasks.has(toolUseId)
  if ((msg.is_backgrounded !== true && !adoptsLaunch) || msg.ambient === true || msg.skip_transcript === true) return
  const mirror = s.activeTasks?.get(taskId)
  if (mirror) mirror.isBackground = true
  const before = agentCount(tracker)
  if (toolUseId) tracker.tasks.delete(toolUseId)
  tracker.tasks.set(taskId, entryFrom({
    id: taskId,
    type: msg.task_type,
    description: msg.description,
    subagentType: msg.subagent_type,
    toolUseId,
  }, tracker.tasks.get(taskId), s.activeTasks?.get(taskId)))
  commit(s, sessionId, before)
}

export function noteTaskUpdated(s, sessionId, task, patch) {
  const tracker = trackerOf(s)
  if (tracker.levelSeen || !task?.id) return
  const before = agentCount(tracker)
  if (TERMINAL_STATUSES.has(task.status)) {
    removeMatching(tracker, task.id, nonEmptyString(task.toolUseId))
  } else if (patch?.is_backgrounded === true && task.ambient !== true && task.skipTranscript !== true) {
    tracker.tasks.set(task.id, entryFrom({
      id: task.id,
      type: task.type,
      description: task.description,
      subagentType: task.subagentType,
      toolUseId: task.toolUseId,
    }, tracker.tasks.get(task.id), null))
  } else {
    return
  }
  commit(s, sessionId, before)
}

// knownToolUseId: the tool_use id the sidecar bound to this task id, for
// notifications that omit tool_use_id (the field is optional in the SDK).
export function noteTaskNotification(s, sessionId, msg, knownToolUseId = null) {
  const tracker = trackerOf(s)
  if (tracker.levelSeen) return
  const taskId = nonEmptyString(msg?.task_id)
  const toolUseId = nonEmptyString(msg?.tool_use_id) ?? nonEmptyString(knownToolUseId)
  if (!taskId && !toolUseId) return
  const before = agentCount(tracker)
  removeMatching(tracker, taskId, toolUseId)
  commit(s, sessionId, before)
}

// Marking the mirror background keeps it through the turn-end cleanup.
function adoptBackgroundTask(s, sessionId, tracker, task) {
  task.isBackground = true
  if (tracker.tasks.has(task.id)) return
  const before = agentCount(tracker)
  tracker.tasks.set(task.id, entryFrom({
    id: task.id,
    type: task.type,
    description: task.description,
    subagentType: task.subagentType,
    toolUseId: task.toolUseId,
  }, null, null))
  commit(s, sessionId, before)
}

// A successful run_in_background tool_result only means "launched". Used only
// when the CLI reported no task event for that tool call. Nothing reliably
// ends such an entry, so it is listed but never holds a notification.
export function noteBackgroundLaunch(s, sessionId, toolCall) {
  const tracker = trackerOf(s)
  if (tracker.levelSeen) return
  const toolUseId = nonEmptyString(toolCall?.id)
  if (!toolUseId || toolCall.input?.run_in_background !== true) return
  for (const entry of tracker.tasks.values()) {
    if (entry.id === toolUseId || entry.toolUseId === toolUseId) return
  }
  for (const task of s.activeTasks?.values() || []) {
    if (task.toolUseId !== toolUseId) continue
    if (task.ambient !== true && task.skipTranscript !== true) adoptBackgroundTask(s, sessionId, tracker, task)
    return
  }
  const input = toolCall.input || {}
  const before = agentCount(tracker)
  tracker.launchKeys.add(toolUseId)
  tracker.tasks.set(toolUseId, entryFrom({
    id: toolUseId,
    type: toolCall.toolName,
    description: nonEmptyString(input.description) ?? nonEmptyString(input.command) ?? toolCall.toolName,
    subagentType: input.subagent_type,
    toolUseId,
    launchOnly: true,
  }, null, null))
  commit(s, sessionId, before)
}

// A legacy self-turn that opens right after an announcement is the SDK
// reporting the work that announcement already covered.
function continuesSettledWork(tracker) {
  return tracker.sessionState === null
    && tracker.settledWorkId === tracker.workId
    && Date.now() - tracker.settledAt < timings.selfTurnDedupeMs
}

function beginMainTurn(tracker, userInitiated) {
  if (userInitiated || (!tracker.mainTurnActive && !continuesSettledWork(tracker))) tracker.workId = nextWorkId()
  tracker.mainTurnActive = true
  cancelSettle(tracker)
}

// A new user turn ends a wait for 'idle': the CLI does not report 'idle' between
// queued turns, and a later error or interrupt would otherwise drop the held completion.
export function noteUserTurn(s, sessionId) {
  const tracker = trackerOf(s)
  if (tracker.awaitingSettle && agentCount(tracker) === 0) announce(s, sessionId, tracker)
  beginMainTurn(tracker, true)
}

// Main-thread output after a result means a new turn is running (the user's
// or one the SDK opened for a finished task). Subagent frames carry
// parent_tool_use_id and are background activity, not a turn.
export function noteMainThreadActivity(s, msg) {
  if (msg?.parent_tool_use_id) return
  beginMainTurn(trackerOf(s), false)
}

export function noteSessionState(s, sessionId, state) {
  if (!SESSION_STATES.has(state)) return
  const tracker = trackerOf(s)
  tracker.sessionState = state
  if (state === 'running') {
    beginMainTurn(tracker, false)
    return
  }
  if (state === 'idle') tracker.mainTurnActive = false
  reviewSettle(s, sessionId, tracker, state === 'idle')
}

// Without level frames a task entry is live only while the sidecar's task
// mirror (s.activeTasks) still tracks it, so a missed bookend cannot wedge it.
function reconcileWithTaskMirror(s, tracker) {
  if (tracker.levelSeen) return false
  let changed = false
  for (const key of [...tracker.tasks.keys()]) {
    if (tracker.launchKeys.has(key) || s.activeTasks?.has(key)) continue
    tracker.tasks.delete(key)
    changed = true
  }
  return changed
}

// Called for every result frame. pendingBackground counts live background
// agents; awaitingIdle asks the host to hold the notification until the
// sidecar announces the end of the work with claude:background-settled.
export function noteTurnResult(s, sessionId, { outcome, result }) {
  const tracker = trackerOf(s)
  tracker.mainTurnActive = false
  if (reconcileWithTaskMirror(s, tracker)) emitIfChanged(s, sessionId)
  const pendingBackground = agentCount(tracker)
  const workId = tracker.workId
  const idle = tracker.sessionState === null || tracker.sessionState === 'idle'
  if (outcome !== 'completed') {
    // Mirrors the host: an error ends the wait, an interrupt keeps it while agents run.
    if (outcome === 'error' || pendingBackground === 0) {
      tracker.awaitingSettle = false
      tracker.lastResult = null
    }
    reviewSettle(s, sessionId, tracker)
    return { pendingBackground, awaitingIdle: false, workId }
  }
  if (pendingBackground === 0 && (idle || !tracker.awaitingSettle)) {
    // Without live agents or a held completion, this turn-end is the notification itself.
    tracker.awaitingSettle = false
    tracker.lastResult = null
    cancelSettle(tracker)
    return { pendingBackground, awaitingIdle: false, workId }
  }
  // Work held after an announcement is new work, even inside a deduped self-turn.
  if (tracker.workId === tracker.settledWorkId) tracker.workId = nextWorkId()
  tracker.awaitingSettle = true
  tracker.heldWorkId = tracker.workId
  tracker.lastResult = typeof result === 'string' && result ? result : tracker.lastResult
  reviewSettle(s, sessionId, tracker)
  return { pendingBackground, awaitingIdle: !idle, workId: tracker.workId }
}

// A turn that ended without a result frame (thrown interrupt, abort, error).
export function noteTurnEndedWithoutResult(s, sessionId, reason) {
  const tracker = s?.backgroundTracker
  if (!tracker) return
  tracker.mainTurnActive = false
  if (reason !== 'interrupted') {
    tracker.awaitingSettle = false
    tracker.lastResult = null
  }
  reviewSettle(s, sessionId, tracker)
}

// The CLI process is gone: its background tasks died with it and the level
// signal restarts empty on the next process. A completion still held for them
// is announced unless the user aborted, since no turn-end will follow.
export function resetBackgroundTasks(s, sessionId, { announceHeld = false } = {}) {
  const tracker = s?.backgroundTracker
  if (!tracker) return
  cancelSettle(tracker)
  const shouldAnnounce = announceHeld && tracker.awaitingSettle && !!sessionId
  tracker.tasks.clear()
  tracker.launchKeys.clear()
  tracker.levelSeen = false
  tracker.sessionState = null
  tracker.mainTurnActive = false
  if (sessionId) emitIfChanged(s, sessionId)
  else tracker.lastSentKey = '[]'
  if (shouldAnnounce) announce(s, sessionId, tracker)
  tracker.awaitingSettle = false
  tracker.lastResult = null
}
