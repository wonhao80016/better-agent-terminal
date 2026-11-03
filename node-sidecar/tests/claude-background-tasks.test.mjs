// Background task tracking and the deferred completion notification.
// Run with: node node-sidecar/tests/claude-background-tasks.test.mjs

import * as assert from 'node:assert/strict'
import { mkdtempSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

const { dispatch } = await import('../src/server.mjs')
const { __setSdkOverrideForTests } = await import('../src/lib/sdk-loader.mjs')
const { __setSendEventForTests } = await import('../src/lib/protocol.mjs')
const { sessions, ensureSession } = await import('../src/lib/state.mjs')
const bg = await import('../src/lib/background-tasks.mjs')

const GRACE_MS = 40
const WATCHDOG_MS = 80
bg.__setBackgroundTimingsForTests({ graceMs: GRACE_MS, idleWatchdogMs: WATCHDOG_MS })

let nextRequestId = 1
function send(method, params) {
  return dispatch({ jsonrpc: '2.0', id: nextRequestId++, method, params })
}

const events = []
const restoreSendEvent = __setSendEventForTests((name, params) => {
  events.push({ name, params })
})

function eventsFor(sessionId, name) {
  return events.filter(e => e.name === name && e.params?.sessionId === sessionId)
}

function sleep(ms) {
  return new Promise(resolve => setTimeout(resolve, ms))
}

async function waitFor(predicate, what, ms = 2000) {
  const deadline = Date.now() + ms
  while (Date.now() < deadline) {
    if (predicate()) return
    await sleep(5)
  }
  throw new Error(`timed out waiting for ${what}`)
}

// A fake SDK whose output frames are fed by the test, independent of pushes,
// so frames the CLI emits between turns can be replayed.
function scriptedSdk() {
  const frames = []
  let wake = null
  let ended = false
  const state = { pushed: [], stopped: [], options: null }
  const sdk = {
    state,
    feed(...msgs) {
      frames.push(...msgs)
      wake?.()
    },
    end() {
      ended = true
      wake?.()
    },
    query({ prompt, options }) {
      state.options = options
      ;(async () => {
        for await (const message of prompt) state.pushed.push(message)
      })()
      const generator = (async function* run() {
        while (true) {
          if (frames.length > 0) {
            yield frames.shift()
            continue
          }
          if (ended) return
          await new Promise(resolve => { wake = resolve })
          wake = null
        }
      })()
      generator.stopTask = async (taskId) => { state.stopped.push(taskId) }
      generator.interrupt = async () => {}
      return generator
    },
  }
  return sdk
}

const result = (text) => ({
  type: 'result',
  subtype: 'success',
  result: text,
  session_id: 'sdk-session',
  duration_ms: 1,
  num_turns: 1,
  is_error: false,
})
const assistantText = (text, parent = null) => ({
  type: 'assistant',
  parent_tool_use_id: parent,
  message: { content: [{ type: 'text', text }] },
})
const level = (tasks) => ({ type: 'system', subtype: 'background_tasks_changed', tasks })
const sessionState = (state) => ({ type: 'system', subtype: 'session_state_changed', state })
const agentTask = (id, description = 'research') => ({ task_id: id, task_type: 'local_agent', description, subagent_type: 'general-purpose' })
const bashTask = (id, description = 'npm run dev') => ({ task_id: id, task_type: 'local_bash', description })

const cwd = mkdtempSync(join(tmpdir(), 'bat-background-'))
let failures = 0

async function check(name, fn) {
  const sdk = scriptedSdk()
  __setSdkOverrideForTests(sdk)
  const sessionId = `bg-${name.replace(/[^a-z0-9]+/gi, '-').toLowerCase()}`.slice(0, 80)
  const s = ensureSession(sessionId)
  s.options = { cwd }
  s.active = true
  try {
    await fn({ sdk, sessionId, s })
    console.log(`ok - ${name}`)
  } catch (err) {
    failures += 1
    console.error(`FAIL - ${name}\n  ${err?.stack || err}`)
  } finally {
    await send('claude.abortSession', { sessionId })
    sessions.delete(sessionId)
    __setSdkOverrideForTests(undefined)
  }
}

async function startTurn(sessionId, prompt = 'go') {
  const reply = await send('claude.sendMessage', { sessionId, prompt, suppressUserEcho: true })
  assert.equal(reply.result?.accepted, true, `send not accepted: ${JSON.stringify(reply)}`)
}

function completedTurnEnds(sessionId) {
  return eventsFor(sessionId, 'claude:turn-end').filter(e => e.params.payload?.reason === 'completed')
}

function settled(sessionId) {
  return eventsFor(sessionId, 'claude:background-settled')
}

function latestList(sessionId) {
  const lists = eventsFor(sessionId, 'claude:background-tasks')
  return lists.length > 0 ? lists[lists.length - 1].params.tasks : null
}

// Replays the host's decision for each completed turn-end / settle event so a
// test can assert how many notifications one unit of work produces.
function hostNotifications(sessionId) {
  let awaiting = false
  let lastWork = null
  const notified = []
  const notifyOnce = (payload) => {
    if (typeof payload.workId === 'number') {
      if (payload.workId === lastWork) return
      lastWork = payload.workId
    }
    notified.push(payload.result ?? null)
  }
  for (const e of events) {
    if (e.params?.sessionId !== sessionId) continue
    if (e.name === 'claude:turn-end') {
      const p = e.params.payload
      if (p.reason === 'completed') {
        if (p.awaitingIdle === true || p.pendingBackground > 0) { awaiting = true; continue }
        awaiting = false
        notifyOnce(p)
      } else if (p.reason === 'aborted' || p.reason === 'error') {
        awaiting = false
      }
    } else if (e.name === 'claude:background-settled' && awaiting) {
      awaiting = false
      notifyOnce(e.params)
    }
  }
  return notified
}

try {
  await check('the CLI is asked to emit session_state_changed', async ({ sdk, sessionId }) => {
    await startTurn(sessionId)
    assert.equal(sdk.state.options?.env?.CLAUDE_CODE_EMIT_SESSION_STATE_EVENTS, '1')
    sdk.feed(result('ok'))
    await waitFor(() => completedTurnEnds(sessionId).length === 1, 'turn-end')
  })

  await check('a new tracker publishes an empty list before the CLI starts', async ({ sdk, sessionId }) => {
    await startTurn(sessionId)
    const lists = eventsFor(sessionId, 'claude:background-tasks')
    assert.equal(lists.length, 1)
    assert.deepEqual(lists[0].params.tasks, [])
    sdk.feed(assistantText('hello'), result('hello'))
    await waitFor(() => completedTurnEnds(sessionId).length === 1, 'turn-end')
    const payload = completedTurnEnds(sessionId)[0].params.payload
    assert.equal(payload.pendingBackground, 0)
    assert.equal(Number.isSafeInteger(payload.workId), true)
    assert.equal('awaitingIdle' in payload, false)
    await sleep(GRACE_MS * 3)
    assert.equal(eventsFor(sessionId, 'claude:background-tasks').length, 1)
    assert.equal(settled(sessionId).length, 0)
  })

  await check('legacy CLI: a background agent defers completion to the turn the SDK opens afterwards', async ({ sdk, sessionId }) => {
    await startTurn(sessionId)
    sdk.feed(
      {
        type: 'assistant',
        message: { content: [{ type: 'tool_use', id: 'tu-1', name: 'Agent', input: { description: 'research', run_in_background: true } }] },
      },
      level([agentTask('t-1')]),
      { type: 'system', subtype: 'task_started', task_id: 't-1', tool_use_id: 'tu-1', task_type: 'local_agent', description: 'research', subagent_type: 'general-purpose', is_backgrounded: true },
      { type: 'user', message: { content: [{ type: 'tool_result', tool_use_id: 'tu-1', content: 'launched' }] } },
      assistantText('working in the background'),
      result('main turn done'),
    )
    await waitFor(() => completedTurnEnds(sessionId).length === 1, 'first turn-end')
    assert.equal(completedTurnEnds(sessionId)[0].params.payload.pendingBackground, 1)
    const latest = latestList(sessionId)
    assert.equal(latest.length, 1)
    assert.equal(latest[0].toolUseId, 'tu-1', 'task_started must bind the level entry to its tool row')
    assert.equal(latest[0].isAgent, true)
    const task = eventsFor(sessionId, 'claude:task').find(e => e.params.task.id === 't-1')
    assert.equal(task.params.task.isBackground, true)

    // The subagent's own output is background activity, not a main turn.
    sdk.feed(assistantText('subagent step', 'tu-1'))
    await sleep(5)
    sdk.feed(
      { type: 'system', subtype: 'task_notification', task_id: 't-1', tool_use_id: 'tu-1', status: 'completed', output_file: '', summary: 'done' },
      level([]),
      { type: 'system', subtype: 'status', status: 'requesting' },
    )
    // The self-turn's first token arrives well after the grace window.
    await sleep(GRACE_MS * 3)
    assert.equal(settled(sessionId).length, 0, "'requesting' must hold the announcement for the self-turn")
    sdk.feed(assistantText('the agent finished'), result('all done'))
    await waitFor(() => completedTurnEnds(sessionId).length === 2, 'second turn-end')
    assert.equal(completedTurnEnds(sessionId)[1].params.payload.pendingBackground, 0)
    await sleep(GRACE_MS * 3)
    assert.equal(settled(sessionId).length, 0, 'the follow-up turn-end already carries the completion')
    assert.deepEqual(hostNotifications(sessionId), ['all done'])
    const ended = eventsFor(sessionId, 'claude:task').filter(e => e.params.task.id === 't-1')
    assert.equal(ended[ended.length - 1].params.task.status, 'completed', 'task_notification must end a background task')
  })

  await check('legacy CLI: agent work that ends without a new turn settles once after the grace window', async ({ sdk, sessionId }) => {
    await startTurn(sessionId)
    sdk.feed(level([agentTask('a-1')]), result('started the agent'))
    await waitFor(() => completedTurnEnds(sessionId).length === 1, 'turn-end')
    const held = completedTurnEnds(sessionId)[0].params.payload
    assert.equal(held.pendingBackground, 1)
    sdk.feed(level([]))
    await waitFor(() => settled(sessionId).length === 1, 'background-settled')
    assert.equal(settled(sessionId)[0].params.result, 'started the agent')
    assert.equal(settled(sessionId)[0].params.workId, held.workId)
    sdk.feed(level([]))
    await sleep(GRACE_MS * 3)
    assert.equal(settled(sessionId).length, 1, 'settled must fire exactly once')

    // A late self-turn reports the same work: the host must not notify twice.
    sdk.feed(assistantText('the agent finished'), result('late report'))
    await waitFor(() => completedTurnEnds(sessionId).length === 2, 'late self-turn')
    assert.equal(completedTurnEnds(sessionId)[1].params.payload.workId, held.workId)
    assert.deepEqual(hostNotifications(sessionId), ['started the agent'])

    // The next user turn is new work.
    await startTurn(sessionId, 'again')
    sdk.feed(assistantText('two'), result('two'))
    await waitFor(() => completedTurnEnds(sessionId).length === 3, 'next user turn')
    assert.deepEqual(hostNotifications(sessionId), ['started the agent', 'two'])
  })

  await check('a background dev server never holds the notification', async ({ sdk, sessionId }) => {
    await startTurn(sessionId)
    sdk.feed(
      {
        type: 'assistant',
        message: { content: [{ type: 'tool_use', id: 'tu-dev', name: 'Bash', input: { command: 'npm run dev', run_in_background: true } }] },
      },
      level([bashTask('b-1')]),
      { type: 'user', message: { content: [{ type: 'tool_result', tool_use_id: 'tu-dev', content: 'Command running in background' }] } },
      result('server started'),
    )
    await waitFor(() => completedTurnEnds(sessionId).length === 1, 'turn 1')
    assert.equal(completedTurnEnds(sessionId)[0].params.payload.pendingBackground, 0)
    const list = latestList(sessionId)
    assert.deepEqual(list.map(t => [t.id, t.isAgent]), [['b-1', false]], 'the shell is still listed')

    await startTurn(sessionId, 'next')
    sdk.feed(assistantText('edited'), result('edited a file'))
    await waitFor(() => completedTurnEnds(sessionId).length === 2, 'turn 2')
    assert.equal(completedTurnEnds(sessionId)[1].params.payload.pendingBackground, 0)
    assert.deepEqual(hostNotifications(sessionId), ['server started', 'edited a file'])
    await sleep(GRACE_MS * 3)
    assert.equal(settled(sessionId).length, 0)
  })

  await check('ambient and non-agent tasks do not count', async ({ sdk, sessionId }) => {
    await startTurn(sessionId)
    sdk.feed(
      level([
        { task_id: 'w-1', task_type: 'monitor_ws', description: 'watcher', ambient: true },
        { task_id: 'm-1', task_type: 'monitor_mcp', description: 'tail logs' },
        { task_id: 'f-1', task_type: 'local_workflow', description: 'spec' },
      ]),
      result('ok'),
    )
    await waitFor(() => completedTurnEnds(sessionId).length === 1, 'turn-end')
    assert.equal(completedTurnEnds(sessionId)[0].params.payload.pendingBackground, 1, 'only the workflow counts')
    assert.deepEqual(latestList(sessionId).map(t => t.id), ['m-1', 'f-1'])
    const state = await send('claude.getSessionState', { sessionId })
    assert.deepEqual(state.result.backgroundTasks.map(t => [t.id, t.isAgent]), [['m-1', false], ['f-1', true]])
  })

  await check('session_state_changed: a turn without background agents notifies on its turn-end', async ({ sdk, sessionId }) => {
    await startTurn(sessionId)
    sdk.feed(sessionState('running'), assistantText('hi'), level([bashTask('b-1')]), result('hi'))
    await waitFor(() => completedTurnEnds(sessionId).length === 1, 'turn-end')
    const payload = completedTurnEnds(sessionId)[0].params.payload
    assert.equal(payload.pendingBackground, 0)
    assert.equal('awaitingIdle' in payload, false)
    assert.deepEqual(hostNotifications(sessionId), ['hi'])
    sdk.feed(sessionState('idle'))
    await sleep(WATCHDOG_MS * 2)
    assert.equal(settled(sessionId).length, 0)
    assert.deepEqual(hostNotifications(sessionId), ['hi'])
  })

  await check('session_state_changed: a self-turn after the agents end waits for idle', async ({ sdk, sessionId }) => {
    await startTurn(sessionId)
    sdk.feed(sessionState('running'), level([agentTask('a-1')]), result('main done'))
    await waitFor(() => completedTurnEnds(sessionId).length === 1, 'turn-end')
    sdk.feed(level([]), assistantText('agent reported'), result('agent result'))
    await waitFor(() => completedTurnEnds(sessionId).length === 2, 'self-turn')
    const held = completedTurnEnds(sessionId)[1].params.payload
    assert.equal(held.pendingBackground, 0)
    assert.equal(held.awaitingIdle, true)
    assert.deepEqual(hostNotifications(sessionId), [])
    sdk.feed(sessionState('idle'))
    await waitFor(() => settled(sessionId).length === 1, 'settled on idle')
    assert.equal(settled(sessionId)[0].params.workId, held.workId)
    sdk.feed(sessionState('idle'))
    await sleep(WATCHDOG_MS * 2)
    assert.equal(settled(sessionId).length, 1)
    assert.deepEqual(hostNotifications(sessionId), ['agent result'])
  })

  await check('session_state_changed: a background agent notifies once after the self-turn goes idle', async ({ sdk, sessionId }) => {
    await startTurn(sessionId)
    sdk.feed(sessionState('running'), level([agentTask('a-1')]), result('main done'), sessionState('idle'))
    await waitFor(() => completedTurnEnds(sessionId).length === 1, 'turn-end')
    assert.equal(completedTurnEnds(sessionId)[0].params.payload.pendingBackground, 1)
    await sleep(WATCHDOG_MS * 2)
    assert.equal(settled(sessionId).length, 0, 'idle with a live agent must not announce')

    sdk.feed(level([]))
    await sleep(GRACE_MS / 4)
    sdk.feed(sessionState('running'))
    await sleep(GRACE_MS * 3)
    assert.equal(settled(sessionId).length, 0, "'running' cancels the idle grace")
    sdk.feed(assistantText('agent reported'), result('agent result'))
    await waitFor(() => completedTurnEnds(sessionId).length === 2, 'self-turn')
    assert.equal(completedTurnEnds(sessionId)[1].params.payload.awaitingIdle, true)
    sdk.feed(sessionState('idle'))
    await waitFor(() => settled(sessionId).length === 1, 'settled')
    assert.equal(settled(sessionId)[0].params.result, 'agent result')
    assert.deepEqual(hostNotifications(sessionId), ['agent result'])
  })

  await check('session_state_changed: an agent list emptying while idle settles after the grace', async ({ sdk, sessionId }) => {
    await startTurn(sessionId)
    sdk.feed(sessionState('running'), level([agentTask('a-1')]), result('main done'), sessionState('idle'))
    await waitFor(() => completedTurnEnds(sessionId).length === 1, 'turn-end')
    sdk.feed(level([]))
    await waitFor(() => settled(sessionId).length === 1, 'settled')
    assert.deepEqual(hostNotifications(sessionId), ['main done'])
  })

  await check('session_state_changed: idle before the result notifies on the turn-end', async ({ sdk, sessionId }) => {
    await startTurn(sessionId)
    sdk.feed(sessionState('running'), assistantText('x'), sessionState('idle'), result('x'))
    await waitFor(() => completedTurnEnds(sessionId).length === 1, 'turn-end')
    assert.equal('awaitingIdle' in completedTurnEnds(sessionId)[0].params.payload, false)
    await sleep(WATCHDOG_MS * 2)
    assert.equal(settled(sessionId).length, 0)
    assert.deepEqual(hostNotifications(sessionId), ['x'])
  })

  await check('session_state_changed: a missing idle is covered by the watchdog', async ({ sdk, sessionId }) => {
    await startTurn(sessionId)
    sdk.feed(sessionState('running'), level([agentTask('a-1')]), result('main done'))
    await waitFor(() => completedTurnEnds(sessionId).length === 1, 'turn-end')
    sdk.feed(level([]), assistantText('agent reported'), result('stuck'))
    await waitFor(() => settled(sessionId).length === 1, 'watchdog settle')
    assert.deepEqual(hostNotifications(sessionId), ['stuck'])
    sdk.feed(sessionState('idle'))
    await sleep(GRACE_MS)
    assert.equal(settled(sessionId).length, 1)
  })

  await check('edge events drive the list when the CLI sends no level frames', async ({ sdk, sessionId }) => {
    await startTurn(sessionId)
    sdk.feed(
      { type: 'system', subtype: 'task_started', task_id: 'e-1', tool_use_id: 'tu-e', task_type: 'local_agent', description: 'agent', is_backgrounded: true },
      { type: 'system', subtype: 'task_started', task_id: 'f-1', tool_use_id: 'tu-f', task_type: 'local_bash', description: 'ls', is_backgrounded: false },
      result('kicked off'),
    )
    await waitFor(() => completedTurnEnds(sessionId).length === 1, 'turn-end')
    assert.equal(completedTurnEnds(sessionId)[0].params.payload.pendingBackground, 1)

    const stop = await send('claude.stopTask', { sessionId, taskId: 'tu-e' })
    assert.equal(stop.result?.ok, true, `stopTask after the turn: ${JSON.stringify(stop.result)}`)
    assert.deepEqual(sdk.state.stopped, ['e-1'], 'tool_use id must map to the task id')

    sdk.feed({ type: 'system', subtype: 'task_notification', task_id: 'e-1', status: 'stopped', output_file: '', summary: '' })
    await waitFor(() => settled(sessionId).length === 1, 'background-settled')
    assert.deepEqual(latestList(sessionId), [])
    const ended = eventsFor(sessionId, 'claude:task').filter(e => e.params.task.id === 'e-1')
    assert.equal(ended[ended.length - 1].params.task.status, 'killed')
  })

  await check('a launch-only entry is listed but never holds the notification', async ({ sdk, sessionId }) => {
    await startTurn(sessionId)
    sdk.feed(
      {
        type: 'assistant',
        message: { content: [{ type: 'tool_use', id: 'tu-l', name: 'Agent', input: { description: 'research', run_in_background: true } }] },
      },
      { type: 'user', message: { content: [{ type: 'tool_result', tool_use_id: 'tu-l', content: 'launched' }] } },
      result('launched'),
    )
    await waitFor(() => completedTurnEnds(sessionId).length === 1, 'turn-end')
    assert.equal(completedTurnEnds(sessionId)[0].params.payload.pendingBackground, 0)
    assert.deepEqual(latestList(sessionId).map(t => [t.id, t.isAgent]), [['tu-l', false]])
    sdk.feed({ type: 'system', subtype: 'task_notification', task_id: 'x', tool_use_id: 'tu-l', status: 'completed', output_file: '', summary: '' })
    await waitFor(() => latestList(sessionId).length === 0, 'launch entry removed')
  })

  for (const order of ['task event first', 'launch first']) {
    await check(`a task bound to a background launch ends by task id alone (${order})`, async ({ sdk, sessionId }) => {
      await startTurn(sessionId)
      const started = { type: 'system', subtype: 'task_started', task_id: 'k-9', tool_use_id: 'tu-r', task_type: 'local_agent', description: 'research' }
      const launched = { type: 'user', message: { content: [{ type: 'tool_result', tool_use_id: 'tu-r', content: 'launched' }] } }
      sdk.feed(
        {
          type: 'assistant',
          message: { content: [{ type: 'tool_use', id: 'tu-r', name: 'Agent', input: { description: 'research', run_in_background: true } }] },
        },
        ...(order === 'task event first' ? [started, launched] : [launched, started]),
        result('kicked off'),
      )
      await waitFor(() => completedTurnEnds(sessionId).length === 1, 'turn-end')
      assert.equal(completedTurnEnds(sessionId)[0].params.payload.pendingBackground, 1)
      assert.deepEqual(latestList(sessionId).map(t => [t.id, t.toolUseId, t.isAgent]), [['k-9', 'tu-r', true]])
      sdk.feed({ type: 'system', subtype: 'task_notification', task_id: 'k-9', status: 'completed', output_file: '', summary: '' })
      await waitFor(() => settled(sessionId).length === 1, 'settled')
      assert.deepEqual(latestList(sessionId), [])
      const ended = eventsFor(sessionId, 'claude:task').filter(e => e.params.task.id === 'k-9')
      assert.equal(ended[ended.length - 1].params.task.status, 'completed')
    })
  }

  await check('a terminal task_updated ends a background entry', async ({ sdk, sessionId }) => {
    await startTurn(sessionId)
    sdk.feed(
      { type: 'system', subtype: 'task_started', task_id: 'u-1', tool_use_id: 'tu-u', task_type: 'local_agent', description: 'job', is_backgrounded: true },
      result('started'),
    )
    await waitFor(() => completedTurnEnds(sessionId).length === 1, 'turn-end')
    sdk.feed({ type: 'system', subtype: 'task_updated', task_id: 'u-1', patch: { status: 'failed', error: 'boom' } })
    await waitFor(() => settled(sessionId).length === 1, 'settled')
    assert.deepEqual(latestList(sessionId), [])
  })

  await check('the turn-end reconciles fallback entries with the task mirror', async ({ sdk, sessionId, s }) => {
    await startTurn(sessionId)
    sdk.feed(
      { type: 'system', subtype: 'task_started', task_id: 'g-1', tool_use_id: 'tu-g', task_type: 'local_agent', description: 'ghost', is_backgrounded: true },
      result('one'),
    )
    await waitFor(() => completedTurnEnds(sessionId).length === 1, 'turn 1')
    assert.equal(completedTurnEnds(sessionId)[0].params.payload.pendingBackground, 1)
    s.activeTasks.delete('g-1')
    await startTurn(sessionId, 'next')
    sdk.feed(result('two'))
    await waitFor(() => completedTurnEnds(sessionId).length === 2, 'turn 2')
    assert.equal(completedTurnEnds(sessionId)[1].params.payload.pendingBackground, 0)
    assert.deepEqual(latestList(sessionId), [])
    assert.deepEqual(hostNotifications(sessionId), ['two'])
  })

  await check('foreground task notifications keep the turn-end cleanup', async ({ sdk, sessionId }) => {
    await startTurn(sessionId)
    sdk.feed(
      { type: 'system', subtype: 'task_started', task_id: 'fg', tool_use_id: 'tu-fg', task_type: 'local_bash', description: 'ls', is_backgrounded: false },
      { type: 'system', subtype: 'task_notification', task_id: 'fg', tool_use_id: 'tu-fg', status: 'completed', output_file: '', summary: '' },
      result('done'),
    )
    await waitFor(() => completedTurnEnds(sessionId).length === 1, 'turn-end')
    assert.equal(eventsFor(sessionId, 'claude:task').filter(e => e.params.task.id === 'fg').length, 1)
  })

  await check('a CLI that exits while a completion is held announces it once', async ({ sdk, sessionId }) => {
    await startTurn(sessionId)
    sdk.feed(level([agentTask('a-1')]), result('waiting'))
    await waitFor(() => completedTurnEnds(sessionId).length === 1, 'turn-end')
    sdk.end()
    await waitFor(() => settled(sessionId).length === 1, 'settled on exit')
    assert.deepEqual(latestList(sessionId), [])
    assert.deepEqual(hostNotifications(sessionId), ['waiting'])
  })

  await check('closing the CLI for a config change announces a held completion', async ({ sdk, sessionId }) => {
    await startTurn(sessionId)
    sdk.feed(level([agentTask('a-1')]), result('waiting'))
    await waitFor(() => completedTurnEnds(sessionId).length === 1, 'turn-end')
    // The scripted SDK has no setPermissionMode control, so the sidecar closes the CLI.
    await send('claude.setPermissionMode', { sessionId, mode: 'plan' })
    assert.equal(settled(sessionId).length, 1)
    await sleep(GRACE_MS * 2)
    assert.equal(settled(sessionId).length, 1)
    assert.deepEqual(hostNotifications(sessionId), ['waiting'])
  })

  for (const method of ['claude.restSession', 'claude.reloadSession']) {
    await check(`${method} drops a completion held for live agents`, async ({ sdk, sessionId }) => {
      await startTurn(sessionId)
      sdk.feed(level([agentTask('a-1')]), result('waiting'))
      await waitFor(() => completedTurnEnds(sessionId).length === 1, 'turn-end')
      await send(method, { sessionId })
      assert.deepEqual(latestList(sessionId), [])
      await sleep(GRACE_MS * 3)
      assert.equal(settled(sessionId).length, 0)
      assert.deepEqual(hostNotifications(sessionId), [])
    })
  }

  await check('work ids stay unique across a session reset', async ({ sdk, sessionId }) => {
    await startTurn(sessionId)
    sdk.feed(assistantText('one'), result('one'))
    await waitFor(() => completedTurnEnds(sessionId).length === 1, 'turn 1')
    assert.equal((await send('claude.resetSession', { sessionId })).result, true)
    const started = await send('claude.startSession', { sessionId, options: { cwd } })
    assert.equal(started.error, undefined, JSON.stringify(started.error))
    await startTurn(sessionId, 'again')
    sdk.feed(assistantText('two'), result('two'))
    await waitFor(() => completedTurnEnds(sessionId).length === 2, 'turn 2')
    const [first, second] = completedTurnEnds(sessionId).map(e => e.params.payload.workId)
    assert.notEqual(first, second)
    assert.deepEqual(hostNotifications(sessionId), ['one', 'two'])
  })

  for (const outcome of ['completed', 'error']) {
    await check(`a queued turn announces the completion held for idle first (${outcome})`, async ({ sdk, sessionId }) => {
      await startTurn(sessionId, 'zero')
      sdk.feed(sessionState('running'), level([agentTask('a-1')]), result('zero'), sessionState('idle'))
      await waitFor(() => completedTurnEnds(sessionId).length === 1, 'turn 0')
      await startTurn(sessionId, 'A')
      const queued = send('claude.sendMessage', { sessionId, prompt: 'B', suppressUserEcho: true })
      sdk.feed(sessionState('running'), level([]), assistantText('a'), result('A'))
      await waitFor(() => sdk.state.pushed.length === 3, 'B pushed')
      assert.equal((await queued).result?.accepted, true)
      assert.equal(completedTurnEnds(sessionId)[1].params.payload.awaitingIdle, true)
      assert.equal(settled(sessionId).length, 1, 'B must not swallow the held completion')
      assert.equal(settled(sessionId)[0].params.result, 'A')
      sdk.feed(outcome === 'completed'
        ? result('B')
        : { type: 'result', subtype: 'error_during_execution', session_id: 'sdk-session', is_error: true, errors: ['boom'] })
      await waitFor(() => eventsFor(sessionId, 'claude:turn-end').length === 3, 'turn B')
      assert.deepEqual(hostNotifications(sessionId), outcome === 'completed' ? ['A', 'B'] : ['A'])
    })
  }

  await check('a hard stop clears the list without announcing completion', async ({ sdk, sessionId }) => {
    await startTurn(sessionId)
    sdk.feed(level([agentTask('k-1', 'long job')]), result('waiting'))
    await waitFor(() => completedTurnEnds(sessionId).length === 1, 'turn-end')
    await send('claude.abortSession', { sessionId })
    assert.deepEqual(latestList(sessionId), [])
    await sleep(GRACE_MS * 3)
    assert.equal(settled(sessionId).length, 0)
    assert.deepEqual(hostNotifications(sessionId), [])
  })

  // Tracker-level checks for paths the scripted SDK cannot reach.
  const unit = (name, fn) => {
    const s = {}
    try {
      fn(s, `unit-${name}`)
      console.log(`ok - ${name}`)
    } catch (err) {
      failures += 1
      console.error(`FAIL - ${name}\n  ${err?.stack || err}`)
    } finally {
      bg.resetBackgroundTasks(s, null)
    }
  }

  unit('a turn that throws without a result frame clears the main-turn flag', (s, sessionId) => {
    bg.noteUserTurn(s, sessionId)
    bg.applyBackgroundLevel(s, sessionId, level([agentTask('a-1')]))
    bg.noteTurnResult(s, sessionId, { outcome: 'completed', result: 'held' })
    bg.noteUserTurn(s, sessionId)
    assert.equal(s.backgroundTracker.mainTurnActive, true)
    bg.noteTurnEndedWithoutResult(s, sessionId, 'interrupted')
    assert.equal(s.backgroundTracker.mainTurnActive, false)
    assert.equal(s.backgroundTracker.awaitingSettle, true, 'an interrupt keeps the held completion')
    bg.noteTurnEndedWithoutResult(s, sessionId, 'error')
    assert.equal(s.backgroundTracker.awaitingSettle, false, 'an error ends the wait, as on the host')
  })

  await (async () => {
    const name = 'an interrupted turn lets a held completion settle'
    const s = {}
    const sessionId = 'unit-interrupt-settle'
    try {
      bg.noteUserTurn(s, sessionId)
      bg.applyBackgroundLevel(s, sessionId, level([agentTask('a-1')]))
      bg.noteTurnResult(s, sessionId, { outcome: 'completed', result: 'held' })
      bg.noteUserTurn(s, sessionId)
      bg.applyBackgroundLevel(s, sessionId, level([]))
      await sleep(GRACE_MS * 3)
      assert.equal(settled(sessionId).length, 0, 'a running main turn carries the completion')
      bg.noteTurnEndedWithoutResult(s, sessionId, 'interrupted')
      await waitFor(() => settled(sessionId).length === 1, 'settled after interrupt')
      console.log(`ok - ${name}`)
    } catch (err) {
      failures += 1
      console.error(`FAIL - ${name}\n  ${err?.stack || err}`)
    } finally {
      bg.resetBackgroundTasks(s, null)
    }
  })()
} finally {
  restoreSendEvent()
  bg.__setBackgroundTimingsForTests(undefined)
  rmSync(cwd, { recursive: true, force: true })
}

if (failures > 0) {
  console.error(`\n${failures} test(s) failed`)
  process.exit(1)
}
console.log('\nclaude-background-tasks: all tests passed')
process.exit(0)
