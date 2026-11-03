// Tests for renderer/src/utils/codex-skills.ts: the Codex `/` menu skills group,
// the inserted mention text and the per-session cache. Deterministic + offline.

import * as assert from 'node:assert/strict'
import { readFile } from 'node:fs/promises'

import {
  buildCodexSlashMenu,
  codexSkillLabel,
  codexSkillMentionText,
  codexSkillSource,
  codexSkillSummary,
  codexSkillsCacheKey,
  CodexSkillsCache,
  filterCodexSkills,
  flattenCodexSlashMenu,
  normalizeCodexSkillsResult,
  type CodexSkillInfo,
} from '../renderer/src/utils/codex-skills'
import type { SlashCommandInfo } from '../renderer/src/utils/slash-commands'

let failures = 0
async function test(name: string, fn: () => void | Promise<void>) {
  try {
    await fn()
    console.log('  ok  -', name)
  } catch (err) {
    failures++
    console.error('  FAIL-', name, '\n   ', (err as Error).message)
  }
}

const cmd = (name: string, description = ''): SlashCommandInfo => ({ name, description, argumentHint: '' })
const skill = (name: string, extra: Partial<CodexSkillInfo> = {}): CodexSkillInfo => ({
  name,
  path: `/home/.codex/skills/${name}/SKILL.md`,
  description: `${name} description`,
  enabled: true,
  ...extra,
})

const COMMANDS = [cmd('new', 'Reset session'), cmd('model', 'Select model'), cmd('review-me', 'BAT command')]

async function main() {
  await test('normalize keeps well-formed skills and drops malformed rows', () => {
    const result = normalizeCodexSkillsResult({
      ready: true,
      skills: [
        { name: 'review', path: '/s/review/SKILL.md', description: 'Review', enabled: true, displayName: ' Code Review ', shortDescription: '', pluginId: null },
        { name: '', path: '/s/blank/SKILL.md', enabled: true },
        { name: 'nopath', enabled: true },
        'garbage',
        { name: 'off', path: '/s/off/SKILL.md', enabled: 'yes' },
      ],
      errors: [{ path: '/bad', message: 'x' }],
    })
    assert.equal(result.ready, true)
    assert.deepEqual(result.skills, [
      { name: 'review', path: '/s/review/SKILL.md', description: 'Review', enabled: true, displayName: 'Code Review' },
      { name: 'off', path: '/s/off/SKILL.md', description: '', enabled: false },
    ])
    assert.deepEqual(normalizeCodexSkillsResult(null), { ready: false, skills: [] })
    assert.deepEqual(normalizeCodexSkillsResult({ ready: false, skills: [] }), { ready: false, skills: [] })
  })

  await test('labels prefer interface fields and the mention uses the skill name', () => {
    const rich = skill('review', { displayName: 'Code Review', shortDescription: 'Review the diff' })
    assert.equal(codexSkillLabel(rich), 'Code Review')
    assert.equal(codexSkillSummary(rich), 'Review the diff')
    assert.equal(codexSkillLabel(skill('plain')), 'plain')
    assert.equal(codexSkillSummary(skill('plain')), 'plain description')
    assert.equal(codexSkillMentionText(rich), '$review ')
  })

  await test('same-name skills insert a linked mention and show where each comes from', () => {
    const user = skill('review', { scope: 'user' })
    const repo = skill('review', { scope: 'repo', path: '/work/app/.codex/skills/review/SKILL.md' })
    const off = skill('deploy', { enabled: false, scope: 'repo', path: '/work/app/.codex/skills/deploy/SKILL.md' })
    const deploy = skill('deploy', { scope: 'user' })
    const all = [user, repo, off, deploy]
    assert.equal(codexSkillMentionText(repo, all), '[$review](/work/app/.codex/skills/review/SKILL.md) ')
    assert.equal(codexSkillMentionText(user, all), '[$review](/home/.codex/skills/review/SKILL.md) ')
    assert.equal(codexSkillMentionText(deploy, all), '$deploy ', 'a disabled twin does not make the name ambiguous')
    const paren = skill('review', { path: 'C:\\Program Files (x86)\\skills\\review\\SKILL.md' })
    assert.equal(codexSkillMentionText(paren, [user, paren]), '$review ', 'a link cannot carry a path with `)`')
    assert.equal(codexSkillSource(repo, all), 'repo')
    assert.equal(codexSkillSource(user, all), 'user')
    assert.equal(codexSkillSource(deploy, all), undefined)

    const pluginA = skill('lint', { scope: 'user', pluginId: 'a', path: '/p/a/skills/lint/SKILL.md' })
    const pluginB = skill('lint', { scope: 'user', pluginId: 'b', path: '/p/b/skills/lint/SKILL.md' })
    assert.equal(codexSkillSource(pluginA, [pluginA, pluginB]), 'user · a')
    const homeCopy = skill('fmt', { scope: 'user', path: '/home/x/.codex/skills/fmt/SKILL.md' })
    const otherCopy = skill('fmt', { scope: 'user', path: 'C:\\Users\\y\\.codex\\skills\\fmt\\SKILL.md' })
    assert.equal(codexSkillSource(homeCopy, [homeCopy, otherCopy]), '…/x/.codex/skills/fmt')
    assert.equal(codexSkillSource(otherCopy, [homeCopy, otherCopy]), '…/y/.codex/skills/fmt')

    const rows = flattenCodexSlashMenu(buildCodexSlashMenu({ commands: [], query: 'review', skills: all, skillsLoading: false }))
    assert.deepEqual(
      rows.map(row => (row.kind === 'skill' ? [row.mentionText, row.source] : [])),
      [
        ['[$review](/home/.codex/skills/review/SKILL.md) ', 'user'],
        ['[$review](/work/app/.codex/skills/review/SKILL.md) ', 'repo'],
      ],
    )
  })

  await test('filter matches name or display name and hides disabled skills', () => {
    const skills = [
      skill('review', { displayName: 'Code Review' }),
      skill('deploy'),
      skill('reviewer-off', { enabled: false }),
    ]
    assert.deepEqual(filterCodexSkills(skills, '').map(s => s.name), ['review', 'deploy'])
    assert.deepEqual(filterCodexSkills(skills, 'REV').map(s => s.name), ['review'])
    assert.deepEqual(filterCodexSkills(skills, 'code').map(s => s.name), ['review'])
    assert.deepEqual(filterCodexSkills(skills, 'description').map(s => s.name), [])
  })

  await test('menu keeps the existing command filter and appends a skills group', () => {
    const groups = buildCodexSlashMenu({
      commands: COMMANDS,
      query: 'rev',
      skills: [skill('review'), skill('deploy')],
      skillsLoading: false,
    })
    assert.deepEqual(groups.map(g => g.id), ['commands', 'skills'])
    const rows = flattenCodexSlashMenu(groups)
    assert.deepEqual(rows.map(r => r.key), ['command:review-me', 'skill:/home/.codex/skills/review/SKILL.md'])
    const unfiltered = buildCodexSlashMenu({ commands: COMMANDS, query: '', skills: [], skillsLoading: false })
    assert.deepEqual(unfiltered.map(g => g.id), ['commands'])
    assert.deepEqual(flattenCodexSlashMenu(unfiltered).map(r => r.key), ['command:new', 'command:model', 'command:review-me'])
  })

  await test('loading keeps an empty skills group visible but adds no selectable row', () => {
    const groups = buildCodexSlashMenu({ commands: COMMANDS, query: 'zzz', skills: [], skillsLoading: true })
    assert.deepEqual(groups.map(g => [g.id, g.loading, g.rows.length]), [['skills', true, 0]])
    assert.equal(flattenCodexSlashMenu(groups).length, 0)
  })

  await test('a skill and a command with the same name stay separate rows', () => {
    const rows = flattenCodexSlashMenu(buildCodexSlashMenu({
      commands: [cmd('new')],
      query: 'new',
      skills: [skill('new')],
      skillsLoading: false,
    }))
    assert.deepEqual(rows.map(r => r.kind), ['command', 'skill'])
    assert.equal(new Set(rows.map(r => r.key)).size, 2)
  })

  await test('cache stores ready results once and dedupes in-flight loads', async () => {
    const cache = new CodexSkillsCache()
    const key = codexSkillsCacheKey('s-1', '/repo')
    let calls = 0
    const fetcher = async () => {
      calls++
      return { ready: true, skills: [skill('review')] }
    }
    const [a, b] = await Promise.all([cache.load(key, fetcher), cache.load(key, fetcher)])
    assert.equal(calls, 1)
    assert.deepEqual(a, b)
    assert.equal(cache.isLoading(key), false)
    await cache.load(key, fetcher)
    assert.equal(calls, 1)
    assert.deepEqual(cache.get(key)?.map(s => s.name), ['review'])
  })

  await test('cache skips not-ready answers and failures', async () => {
    const cache = new CodexSkillsCache()
    const key = codexSkillsCacheKey('s-1', '/repo')
    assert.deepEqual(await cache.load(key, async () => ({ ready: false, skills: [] })), [])
    assert.equal(cache.get(key), undefined)
    await assert.rejects(cache.load(key, async () => { throw new Error('host down') }), /host down/)
    assert.equal(cache.get(key), undefined)
    assert.equal(cache.isLoading(key), false)
  })

  await test('invalidation is per session and discards an answer that was in flight', async () => {
    const cache = new CodexSkillsCache()
    const keyA = codexSkillsCacheKey('s-1', '/repo')
    const keyB = codexSkillsCacheKey('s-2', '/repo')
    await cache.load(keyB, async () => ({ ready: true, skills: [skill('other')] }))
    let release: (value: unknown) => void = () => {}
    const pending = cache.load(keyA, () => new Promise(resolve => { release = resolve }))
    await new Promise(resolve => setTimeout(resolve, 0))
    cache.invalidateSession('s-1')
    assert.equal(cache.isLoading(keyA), false)
    release({ ready: true, skills: [skill('stale')] })
    await pending
    assert.equal(cache.get(keyA), undefined)
    assert.deepEqual(cache.get(keyB)?.map(s => s.name), ['other'])
    cache.invalidateSession('s-2')
    assert.equal(cache.get(keyB), undefined)
  })

  await test('Codex panel wires the skills group, the host call and the change event', async () => {
    const source = await readFile('renderer/src/components/CodexAgentPanel.tsx', 'utf8')
    assert.ok(source.includes('host.codex.listSkills(sessionId, codexSkillsCwd)'))
    assert.ok(source.includes('host.codex.onSkillsChanged('))
    assert.ok(source.includes('setInputValue(row.mentionText)'))
    assert.ok(source.includes('{row.source &&'))
    assert.ok(!source.includes('filteredSlashCommands.slice(0, 10)'),
      'skills sit after the BAT commands, so the menu must scroll instead of truncating')
  })

  if (failures > 0) {
    console.error(`codex skills: ${failures} failed`)
    process.exit(1)
  }
  console.log('codex skills: passed')
}

main().catch(error => {
  console.error(error instanceof Error ? error.stack || error.message : String(error))
  process.exit(1)
})
