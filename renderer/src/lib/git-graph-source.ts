import type { GitGraphCommit, GitGraphScope } from './git-graph'

export type GraphLogFetcher = (cwd: string, count: number, scope: GitGraphScope) => Promise<GitGraphCommit[] | null>

export interface GraphLogResult {
  commits: GitGraphCommit[]
  scope: GitGraphScope
}

export interface GraphLogSource {
  /** Resolves null when no graph can be shown; never rejects. */
  load(cwd: string, scope: GitGraphScope, count: number): Promise<GraphLogResult | null>
  /** Allows a failed path (and scope) to be retried. */
  forget(cwd: string, scope?: GitGraphScope): void
}

/**
 * A failed path + scope is not retried until `forget`, so a repo that times
 * out or a remote host without the graph channel is asked at most once.
 */
export function createGraphLogSource(fetchLog: GraphLogFetcher, logFailure: (message: string) => void): GraphLogSource {
  const failed = new Set<string>()
  const logged = new Set<string>()
  const keyOf = (cwd: string, scope: GitGraphScope) => `${scope}\u0000${cwd}`

  async function attempt(cwd: string, scope: GitGraphScope, count: number): Promise<GitGraphCommit[] | null> {
    const key = keyOf(cwd, scope)
    if (failed.has(key)) return null
    let reason = 'unavailable'
    try {
      const commits = await fetchLog(cwd, count, scope)
      if (Array.isArray(commits)) return commits
    } catch (error) {
      reason = `failed: ${error instanceof Error ? error.message : String(error)}`
    }
    failed.add(key)
    if (!logged.has(key)) {
      logged.add(key)
      logFailure(`[git-graph] graph log ${reason} (scope=${scope}, cwd=${cwd}); showing the plain commit list`)
    }
    return null
  }

  return {
    async load(cwd, scope, count) {
      const commits = await attempt(cwd, scope, count)
      if (commits) return { commits, scope }
      if (scope === 'current') return null
      const fallback = await attempt(cwd, 'current', count)
      return fallback ? { commits: fallback, scope: 'current' } : null
    },
    forget(cwd, scope) {
      for (const target of scope ? [scope] : (['current', 'all'] as const)) failed.delete(keyOf(cwd, target))
    },
  }
}
