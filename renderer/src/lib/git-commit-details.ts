export interface GitCommitDetails {
  hash: string
  parents: string[]
  authorName: string
  authorEmail: string
  authorDate: string
  committerName: string
  committerEmail: string
  committerDate: string
  message: string
}

export type CommitDetailsEntry = GitCommitDetails | null | 'loading'

export interface ParentLink {
  hash: string
  shortHash: string
  loaded: boolean
}

/** Expanding a row collapses any other; activating the expanded row collapses it. */
export function nextExpandedCommit(current: string | null, hash: string): string | null {
  return current === hash ? null : hash
}

/** Local time like "Fri Nov 13 2026 15:28:21 GMT+0800"; the input is git's strict ISO date. */
export function formatCommitDateTime(iso: string): string {
  const date = new Date(iso)
  if (Number.isNaN(date.getTime())) return iso
  return date.toString().replace(/\s*\(.*\)$/, '')
}

export function committerDiffersFromAuthor(details: GitCommitDetails): boolean {
  return details.committerName !== details.authorName
    || details.committerEmail !== details.authorEmail
    || Date.parse(details.committerDate) !== Date.parse(details.authorDate)
}

/** A parent can be jumped to only when its row is in the loaded list. */
export function parentLinks(parents: readonly string[], loadedHashes: ReadonlySet<string>): ParentLink[] {
  return parents.map(hash => ({ hash, shortHash: hash.substring(0, 7), loaded: loadedHashes.has(hash) }))
}

// Details fetched on demand and cached per hash for the panel session; failures are not
// cached. reset() (on a repo switch) drops every entry and any response still in flight.
export class CommitDetailsCache {
  private readonly entries = new Map<string, GitCommitDetails>()
  private readonly failed = new Set<string>()
  private readonly pending = new Set<string>()
  private generation = 0

  constructor(
    private readonly fetchDetails: (hash: string) => Promise<GitCommitDetails | null>,
    private readonly onChange: () => void,
  ) {}

  get(hash: string): CommitDetailsEntry | undefined {
    if (this.pending.has(hash)) return 'loading'
    if (this.entries.has(hash)) return this.entries.get(hash)
    return this.failed.has(hash) ? null : undefined
  }

  request(hash: string): Promise<void> {
    if (this.pending.has(hash) || this.entries.has(hash)) return Promise.resolve()
    const generation = this.generation
    this.pending.add(hash)
    this.failed.delete(hash)
    this.onChange()
    return Promise.resolve()
      .then(() => this.fetchDetails(hash))
      .catch(() => null)
      .then(details => {
        if (generation !== this.generation) return
        this.pending.delete(hash)
        if (details && details.hash === hash) this.entries.set(hash, details)
        else this.failed.add(hash)
        this.onChange()
      })
  }

  reset(): void {
    this.generation++
    this.entries.clear()
    this.failed.clear()
    this.pending.clear()
    this.onChange()
  }
}
