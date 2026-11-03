// Codex skills for the Codex panel's `/` menu; React-free for node:assert tests.
// Picking a skill inserts a Codex mention; on send the host resolves it to the
// structured `{ type: 'skill', name, path }` turn input.

import type { SlashCommandInfo } from './slash-commands'

export interface CodexSkillInfo {
  name: string
  path: string
  description: string
  enabled: boolean
  displayName?: string
  shortDescription?: string
  defaultPrompt?: string
  pluginId?: string
  scope?: string
}

export interface CodexSkillsListResult {
  /** False when the host has no live Codex session to ask; such results are never cached. */
  ready: boolean
  skills: CodexSkillInfo[]
}

const OPTIONAL_STRING_FIELDS = ['displayName', 'shortDescription', 'defaultPrompt', 'pluginId', 'scope'] as const

function nonEmptyString(value: unknown): string | undefined {
  if (typeof value !== 'string') return undefined
  const trimmed = value.trim()
  return trimmed ? trimmed : undefined
}

function normalizeSkill(value: unknown): CodexSkillInfo | null {
  if (!value || typeof value !== 'object') return null
  const raw = value as Record<string, unknown>
  const name = nonEmptyString(raw.name)
  const path = nonEmptyString(raw.path)
  if (!name || !path) return null
  const skill: CodexSkillInfo = {
    name,
    path,
    description: nonEmptyString(raw.description) ?? '',
    enabled: raw.enabled === true,
  }
  for (const field of OPTIONAL_STRING_FIELDS) {
    const text = nonEmptyString(raw[field])
    if (text) skill[field] = text
  }
  return skill
}

export function normalizeCodexSkillsResult(value: unknown): CodexSkillsListResult {
  if (!value || typeof value !== 'object') return { ready: false, skills: [] }
  const raw = value as Record<string, unknown>
  const skills = Array.isArray(raw.skills)
    ? raw.skills.map(normalizeSkill).filter((skill): skill is CodexSkillInfo => skill !== null)
    : []
  return { ready: raw.ready === true, skills }
}

export function codexSkillLabel(skill: CodexSkillInfo): string {
  return skill.displayName ?? skill.name
}

export function codexSkillSummary(skill: CodexSkillInfo): string {
  return skill.shortDescription ?? skill.description
}

function sameNameEnabledPeers(skill: CodexSkillInfo, skills: readonly CodexSkillInfo[]): CodexSkillInfo[] {
  return skills.filter(other => other.enabled && other.name === skill.name && other.path !== skill.path)
}

/**
 * A name other enabled skills share becomes Codex's linked mention `[$name](path)`,
 * which the host resolves by path; the host skips an ambiguous plain `$name`.
 * The link form ends at the first `)`, so such paths fall back to `$name`.
 */
export function codexSkillMentionText(skill: CodexSkillInfo, skills: readonly CodexSkillInfo[] = []): string {
  const linkable = !/[)\r\n]/.test(skill.path)
  if (linkable && sameNameEnabledPeers(skill, skills).length > 0) return `[$${skill.name}](${skill.path}) `
  return `$${skill.name} `
}

function skillOrigin(skill: CodexSkillInfo): string {
  return [skill.scope, skill.pluginId].filter(Boolean).join(' · ')
}

function skillDirSegments(path: string): string[] {
  const segments = path.split(/[\\/]+/).filter(Boolean)
  if (segments.length > 1 && segments[segments.length - 1].toLowerCase() === 'skill.md') segments.pop()
  return segments
}

function distinguishingPathTail(path: string, otherPaths: readonly string[]): string {
  const own = skillDirSegments(path)
  const others = otherPaths.map(skillDirSegments)
  for (let count = 1; count <= own.length; count++) {
    const tail = own.slice(-count).join('/')
    if (others.every(segments => segments.slice(-count).join('/') !== tail)) {
      return count < own.length ? `…/${tail}` : tail
    }
  }
  return path
}

/** Tells same-name enabled skills apart in the menu; undefined when the name is unique. */
export function codexSkillSource(skill: CodexSkillInfo, skills: readonly CodexSkillInfo[]): string | undefined {
  const peers = sameNameEnabledPeers(skill, skills)
  if (peers.length === 0) return undefined
  const origin = skillOrigin(skill)
  if (origin && peers.every(peer => skillOrigin(peer) !== origin)) return origin
  return distinguishingPathTail(skill.path, peers.map(peer => peer.path))
}

/** Disabled skills are left out: Codex will not load them for an explicit mention. */
export function filterCodexSkills(skills: readonly CodexSkillInfo[], query: string): CodexSkillInfo[] {
  const q = query.trim().toLowerCase()
  return skills.filter(skill =>
    skill.enabled
    && (!q
      || skill.name.toLowerCase().includes(q)
      || (skill.displayName?.toLowerCase().includes(q) ?? false)),
  )
}

export type CodexSlashMenuRow =
  | { kind: 'command'; key: string; command: SlashCommandInfo }
  | { kind: 'skill'; key: string; skill: CodexSkillInfo; mentionText: string; source?: string }

export interface CodexSlashMenuGroup {
  id: 'commands' | 'skills'
  rows: CodexSlashMenuRow[]
  loading: boolean
}

export function buildCodexSlashMenu(options: {
  commands: readonly SlashCommandInfo[]
  query: string
  skills: readonly CodexSkillInfo[]
  skillsLoading: boolean
}): CodexSlashMenuGroup[] {
  const q = options.query.toLowerCase()
  const commands = q
    ? options.commands.filter(command => command.name.toLowerCase().includes(q))
    : [...options.commands]
  const groups: CodexSlashMenuGroup[] = []
  if (commands.length > 0) {
    groups.push({
      id: 'commands',
      loading: false,
      rows: commands.map(command => ({ kind: 'command', key: `command:${command.name}`, command })),
    })
  }
  const skills = filterCodexSkills(options.skills, options.query)
  if (skills.length > 0 || options.skillsLoading) {
    groups.push({
      id: 'skills',
      loading: options.skillsLoading,
      rows: skills.map(skill => ({
        kind: 'skill',
        key: `skill:${skill.path}`,
        skill,
        mentionText: codexSkillMentionText(skill, options.skills),
        source: codexSkillSource(skill, options.skills),
      })),
    })
  }
  return groups
}

/** Arrow keys index this flat list; it walks rows in the same order the groups render. */
export function flattenCodexSlashMenu(groups: readonly CodexSlashMenuGroup[]): CodexSlashMenuRow[] {
  return groups.flatMap(group => group.rows)
}

export function codexSkillsCacheKey(sessionId: string, cwd: string): string {
  return `${sessionId}\u0000${cwd}`
}

/**
 * Per-session, per-cwd skill lists. Keyed by session as well as cwd because each
 * session may run under a different Codex account whose user skills differ.
 */
export class CodexSkillsCache {
  private readonly entries = new Map<string, CodexSkillInfo[]>()
  private readonly inflight = new Map<string, Promise<CodexSkillInfo[]>>()
  private readonly generations = new Map<string, number>()

  get(key: string): CodexSkillInfo[] | undefined {
    return this.entries.get(key)
  }

  isLoading(key: string): boolean {
    return this.inflight.has(key)
  }

  load(key: string, fetcher: () => Promise<unknown>): Promise<CodexSkillInfo[]> {
    const cached = this.entries.get(key)
    if (cached) return Promise.resolve(cached)
    const pending = this.inflight.get(key)
    if (pending) return pending
    const generation = this.generations.get(key) ?? 0
    const request = Promise.resolve()
      .then(fetcher)
      .then(value => {
        const result = normalizeCodexSkillsResult(value)
        // A skills/changed that arrived mid-request makes this answer stale.
        if (result.ready && (this.generations.get(key) ?? 0) === generation) {
          this.entries.set(key, result.skills)
        }
        return result.skills
      })
      .finally(() => {
        if (this.inflight.get(key) === request) this.inflight.delete(key)
      })
    this.inflight.set(key, request)
    return request
  }

  invalidateSession(sessionId: string): void {
    const prefix = `${sessionId}\u0000`
    const keys = new Set([...this.entries.keys(), ...this.inflight.keys()])
    for (const key of keys) {
      if (!key.startsWith(prefix)) continue
      this.entries.delete(key)
      this.inflight.delete(key)
      this.generations.set(key, (this.generations.get(key) ?? 0) + 1)
    }
  }
}

export const codexSkillsCache = new CodexSkillsCache()
