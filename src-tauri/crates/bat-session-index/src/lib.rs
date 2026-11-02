//! Indexes local Claude Code and Codex transcripts and groups them by project.

mod cache;
mod claude;
mod codex;
mod dir_probe;
mod discover;
mod finalize;
mod git;
mod group;
mod line_reader;
mod model;
mod paths;
mod text;
mod time;

#[cfg(test)]
mod test_support;

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::cache::{load_cache, mtime_ns, save_cache, CacheEntry};
use crate::dir_probe::DirState;
use crate::group::{group_sessions, GroupContext};
use crate::model::{is_valid_session_id, SessionRecord};

pub use crate::paths::{normalize_host_path, normalize_path, PathStyle};

pub const DEFAULT_MAX_LINE_BYTES: u64 = 10 * 1024 * 1024;
pub const MIN_MAX_LINE_BYTES: u64 = 1024 * 1024;
const PARSE_CONCURRENCY: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionAgent {
    Claude,
    Codex,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionGroupKind {
    Repo,
    Dir,
    Missing,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionHistoryEntry {
    pub agent: SessionAgent,
    pub id: String,
    pub title: String,
    pub first_prompt: Option<String>,
    pub last_reply: Option<String>,
    pub message_count: u32,
    pub subagent_count: u32,
    pub model: Option<String>,
    pub git_branch: Option<String>,
    pub cwd: Option<String>,
    pub cwd_exists: bool,
    pub started_at: Option<String>,
    pub updated_at: Option<String>,
    pub group_key: String,
    pub skipped_lines: u32,
}

impl SessionHistoryEntry {
    fn from_record(record: SessionRecord, group_key: String, cwd_exists: bool) -> Self {
        SessionHistoryEntry {
            agent: record.agent,
            id: record.id,
            title: record.title,
            first_prompt: record.first_prompt,
            last_reply: record.last_reply,
            message_count: record.message_count,
            subagent_count: record.subagent_count,
            model: record.model,
            git_branch: record.git_branch,
            cwd: record.cwd,
            cwd_exists,
            started_at: record.started_at,
            updated_at: record.updated_at,
            group_key,
            skipped_lines: record.skipped_lines,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionHistoryGroup {
    pub key: String,
    pub kind: SessionGroupKind,
    pub name: String,
    pub root_path: Option<String>,
    pub normalized_root_path: Option<String>,
    pub worktree_count: u32,
    pub session_count: u32,
    pub latest_updated_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionHistoryScanStatus {
    pub file_count: u32,
    pub parsed_count: u32,
    pub cache_hits: u32,
    pub failed_count: u32,
    pub skipped_lines: u32,
    pub duration_ms: u64,
    pub scanned_at: String,
    pub git_available: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionHistoryResult {
    pub sessions: Vec<SessionHistoryEntry>,
    pub groups: Vec<SessionHistoryGroup>,
    pub scan: SessionHistoryScanStatus,
    pub case_insensitive_paths: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanOptions {
    /// None skips Claude.
    pub claude_home: Option<PathBuf>,
    /// None skips Codex.
    pub codex_home: Option<PathBuf>,
    /// None disables the persistent cache.
    pub cache_path: Option<PathBuf>,
    /// Clamped to at least [`MIN_MAX_LINE_BYTES`].
    pub max_line_bytes: u64,
    /// Manual refresh: re-runs git for directories that were not repositories and re-probes cached UNC folders.
    pub retry_git_misses: bool,
}

impl Default for ScanOptions {
    /// `~/.claude` and `$CODEX_HOME` (else `~/.codex`), no persistent cache.
    fn default() -> Self {
        let home = home_dir();
        let codex_home = std::env::var_os("CODEX_HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .or_else(|| home.as_ref().map(|h| h.join(".codex")));
        ScanOptions {
            claude_home: home.map(|h| h.join(".claude")),
            codex_home,
            cache_path: None,
            max_line_bytes: DEFAULT_MAX_LINE_BYTES,
            retry_git_misses: false,
        }
    }
}

fn home_dir() -> Option<PathBuf> {
    let var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var_os(var)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

#[derive(Clone, Copy)]
enum SourceKind {
    Claude,
    Codex,
}

enum JobOutcome {
    Vanished,
    Hit(String, CacheEntry),
    Parsed(String, CacheEntry),
    Failed,
}

/// Blocking; read-only for the agent data directories. Unreadable files and cache problems are reported in
/// the scan status instead of failing the call. The caller must serialize calls.
pub fn scan(opts: &ScanOptions) -> SessionHistoryResult {
    let started = Instant::now();
    let max_line_bytes = opts.max_line_bytes.max(MIN_MAX_LINE_BYTES);
    let style = PathStyle::host();

    let mut jobs: Vec<(PathBuf, SourceKind)> = Vec::new();
    if let Some(home) = &opts.claude_home {
        jobs.extend(
            discover::list_claude_transcripts(home)
                .into_iter()
                .map(|p| (p, SourceKind::Claude)),
        );
    }
    let codex_titles = match &opts.codex_home {
        Some(home) => {
            jobs.extend(
                discover::list_codex_rollouts(home)
                    .into_iter()
                    .map(|p| (p, SourceKind::Codex)),
            );
            codex::read_codex_title_index(&home.join("session_index.jsonl"), max_line_bytes)
        }
        None => HashMap::new(),
    };
    let previous = opts
        .cache_path
        .as_deref()
        .map(|path| load_cache(path, max_line_bytes))
        .unwrap_or_default();

    let outcomes = run_jobs(&jobs, &previous, max_line_bytes);

    let mut status = SessionHistoryScanStatus {
        file_count: jobs.len() as u32,
        parsed_count: 0,
        cache_hits: 0,
        failed_count: 0,
        skipped_lines: 0,
        duration_ms: 0,
        scanned_at: String::new(),
        git_available: false,
    };
    let mut next_entries: HashMap<String, CacheEntry> = HashMap::with_capacity(outcomes.len());
    for outcome in outcomes {
        match outcome {
            JobOutcome::Vanished => {}
            JobOutcome::Failed => status.failed_count += 1,
            JobOutcome::Hit(path, entry) => {
                status.cache_hits += 1;
                next_entries.insert(path, entry);
            }
            JobOutcome::Parsed(path, entry) => {
                status.parsed_count += 1;
                next_entries.insert(path, entry);
            }
        }
    }
    // Ids reach shell commands the user copies, so a file with an unsafe id is reported as failed, not listed.
    let (records, rejected): (Vec<SessionRecord>, Vec<SessionRecord>) = next_entries
        .values()
        .filter_map(|entry| entry.record.clone())
        .partition(|record| is_valid_session_id(&record.id));
    status.failed_count += rejected.len() as u32;
    status.skipped_lines = records.iter().map(|r| r.skipped_lines).sum();

    // Unchanged hits covering every previous entry mean the file on disk is already current.
    let cache_changed = status.parsed_count > 0 || status.cache_hits as usize != previous.len();
    if let (Some(path), true) = (&opts.cache_path, cache_changed) {
        let _ = save_cache(path, max_line_bytes, next_entries);
    }

    let sessions = finalize::finalize_sessions(records, &codex_titles);
    let (sessions, groups, git_available) = group_with_git(sessions, style, opts.retry_git_misses);
    status.git_available = git_available;
    status.duration_ms = started.elapsed().as_millis() as u64;
    status.scanned_at = time::now_iso_utc();
    SessionHistoryResult {
        sessions,
        groups,
        scan: status,
        case_insensitive_paths: style.case_insensitive(),
    }
}

fn run_jobs(
    jobs: &[(PathBuf, SourceKind)],
    previous: &HashMap<String, CacheEntry>,
    max_line_bytes: u64,
) -> Vec<JobOutcome> {
    let next = AtomicUsize::new(0);
    thread::scope(|scope| {
        let workers: Vec<_> = (0..PARSE_CONCURRENCY.min(jobs.len()))
            .map(|_| {
                scope.spawn(|| {
                    let mut outcomes = Vec::new();
                    while let Some((path, kind)) = jobs.get(next.fetch_add(1, Ordering::Relaxed)) {
                        outcomes.push(run_job(path, *kind, previous, max_line_bytes));
                    }
                    outcomes
                })
            })
            .collect();
        workers
            .into_iter()
            .flat_map(|worker| worker.join().unwrap_or_default())
            .collect()
    })
}

fn run_job(
    path: &Path,
    kind: SourceKind,
    previous: &HashMap<String, CacheEntry>,
    max_line_bytes: u64,
) -> JobOutcome {
    let Ok(meta) = fs::metadata(path) else {
        return JobOutcome::Vanished;
    };
    let mtime = meta.modified().map(mtime_ns).unwrap_or(0);
    let size = meta.len();
    let key = path.to_string_lossy().into_owned();
    if let Some(entry) = previous
        .get(&key)
        .filter(|e| e.mtime_ns == mtime && e.size == size)
    {
        return JobOutcome::Hit(key, entry.clone());
    }
    let parsed = match kind {
        SourceKind::Claude => claude::parse_claude_session(path, max_line_bytes).map(Some),
        SourceKind::Codex => codex::parse_codex_session(path, max_line_bytes),
    };
    match parsed {
        Ok(record) => JobOutcome::Parsed(
            key,
            CacheEntry {
                mtime_ns: mtime,
                size,
                record,
            },
        ),
        Err(_) => JobOutcome::Failed,
    }
}

fn group_with_git(
    sessions: Vec<SessionRecord>,
    style: PathStyle,
    retry_git_misses: bool,
) -> (Vec<SessionHistoryEntry>, Vec<SessionHistoryGroup>, bool) {
    let cwds: Vec<&str> = sessions.iter().filter_map(|s| s.cwd.as_deref()).collect();
    let dir_states = dir_probe::process_prober().probe_all(&cwds, style, retry_git_misses);
    let existing: Vec<&str> = cwds
        .into_iter()
        .filter(|cwd| dir_states.get(&normalize_path(cwd, style)) == Some(&DirState::Exists))
        .collect();

    let mut resolver = git::process_resolver();
    resolver.resolve_all(&existing, style, retry_git_misses, &git::run_git_common_dir);
    let git_available = resolver.git_available(git::probe_git_version);
    let repo_of = |dir: &str| resolver.lookup(dir, style).cloned();
    let dir_state = |dir: &str| {
        dir_states
            .get(&normalize_path(dir, style))
            .copied()
            .unwrap_or(DirState::Unknown)
    };
    let ctx = GroupContext {
        style,
        repo_of: &repo_of,
        dir_state: &dir_state,
    };
    let (entries, groups) = group_sessions(sessions, &ctx);
    (entries, groups, git_available)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{copy_dir, fixture, TempDir};
    use std::fs::OpenOptions;
    use std::io::Write;

    const CODEX_PAGED_ID: &str = "0aaaaaaa-0000-7000-8000-000000000001";
    const FILE_COUNT: u32 = 10;

    struct Data {
        dir: TempDir,
    }

    impl Data {
        fn new(label: &str) -> Self {
            let dir = TempDir::new(label);
            copy_dir(&fixture(&[]), &dir.path().join("data"));
            Data { dir }
        }

        fn options(&self) -> ScanOptions {
            ScanOptions {
                claude_home: Some(self.dir.path().join("data").join("claude")),
                codex_home: Some(self.dir.path().join("data").join("codex")),
                cache_path: None,
                max_line_bytes: DEFAULT_MAX_LINE_BYTES,
                retry_git_misses: false,
            }
        }

        fn cached_options(&self) -> ScanOptions {
            ScanOptions {
                cache_path: Some(self.dir.path().join("cache").join("index-v1.json")),
                ..self.options()
            }
        }
    }

    fn counts(result: &SessionHistoryResult) -> (u32, u32, u32, u32) {
        let s = &result.scan;
        (s.file_count, s.parsed_count, s.cache_hits, s.failed_count)
    }

    #[test]
    fn lists_interactive_top_level_sessions_newest_first() {
        let data = Data::new("scan-list");
        let result = scan(&data.options());
        let ids: Vec<&str> = result.sessions.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                CODEX_PAGED_ID,
                "33333333-aaaa-4bbb-8ccc-000000000003",
                "11111111-aaaa-4bbb-8ccc-000000000001"
            ]
        );
        assert_eq!(result.sessions[0].title, "Rename config loader");
        assert_eq!(result.sessions[0].subagent_count, 2);
        assert_eq!(counts(&result), (FILE_COUNT, FILE_COUNT, 0, 0));
        assert_eq!(
            result.case_insensitive_paths,
            PathStyle::host().case_insensitive()
        );
    }

    #[test]
    fn paginated_codex_thread_is_merged() {
        let data = Data::new("scan-pages");
        let result = scan(&data.options());
        let paged: Vec<&SessionHistoryEntry> = result
            .sessions
            .iter()
            .filter(|s| s.id == CODEX_PAGED_ID)
            .collect();
        assert_eq!(paged.len(), 1);
        let s = paged[0];
        assert_eq!(s.message_count, 6);
        assert_eq!(s.first_prompt.as_deref(), Some("Rename the config loader"));
        assert_eq!(s.last_reply.as_deref(), Some("Changelog updated."));
        assert_eq!(s.model.as_deref(), Some("test-model-b"));
        assert_eq!(s.git_branch.as_deref(), Some("feat/config-loader"));
        assert_eq!(s.started_at.as_deref(), Some("2026-10-03T10:00:00.000Z"));
        assert_eq!(s.updated_at.as_deref(), Some("2026-10-07T09:01:00.000Z"));
    }

    #[test]
    fn non_interactive_and_subagent_sessions_are_hidden() {
        let data = Data::new("scan-hidden");
        let ids: Vec<String> = scan(&data.options())
            .sessions
            .into_iter()
            .map(|s| s.id)
            .collect();
        for hidden in [
            "22222222-aaaa-4bbb-8ccc-000000000002",
            "0aaaaaaa-0000-7000-8000-000000000002",
            "0aaaaaaa-0000-7000-8000-000000000003",
            "0aaaaaaa-0000-7000-8000-000000000004",
            "0aaaaaaa-0000-7000-8000-000000000005",
        ] {
            assert!(
                !ids.iter().any(|id| id == hidden),
                "{hidden} should be hidden"
            );
        }
    }

    #[test]
    fn persisted_cache_skips_unchanged_files_and_reparses_modified_ones() {
        let data = Data::new("scan-cache");
        let first = scan(&data.cached_options());
        assert_eq!(counts(&first), (FILE_COUNT, FILE_COUNT, 0, 0));

        let second = scan(&data.cached_options());
        assert_eq!(counts(&second), (FILE_COUNT, 0, FILE_COUNT, 0));
        assert_eq!(second.sessions, first.sessions);
        assert_eq!(second.groups, first.groups);

        let exec = data.dir.path().join("data/codex/sessions/2026/10/04/rollout-2026-10-04T11-00-00-0aaaaaaa-0000-7000-8000-000000000002.jsonl");
        let mut file = OpenOptions::new().append(true).open(exec).unwrap();
        file.write_all(b"{\"timestamp\":\"2026-10-04T11:10:00.000Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"done\"}]}}\n").unwrap();
        drop(file);
        let third = scan(&data.cached_options());
        assert_eq!(counts(&third), (FILE_COUNT, 1, FILE_COUNT - 1, 0));
    }

    #[test]
    fn deleted_files_are_pruned_from_the_cache() {
        let data = Data::new("scan-prune");
        scan(&data.cached_options());
        fs::remove_file(
            data.dir.path().join(
                "data/claude/projects/D--work-demo/33333333-aaaa-4bbb-8ccc-000000000003.jsonl",
            ),
        )
        .unwrap();
        let after = scan(&data.cached_options());
        assert_eq!(counts(&after), (FILE_COUNT - 1, 0, FILE_COUNT - 1, 0));
        let cache = load_cache(
            data.cached_options().cache_path.as_deref().unwrap(),
            DEFAULT_MAX_LINE_BYTES,
        );
        assert_eq!(cache.len(), (FILE_COUNT - 1) as usize);
    }

    #[test]
    fn cache_is_discarded_when_max_line_bytes_or_schema_changes() {
        let data = Data::new("scan-invalidate");
        scan(&data.cached_options());
        let other_limit = ScanOptions {
            max_line_bytes: 2 * MIN_MAX_LINE_BYTES,
            ..data.cached_options()
        };
        assert_eq!(counts(&scan(&other_limit)), (FILE_COUNT, FILE_COUNT, 0, 0));
        assert_eq!(counts(&scan(&other_limit)), (FILE_COUNT, 0, FILE_COUNT, 0));

        let cache_path = data.cached_options().cache_path.unwrap();
        fs::write(
            &cache_path,
            r#"{"version":0,"maxLineBytes":2097152,"entries":{}}"#,
        )
        .unwrap();
        assert_eq!(counts(&scan(&other_limit)), (FILE_COUNT, FILE_COUNT, 0, 0));
    }

    #[test]
    fn small_limits_are_clamped() {
        let data = Data::new("scan-clamp");
        let tiny = ScanOptions {
            max_line_bytes: 0,
            ..data.options()
        };
        assert_eq!(scan(&tiny).scan.skipped_lines, 0);
    }

    #[test]
    fn broken_cache_files_never_fail_the_scan() {
        let data = Data::new("scan-broken-cache");
        let cache_path = data.cached_options().cache_path.unwrap();
        fs::create_dir_all(cache_path.parent().unwrap()).unwrap();
        fs::write(&cache_path, "{not json").unwrap();
        assert_eq!(
            counts(&scan(&data.cached_options())),
            (FILE_COUNT, FILE_COUNT, 0, 0)
        );

        let blocked = ScanOptions {
            cache_path: Some(
                data.dir
                    .path()
                    .join("data")
                    .join("codex")
                    .join("session_index.jsonl")
                    .join("cache.json"),
            ),
            ..data.options()
        };
        assert_eq!(scan(&blocked).sessions.len(), 3);
    }

    #[test]
    fn missing_or_skipped_homes_produce_empty_results() {
        let dir = TempDir::new("scan-empty");
        let missing = ScanOptions {
            claude_home: Some(dir.path().join("nope")),
            codex_home: Some(dir.path().join("nope")),
            cache_path: None,
            max_line_bytes: DEFAULT_MAX_LINE_BYTES,
            retry_git_misses: false,
        };
        let result = scan(&missing);
        assert!(result.sessions.is_empty() && result.groups.is_empty());
        assert_eq!(result.scan.file_count, 0);

        let data = Data::new("scan-skip");
        let claude_only = ScanOptions {
            codex_home: None,
            ..data.options()
        };
        assert!(scan(&claude_only)
            .sessions
            .iter()
            .all(|s| s.agent == SessionAgent::Claude));
    }

    #[test]
    fn json_shape_matches_contract() {
        let data = Data::new("scan-json");
        let value = serde_json::to_value(scan(&data.options())).unwrap();
        let keys = |v: &serde_json::Value| {
            let mut k: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
            k.sort();
            k
        };
        assert_eq!(
            keys(&value),
            ["caseInsensitivePaths", "groups", "scan", "sessions"]
        );
        assert_eq!(
            keys(&value["sessions"][0]),
            [
                "agent",
                "cwd",
                "cwdExists",
                "firstPrompt",
                "gitBranch",
                "groupKey",
                "id",
                "lastReply",
                "messageCount",
                "model",
                "skippedLines",
                "startedAt",
                "subagentCount",
                "title",
                "updatedAt"
            ]
        );
        assert_eq!(
            keys(&value["groups"][0]),
            [
                "key",
                "kind",
                "latestUpdatedAt",
                "name",
                "normalizedRootPath",
                "rootPath",
                "sessionCount",
                "worktreeCount"
            ]
        );
        assert_eq!(
            keys(&value["scan"]),
            [
                "cacheHits",
                "durationMs",
                "failedCount",
                "fileCount",
                "gitAvailable",
                "parsedCount",
                "scannedAt",
                "skippedLines"
            ]
        );
        assert_eq!(value["sessions"][0]["agent"], "codex");
        let untitled = &value["sessions"][1];
        assert_eq!(untitled["title"], "(untitled)");
        assert!(untitled["firstPrompt"].is_null());
        assert!(time::parse_iso_millis(value["scan"]["scannedAt"].as_str().unwrap()).is_some());
    }

    #[test]
    fn real_git_repo_and_worktree_share_one_group() {
        if let Some(reason) = git::tests::git_unavailable_reason() {
            eprintln!("skipping real git scan test: {reason}");
            return;
        }
        let dir = TempDir::new("scan-git");
        let (main, linked) = git::tests::make_repo_with_worktree(dir.path());
        let plain = dir.path().join("plain");
        fs::create_dir_all(&plain).unwrap();
        let gone = dir.path().join("gone");
        let project = dir.path().join("claude").join("projects").join("p");
        fs::create_dir_all(&project).unwrap();
        let write_session = |id: &str, cwd: Option<&Path>, time: &str| {
            let mut record = serde_json::json!({
                "type": "user", "timestamp": time, "entrypoint": "cli",
                "message": {"role": "user", "content": format!("prompt {id}")}
            });
            if let Some(cwd) = cwd {
                record["cwd"] = cwd.to_string_lossy().into_owned().into();
            }
            fs::write(project.join(format!("{id}.jsonl")), format!("{record}\n")).unwrap();
        };
        write_session("unknown", None, "2026-10-09T00:00:00Z");
        write_session("gone", Some(&gone), "2026-10-08T00:00:00Z");
        write_session("main", Some(&main), "2026-10-01T00:00:00Z");
        write_session("linked", Some(&linked), "2026-10-02T00:00:00Z");
        write_session("main-sub", Some(&main.join(".")), "2026-09-01T00:00:00Z");
        write_session("plain", Some(&plain), "2026-10-03T00:00:00Z");

        let result = scan(&ScanOptions {
            claude_home: Some(dir.path().join("claude")),
            codex_home: None,
            cache_path: None,
            max_line_bytes: DEFAULT_MAX_LINE_BYTES,
            retry_git_misses: true,
        });
        assert!(result.scan.git_available);
        let summary: Vec<(SessionGroupKind, u32)> = result
            .groups
            .iter()
            .map(|g| (g.kind, g.session_count))
            .collect();
        assert_eq!(
            summary,
            vec![
                (SessionGroupKind::Dir, 1),
                (SessionGroupKind::Repo, 3),
                (SessionGroupKind::Missing, 1),
                (SessionGroupKind::Unknown, 1),
            ]
        );
        let style = PathStyle::host();
        let repo = &result.groups[1];
        assert_eq!(repo.name, "main");
        assert_eq!(repo.worktree_count, 1);
        assert_eq!(
            repo.normalized_root_path,
            Some(normalize_path(main.to_str().unwrap(), style))
        );
        assert!(repo.key.starts_with("repo:"));
        assert_eq!(result.groups[0].name, "plain");
        let linked_entry = result.sessions.iter().find(|s| s.id == "linked").unwrap();
        assert_eq!(linked_entry.group_key, repo.key);
        assert!(linked_entry.cwd_exists);
        assert!(
            !result
                .sessions
                .iter()
                .find(|s| s.id == "gone")
                .unwrap()
                .cwd_exists
        );
    }

    #[test]
    fn sessions_with_unsafe_ids_are_dropped_and_counted_as_failed() {
        let data = Data::new("scan-bad-id");
        let project = data.dir.path().join("data/claude/projects/D--work-demo");
        let record = r#"{"type":"user","timestamp":"2026-10-20T00:00:00Z","entrypoint":"cli","cwd":"D:\\work\\demo","message":{"role":"user","content":"hi"}}"#;
        fs::write(project.join("evil;calc & x.jsonl"), format!("{record}\n")).unwrap();
        let codex_day = data.dir.path().join("data/codex/sessions/2026/10/20");
        fs::create_dir_all(&codex_day).unwrap();
        let meta = r#"{"timestamp":"2026-10-20T00:00:00Z","type":"session_meta","payload":{"id":"$(touch pwned)","cwd":"D:\\work\\demo","source":"cli"}}"#;
        let prompt = r#"{"timestamp":"2026-10-20T00:00:01Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"hi"}]}}"#;
        fs::write(
            codex_day.join("rollout-2026-10-20T00-00-00-bad.jsonl"),
            format!("{meta}\n{prompt}\n"),
        )
        .unwrap();

        let result = scan(&data.options());
        assert!(result
            .sessions
            .iter()
            .all(|s| model::is_valid_session_id(&s.id)));
        assert_eq!(result.sessions.len(), 3);
        assert_eq!(counts(&result), (FILE_COUNT + 2, FILE_COUNT + 2, 0, 2));
    }

    /// Reads the real `~/.claude` and `~/.codex` without writing to them; the cache goes to a temp dir.
    #[test]
    #[ignore = "reads local agent data; run with --ignored --nocapture"]
    fn measure_local_scan() {
        let dir = TempDir::new("measure");
        let opts = ScanOptions {
            cache_path: Some(dir.path().join("index-v1.json")),
            ..ScanOptions::default()
        };
        println!(
            "claude_home={:?} codex_home={:?}",
            opts.claude_home, opts.codex_home
        );
        for label in ["cold", "warm", "warm2"] {
            let started = Instant::now();
            let result = scan(&opts);
            let s = &result.scan;
            println!(
                "{label}: wall={}ms durationMs={} files={} parsed={} hits={} failed={} skippedLines={} sessions={} groups={} gitAvailable={}",
                started.elapsed().as_millis(),
                s.duration_ms,
                s.file_count,
                s.parsed_count,
                s.cache_hits,
                s.failed_count,
                s.skipped_lines,
                result.sessions.len(),
                result.groups.len(),
                s.git_available
            );
        }
        let result = scan(&opts);
        for kind in [
            SessionGroupKind::Repo,
            SessionGroupKind::Dir,
            SessionGroupKind::Missing,
            SessionGroupKind::Unknown,
        ] {
            println!(
                "groups {kind:?}: {}",
                result.groups.iter().filter(|g| g.kind == kind).count()
            );
        }
        for agent in [SessionAgent::Claude, SessionAgent::Codex] {
            println!(
                "sessions {agent:?}: {}",
                result.sessions.iter().filter(|s| s.agent == agent).count()
            );
        }
    }
}
