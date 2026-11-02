//! Moves whole sessions to the OS trash after host-side ownership, containment and activity checks.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

use crate::cache::{forget_cache_entries, load_cache_any_limit, mtime_ns};
use crate::finalize::finalize_sessions;
use crate::{clamp_max_line_bytes, collect, time, ScanOptions, SessionAgent};

/// A transcript touched this recently may belong to a CLI session that is still running.
pub const RECENT_ACTIVITY_WINDOW: Duration = Duration::from_secs(10 * 60);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SessionRef {
    pub agent: SessionAgent,
    pub id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TrashBlockReason {
    RecentlyUpdated,
    NotFound,
    OutsideDataDir,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionTrashPreviewItem {
    pub agent: SessionAgent,
    pub id: String,
    pub file_count: u64,
    pub total_bytes: u64,
    pub newest_modified_at: Option<String>,
    pub blocked_reason: Option<TrashBlockReason>,
}

/// Totals cover only the items that are not blocked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionTrashPreview {
    pub items: Vec<SessionTrashPreviewItem>,
    pub file_count: u64,
    pub total_bytes: u64,
    pub blocked_count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockedSession {
    #[serde(flatten)]
    pub session: SessionRef,
    pub reason: TrashBlockReason,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailedSession {
    #[serde(flatten)]
    pub session: SessionRef,
    pub error: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionTrashResult {
    pub trashed: Vec<SessionRef>,
    pub blocked: Vec<BlockedSession>,
    pub failed: Vec<FailedSession>,
}

/// Upper bound on sessions in one preview or trash request; larger batches risk outlasting the remote invoke timeout.
pub const MAX_TRASH_SESSIONS_PER_REQUEST: usize = 200;

/// Implementations must only move to a recoverable trash and never fall back to deleting.
pub trait TrashBackend {
    /// Moves `paths` in the given order and stops at the first failure; earlier paths may already be moved.
    fn move_to_trash(&self, paths: &[PathBuf]) -> Result<(), String>;
}

/// The OS Recycle Bin / Trash.
pub struct SystemTrash;

impl TrashBackend for SystemTrash {
    #[cfg(windows)]
    fn move_to_trash(&self, paths: &[PathBuf]) -> Result<(), String> {
        crate::windows_recycle::recycle(paths)
    }

    /// `NSFileManager.trashItemAtURL` only moves items into a Trash and, unlike the Finder method, needs no
    /// Automation consent or subprocess.
    #[cfg(target_os = "macos")]
    fn move_to_trash(&self, paths: &[PathBuf]) -> Result<(), String> {
        use trash::macos::{DeleteMethod, TrashContextExtMacos};
        let mut context = trash::TrashContext::default();
        context.set_delete_method(DeleteMethod::NsFileManager);
        context.delete_all(paths).map_err(|err| err.to_string())
    }

    #[cfg(not(any(windows, target_os = "macos")))]
    fn move_to_trash(&self, paths: &[PathBuf]) -> Result<(), String> {
        trash::delete_all(paths).map_err(|err| err.to_string())
    }
}

/// Blocking; the caller must serialize with `scan`. Never modifies any file.
pub fn trash_preview(
    opts: &ScanOptions,
    sessions: &[SessionRef],
    now: SystemTime,
) -> SessionTrashPreview {
    let index = owned_index(opts);
    let mut claimed = HashSet::new();
    let mut preview = SessionTrashPreview {
        items: Vec::new(),
        file_count: 0,
        total_bytes: 0,
        blocked_count: 0,
    };
    for session in unique(sessions) {
        let item = match plan(opts, &index, &session, now, &mut claimed) {
            Ok(plan) => {
                preview.file_count += plan.usage.file_count;
                preview.total_bytes += plan.usage.total_bytes;
                preview_item(session, &plan.usage, None)
            }
            Err(blocked) => {
                preview.blocked_count += 1;
                preview_item(session, &blocked.usage, Some(blocked.reason))
            }
        };
        preview.items.push(item);
    }
    preview
}

/// Blocking; the caller must serialize with `scan`. Checks each session again right before moving it and
/// drops the moved transcripts from the persistent cache.
pub fn trash_sessions(
    opts: &ScanOptions,
    sessions: &[SessionRef],
    now: &dyn Fn() -> SystemTime,
    backend: &dyn TrashBackend,
) -> SessionTrashResult {
    let index = owned_index(opts);
    let mut claimed = HashSet::new();
    let mut result = SessionTrashResult::default();
    let mut moved_keys: Vec<String> = Vec::new();
    for session in unique(sessions) {
        let plan = match plan(opts, &index, &session, now(), &mut claimed) {
            Ok(plan) => plan,
            Err(blocked) => {
                result.blocked.push(BlockedSession {
                    session,
                    reason: blocked.reason,
                });
                continue;
            }
        };
        let outcome = backend.move_to_trash(&plan.paths);
        let left: Vec<&PathBuf> = plan
            .paths
            .iter()
            .filter(|path| fs::symlink_metadata(path).is_ok())
            .collect();
        moved_keys.extend(
            plan.paths
                .iter()
                .filter(|path| !left.contains(path))
                .map(|path| path.to_string_lossy().into_owned()),
        );
        let error = match outcome {
            Ok(()) if left.is_empty() => None,
            Ok(()) => {
                Some("the trash reported success but some files are still in place".to_string())
            }
            Err(err) => Some(err),
        };
        match error {
            None => result.trashed.push(session),
            Some(err) => result.failed.push(FailedSession {
                session,
                error: format!(
                    "{} of {} paths were not moved to the trash: {err}",
                    left.len(),
                    plan.paths.len()
                ),
            }),
        }
    }
    if let Some(cache_path) = &opts.cache_path {
        let _ = forget_cache_entries(cache_path, &moved_keys);
    }
    result
}

fn preview_item(
    session: SessionRef,
    usage: &Usage,
    blocked_reason: Option<TrashBlockReason>,
) -> SessionTrashPreviewItem {
    SessionTrashPreviewItem {
        agent: session.agent,
        id: session.id,
        file_count: usage.file_count,
        total_bytes: usage.total_bytes,
        newest_modified_at: usage
            .newest
            .map(|mtime| time::format_iso_utc(mtime_ns(mtime).div_euclid(1_000_000))),
        blocked_reason,
    }
}

fn unique(sessions: &[SessionRef]) -> Vec<SessionRef> {
    let mut seen = HashSet::new();
    sessions
        .iter()
        .filter(|session| seen.insert(*session))
        .cloned()
        .collect()
}

type OwnedIndex = HashMap<(SessionAgent, String), Vec<PathBuf>>;

/// Owned paths of every listable session, archived and non-interactive included. Reuses the persistent
/// cache under whatever line limit it was written with, and leaves it untouched.
fn owned_index(opts: &ScanOptions) -> OwnedIndex {
    let (limit, previous) = opts
        .cache_path
        .as_deref()
        .and_then(load_cache_any_limit)
        .unwrap_or_else(|| (opts.max_line_bytes, HashMap::new()));
    let collected = collect(opts, clamp_max_line_bytes(limit), true, &previous);
    finalize_sessions(collected.records, &collected.codex_titles, true)
        .into_iter()
        .map(|record| {
            let paths = record.owned_paths.iter().map(PathBuf::from).collect();
            ((record.agent, record.id), paths)
        })
        .collect()
}

#[derive(Debug, Default)]
pub(crate) struct Usage {
    file_count: u64,
    pub(crate) total_bytes: u64,
    newest: Option<SystemTime>,
}

struct Plan {
    paths: Vec<PathBuf>,
    usage: Usage,
}

struct Blocked {
    reason: TrashBlockReason,
    usage: Usage,
}

impl From<TrashBlockReason> for Blocked {
    fn from(reason: TrashBlockReason) -> Self {
        Blocked {
            reason,
            usage: Usage::default(),
        }
    }
}

/// A path already claimed by an earlier session in the same request is counted and moved only once.
fn plan(
    opts: &ScanOptions,
    index: &OwnedIndex,
    session: &SessionRef,
    now: SystemTime,
    claimed: &mut HashSet<PathBuf>,
) -> Result<Plan, Blocked> {
    let home = match session.agent {
        SessionAgent::Claude => opts.claude_home.as_deref(),
        SessionAgent::Codex => opts.codex_home.as_deref(),
    };
    let (Some(home), Some(owned)) = (home, index.get(&(session.agent, session.id.clone()))) else {
        return Err(TrashBlockReason::NotFound.into());
    };
    let paths: Vec<PathBuf> = owned
        .iter()
        .filter(|path| !claimed.contains(*path) && fs::symlink_metadata(path).is_ok())
        .cloned()
        .collect();
    if paths.is_empty() {
        return Err(TrashBlockReason::NotFound.into());
    }
    if !paths.iter().all(|path| is_inside_home(path, home)) {
        return Err(TrashBlockReason::OutsideDataDir.into());
    }
    let usage = measure(&paths);
    if usage.newest.is_some_and(|newest| is_recent(newest, now)) {
        return Err(Blocked {
            reason: TrashBlockReason::RecentlyUpdated,
            usage,
        });
    }
    claimed.extend(paths.iter().cloned());
    Ok(Plan { paths, usage })
}

/// A modification time in the future also counts as recent.
fn is_recent(modified: SystemTime, now: SystemTime) -> bool {
    now.duration_since(modified)
        .map_or(true, |age| age < RECENT_ACTIVITY_WINDOW)
}

/// Requires a strict descendant reached through plain components with no link on the way, whose canonical
/// form is still under the canonical home.
pub(crate) fn is_inside_home(path: &Path, home: &Path) -> bool {
    let Ok(relative) = path.strip_prefix(home) else {
        return false;
    };
    if relative.as_os_str().is_empty()
        || !relative
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
    {
        return false;
    }
    let mut current = home.to_path_buf();
    for component in relative.components() {
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(meta) if !meta.file_type().is_symlink() => {}
            _ => return false,
        }
    }
    match (fs::canonicalize(home), fs::canonicalize(path)) {
        (Ok(home), Ok(path)) => path != home && path.starts_with(&home),
        _ => false,
    }
}

/// lstat semantics: links (including Windows junctions) count as one entry and are never followed; only regular
/// files contribute bytes. Only non-directory entries supply the newest mtime.
pub(crate) fn measure(paths: &[PathBuf]) -> Usage {
    let mut usage = Usage::default();
    let mut pending: Vec<PathBuf> = paths.to_vec();
    while let Some(path) = pending.pop() {
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.is_dir() {
            if let Ok(entries) = fs::read_dir(&path) {
                pending.extend(entries.filter_map(Result::ok).map(|entry| entry.path()));
            }
            continue;
        }
        if let Ok(modified) = meta.modified() {
            usage.newest = Some(usage.newest.map_or(modified, |newest| newest.max(modified)));
        }
        usage.file_count += 1;
        if meta.is_file() {
            usage.total_bytes += meta.len();
        }
    }
    usage
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::load_cache;
    use crate::test_support::{copy_dir, fixture, TempDir};
    use crate::{scan, DEFAULT_MAX_LINE_BYTES, MIN_MAX_LINE_BYTES};
    use std::cell::{Cell, RefCell};
    use std::fs::File;

    const CLAUDE_MAIN: &str = "11111111-aaaa-4bbb-8ccc-000000000001";
    const CLAUDE_PRINT: &str = "22222222-aaaa-4bbb-8ccc-000000000002";
    const CLAUDE_BARE: &str = "33333333-aaaa-4bbb-8ccc-000000000003";
    const CODEX_PAGED: &str = "0aaaaaaa-0000-7000-8000-000000000001";
    const CODEX_EXEC: &str = "0aaaaaaa-0000-7000-8000-000000000002";
    const CODEX_SPAWNED: &str = "0aaaaaaa-0000-7000-8000-000000000003";
    const CODEX_GRANDCHILD: &str = "0aaaaaaa-0000-7000-8000-0000000000c1";

    struct Data {
        dir: TempDir,
    }

    impl Data {
        fn new(label: &str) -> Self {
            let dir = TempDir::new(label);
            copy_dir(&fixture(&[]), &dir.path().join("data"));
            Data { dir }
        }

        fn claude_home(&self) -> PathBuf {
            self.dir.path().join("data").join("claude")
        }

        fn codex_home(&self) -> PathBuf {
            self.dir.path().join("data").join("codex")
        }

        fn claude_project(&self) -> PathBuf {
            self.claude_home().join("projects").join("D--work-demo")
        }

        fn options(&self) -> ScanOptions {
            ScanOptions {
                claude_home: Some(self.claude_home()),
                codex_home: Some(self.codex_home()),
                cache_path: Some(self.dir.path().join("cache").join("index-v1.json")),
                max_line_bytes: DEFAULT_MAX_LINE_BYTES,
                retry_git_misses: false,
                include_non_interactive: false,
                include_codex_archived: false,
            }
        }

        /// A spawned subagent of the spawned subagent, stored under archived_sessions/.
        fn add_archived_grandchild(&self) -> PathBuf {
            let dir = self.codex_home().join("archived_sessions");
            fs::create_dir_all(&dir).unwrap();
            let path = dir.join(format!(
                "rollout-2026-10-05T08-00-00-{CODEX_GRANDCHILD}.jsonl"
            ));
            let meta = serde_json::json!({
                "timestamp": "2026-10-05T08:00:00.000Z", "type": "session_meta",
                "payload": {"id": CODEX_GRANDCHILD, "cwd": "D:\\work\\demo",
                            "source": {"subagent": {"thread_spawn": {"parent_thread_id": CODEX_SPAWNED}}}}
            });
            fs::write(&path, format!("{meta}\n")).unwrap();
            path
        }
    }

    fn claude(id: &str) -> SessionRef {
        SessionRef {
            agent: SessionAgent::Claude,
            id: id.into(),
        }
    }

    fn codex(id: &str) -> SessionRef {
        SessionRef {
            agent: SessionAgent::Codex,
            id: id.into(),
        }
    }

    fn later() -> SystemTime {
        SystemTime::now() + Duration::from_secs(24 * 60 * 60)
    }

    fn reason(preview: &SessionTrashPreview, index: usize) -> Option<TrashBlockReason> {
        preview.items[index].blocked_reason
    }

    fn file_size(path: &Path) -> u64 {
        fs::metadata(path).unwrap().len()
    }

    fn set_mtime_all(root: &Path, mtime: SystemTime) {
        if root.is_dir() {
            for entry in fs::read_dir(root).unwrap() {
                set_mtime_all(&entry.unwrap().path(), mtime);
            }
        } else {
            set_mtime(root, mtime);
        }
    }

    fn set_mtime(path: &Path, mtime: SystemTime) {
        File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(mtime)
            .unwrap();
    }

    /// Records each request and moves up to `budget` paths into `bin`, stopping with an error at the budget or at
    /// `locked`; it never deletes.
    struct FakeTrash {
        bin: PathBuf,
        budget: Cell<usize>,
        locked: Option<PathBuf>,
        calls: RefCell<Vec<Vec<PathBuf>>>,
    }

    impl FakeTrash {
        fn moving(bin: PathBuf) -> Self {
            Self::moving_at_most(bin, usize::MAX)
        }

        fn moving_at_most(bin: PathBuf, budget: usize) -> Self {
            fs::create_dir_all(&bin).unwrap();
            FakeTrash {
                bin,
                budget: Cell::new(budget),
                locked: None,
                calls: RefCell::new(Vec::new()),
            }
        }

        fn stopping_at(bin: PathBuf, locked: PathBuf) -> Self {
            FakeTrash {
                locked: Some(locked),
                ..Self::moving(bin)
            }
        }

        fn failing() -> Self {
            FakeTrash {
                bin: PathBuf::new(),
                budget: Cell::new(0),
                locked: None,
                calls: RefCell::new(Vec::new()),
            }
        }

        fn moved_paths(&self) -> Vec<PathBuf> {
            self.calls.borrow().concat()
        }
    }

    impl TrashBackend for FakeTrash {
        fn move_to_trash(&self, paths: &[PathBuf]) -> Result<(), String> {
            self.calls.borrow_mut().push(paths.to_vec());
            for path in paths {
                if self.locked.as_ref() == Some(path) {
                    return Err("file is locked".into());
                }
                if self.budget.get() == 0 {
                    return Err("trash unavailable".into());
                }
                self.budget.set(self.budget.get() - 1);
                let target = self.bin.join(format!(
                    "{}-{}",
                    self.budget.get(),
                    path.file_name().unwrap().to_string_lossy()
                ));
                fs::rename(path, target).map_err(|err| err.to_string())?;
            }
            Ok(())
        }
    }

    /// Creates a directory link (a junction on Windows); None when the platform refuses.
    fn link_dir(link: &Path, target: &Path) -> Option<()> {
        #[cfg(windows)]
        {
            let status = std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(link)
                .arg(target)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .ok()?;
            status.success().then_some(())
        }
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(target, link).ok()
        }
    }

    #[test]
    fn claude_session_owns_transcript_and_sibling_folder() {
        let data = Data::new("trash-claude-owned");
        let project = data.claude_project();
        let preview = trash_preview(
            &data.options(),
            &[claude(CLAUDE_MAIN), claude(CLAUDE_BARE)],
            later(),
        );
        let sidecar = project.join(CLAUDE_MAIN).join("subagents");
        let expected_main = file_size(&project.join(format!("{CLAUDE_MAIN}.jsonl")))
            + ["agent-a1.jsonl", "agent-a1.meta.json", "agent-a2.jsonl"]
                .iter()
                .map(|name| file_size(&sidecar.join(name)))
                .sum::<u64>();
        assert_eq!(reason(&preview, 0), None);
        assert_eq!(preview.items[0].file_count, 4);
        assert_eq!(preview.items[0].total_bytes, expected_main);
        assert!(preview.items[0].newest_modified_at.is_some());
        assert_eq!(preview.items[1].file_count, 1);
        assert_eq!(preview.file_count, 5);
        assert_eq!(
            preview.total_bytes,
            expected_main + file_size(&project.join(format!("{CLAUDE_BARE}.jsonl")))
        );
        assert_eq!(preview.blocked_count, 0);
    }

    #[test]
    fn codex_thread_owns_every_page_and_all_descendants() {
        let data = Data::new("trash-codex-owned");
        let grandchild = data.add_archived_grandchild();
        let index = owned_index(&data.options());
        let mut owned: Vec<String> = index[&(SessionAgent::Codex, CODEX_PAGED.to_string())]
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        owned.sort();
        assert_eq!(
            owned,
            vec![
                format!("rollout-2026-10-03T10-00-00-{CODEX_PAGED}.jsonl"),
                format!("rollout-2026-10-04T11-30-00-{CODEX_SPAWNED}.jsonl"),
                "rollout-2026-10-04T12-30-00-0aaaaaaa-0000-7000-8000-000000000005.jsonl".to_string(),
                format!("rollout-2026-10-05T08-00-00-{CODEX_GRANDCHILD}.jsonl"),
                format!("rollout-2026-10-07T09-00-00-{CODEX_PAGED}_0aaaaaaa-0000-7000-8000-0000000000f1.jsonl"),
            ]
        );
        assert!(index[&(SessionAgent::Codex, CODEX_PAGED.to_string())].contains(&grandchild));

        let preview = trash_preview(&data.options(), &[codex(CODEX_PAGED)], later());
        assert_eq!(reason(&preview, 0), None);
        assert_eq!(preview.file_count, 5);
    }

    #[test]
    fn non_interactive_sessions_can_be_trashed_but_subagents_and_unknown_ids_cannot() {
        let data = Data::new("trash-not-found");
        let preview = trash_preview(
            &data.options(),
            &[
                claude(CLAUDE_PRINT),
                codex(CODEX_EXEC),
                codex(CODEX_SPAWNED),
                claude("44444444-aaaa-4bbb-8ccc-000000000404"),
                claude(CODEX_PAGED),
            ],
            later(),
        );
        let reasons: Vec<Option<TrashBlockReason>> =
            preview.items.iter().map(|i| i.blocked_reason).collect();
        let not_found = Some(TrashBlockReason::NotFound);
        assert_eq!(reasons, vec![None, None, not_found, not_found, not_found]);
        assert_eq!(preview.blocked_count, 3);
        assert_eq!(preview.file_count, 2);
    }

    #[test]
    fn duplicate_requests_are_counted_once() {
        let data = Data::new("trash-dup");
        let preview = trash_preview(
            &data.options(),
            &[claude(CLAUDE_BARE), claude(CLAUDE_BARE)],
            later(),
        );
        assert_eq!(preview.items.len(), 1);
        assert_eq!(preview.file_count, 1);
    }

    #[test]
    fn paths_must_stay_inside_the_home() {
        let data = Data::new("trash-inside");
        let home = data.claude_home();
        let transcript = data.claude_project().join(format!("{CLAUDE_MAIN}.jsonl"));
        assert!(is_inside_home(&transcript, &home));
        assert!(!is_inside_home(&home, &home));
        assert!(!is_inside_home(
            &home.join("projects").join("..").join("projects"),
            &home
        ));
        assert!(!is_inside_home(
            &data.codex_home().join("session_index.jsonl"),
            &home
        ));
        assert!(!is_inside_home(
            &home.join("projects").join("absent.jsonl"),
            &home
        ));
        assert!(!is_inside_home(Path::new("relative.jsonl"), &home));
    }

    #[test]
    fn linked_sibling_folder_is_outside_the_data_dir() {
        let data = Data::new("trash-link-escape");
        let outside = data.dir.path().join("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("precious.txt"), "keep").unwrap();
        let link = data.claude_project().join(CLAUDE_BARE);
        if link_dir(&link, &outside).is_none() {
            eprintln!("skipping: this platform refused to create a directory link");
            return;
        }
        let preview = trash_preview(&data.options(), &[claude(CLAUDE_BARE)], later());
        assert_eq!(reason(&preview, 0), Some(TrashBlockReason::OutsideDataDir));
        assert!(!is_inside_home(&link, &data.claude_home()));

        let backend = FakeTrash::failing();
        let result = trash_sessions(&data.options(), &[claude(CLAUDE_BARE)], &later, &backend);
        assert_eq!(result.blocked[0].reason, TrashBlockReason::OutsideDataDir);
        assert!(backend.calls.borrow().is_empty());
        assert!(outside.join("precious.txt").is_file());
    }

    #[test]
    fn links_inside_a_session_folder_are_counted_but_not_followed() {
        let data = Data::new("trash-lstat");
        let outside = data.dir.path().join("big");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("large.bin"), vec![0u8; 64 * 1024]).unwrap();
        let sidecar = data.claude_project().join(CLAUDE_MAIN);
        let before = trash_preview(&data.options(), &[claude(CLAUDE_MAIN)], later());
        if link_dir(&sidecar.join("linked"), &outside).is_none() {
            eprintln!("skipping: this platform refused to create a directory link");
            return;
        }
        let after = trash_preview(&data.options(), &[claude(CLAUDE_MAIN)], later());
        assert_eq!(reason(&after, 0), None);
        assert_eq!(after.items[0].file_count, before.items[0].file_count + 1);
        assert!(after.items[0].total_bytes < before.items[0].total_bytes + 64 * 1024);
    }

    #[test]
    fn recently_modified_sessions_are_blocked_in_preview_and_before_moving() {
        let data = Data::new("trash-recent");
        let now = SystemTime::now();
        let old = now - Duration::from_secs(60 * 60);
        let transcript = data.claude_project().join(format!("{CLAUDE_MAIN}.jsonl"));
        let subagent = data
            .claude_project()
            .join(CLAUDE_MAIN)
            .join("subagents")
            .join("agent-a2.jsonl");
        set_mtime(&transcript, old);
        set_mtime_all(&data.claude_project().join(CLAUDE_MAIN), old);
        set_mtime(&subagent, now - Duration::from_secs(5 * 60));

        let preview = trash_preview(&data.options(), &[claude(CLAUDE_MAIN)], now);
        assert_eq!(reason(&preview, 0), Some(TrashBlockReason::RecentlyUpdated));
        assert_eq!(preview.items[0].file_count, 4);
        assert_eq!(preview.file_count, 0);

        let future = trash_preview(&data.options(), &[claude(CLAUDE_MAIN)], old);
        assert_eq!(reason(&future, 0), Some(TrashBlockReason::RecentlyUpdated));

        set_mtime(&subagent, old);
        assert_eq!(
            reason(
                &trash_preview(&data.options(), &[claude(CLAUDE_MAIN)], now),
                0
            ),
            None
        );
        set_mtime(&subagent, now - Duration::from_secs(60));
        let backend = FakeTrash::failing();
        let clock = || now;
        let result = trash_sessions(&data.options(), &[claude(CLAUDE_MAIN)], &clock, &backend);
        assert_eq!(result.blocked[0].reason, TrashBlockReason::RecentlyUpdated);
        assert!(result.trashed.is_empty());
        assert!(backend.calls.borrow().is_empty());
    }

    #[test]
    fn trash_moves_every_owned_path_and_invalidates_the_cache() {
        let data = Data::new("trash-move");
        let opts = data.options();
        let first = scan(&opts);
        let file_count = first.scan.file_count;
        let transcript = data.claude_project().join(format!("{CLAUDE_MAIN}.jsonl"));
        let key = transcript.to_string_lossy().into_owned();
        assert!(
            load_cache(opts.cache_path.as_deref().unwrap(), DEFAULT_MAX_LINE_BYTES)
                .contains_key(&key)
        );

        let backend = FakeTrash::moving(data.dir.path().join("bin"));
        let result = trash_sessions(
            &opts,
            &[claude(CLAUDE_MAIN), claude("absent")],
            &later,
            &backend,
        );
        assert_eq!(result.trashed, vec![claude(CLAUDE_MAIN)]);
        assert_eq!(result.blocked[0].reason, TrashBlockReason::NotFound);
        assert!(result.failed.is_empty());
        assert_eq!(
            *backend.calls.borrow(),
            vec![vec![
                data.claude_project().join(CLAUDE_MAIN),
                transcript.clone()
            ]]
        );
        assert!(!transcript.exists());

        let cache = load_cache(opts.cache_path.as_deref().unwrap(), DEFAULT_MAX_LINE_BYTES);
        assert!(!cache.contains_key(&key));
        assert_eq!(cache.len() as u32, file_count - 1);
        let next = scan(&opts);
        assert!(next.sessions.iter().all(|s| s.id != CLAUDE_MAIN));
        assert_eq!(next.scan.parsed_count, 0);
    }

    #[test]
    fn trash_failure_is_reported_and_never_deletes() {
        let data = Data::new("trash-fail");
        let opts = data.options();
        scan(&opts);
        let backend = FakeTrash::failing();
        let result = trash_sessions(&opts, &[codex(CODEX_PAGED)], &later, &backend);
        assert!(result.trashed.is_empty());
        assert_eq!(result.failed.len(), 1);
        assert_eq!(result.failed[0].session, codex(CODEX_PAGED));
        assert!(result.failed[0].error.contains("trash unavailable"));
        assert_eq!(backend.calls.borrow().len(), 1);
        for path in &owned_index(&opts)[&(SessionAgent::Codex, CODEX_PAGED.to_string())] {
            assert!(path.is_file(), "{path:?} must survive a failed trash");
        }
        assert_eq!(
            load_cache(opts.cache_path.as_deref().unwrap(), DEFAULT_MAX_LINE_BYTES).len() as u32,
            scan(&opts).scan.file_count
        );
    }

    #[test]
    fn owned_index_reuses_a_cache_written_under_another_line_limit() {
        let data = Data::new("trash-cache-limit");
        let custom = ScanOptions {
            max_line_bytes: 2 * MIN_MAX_LINE_BYTES,
            ..data.options()
        };
        scan(&custom);
        let cache_path = custom.cache_path.clone().unwrap();
        let written = fs::read(&cache_path).unwrap();
        let preview = trash_preview(&data.options(), &[claude(CLAUDE_BARE)], later());
        assert_eq!(reason(&preview, 0), None);
        assert_eq!(fs::read(&cache_path).unwrap(), written);
    }

    #[test]
    fn identifying_files_move_last_so_a_partial_failure_stays_listed() {
        let data = Data::new("trash-order");
        let opts = data.options();
        let index = owned_index(&opts);
        let codex: Vec<String> = index[&(SessionAgent::Codex, CODEX_PAGED.to_string())]
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        let (descendants, pages) = codex.split_at(codex.len() - 2);
        assert_eq!(
            pages,
            [
                format!("rollout-2026-10-03T10-00-00-{CODEX_PAGED}.jsonl"),
                format!("rollout-2026-10-07T09-00-00-{CODEX_PAGED}_0aaaaaaa-0000-7000-8000-0000000000f1.jsonl"),
            ]
        );
        assert!(descendants.iter().all(|name| !name.contains(CODEX_PAGED)));

        let transcript = data.claude_project().join(format!("{CLAUDE_MAIN}.jsonl"));
        let sidecar = data.claude_project().join(CLAUDE_MAIN);
        let backend = FakeTrash::moving_at_most(data.dir.path().join("bin"), 1);
        let result = trash_sessions(&opts, &[claude(CLAUDE_MAIN)], &later, &backend);
        assert!(result.trashed.is_empty());
        assert!(
            result.failed[0].error.starts_with("1 of 2 paths"),
            "{}",
            result.failed[0].error
        );
        assert_eq!(
            backend.moved_paths(),
            vec![sidecar.clone(), transcript.clone()]
        );
        assert!(!sidecar.exists());
        assert!(transcript.is_file());
        assert!(scan(&opts).sessions.iter().any(|s| s.id == CLAUDE_MAIN));

        let retry = FakeTrash::moving(data.dir.path().join("bin-retry"));
        let result = trash_sessions(&opts, &[claude(CLAUDE_MAIN)], &later, &retry);
        assert_eq!(result.trashed, vec![claude(CLAUDE_MAIN)]);
        assert_eq!(retry.moved_paths(), vec![transcript.clone()]);
        assert!(scan(&opts).sessions.iter().all(|s| s.id != CLAUDE_MAIN));
    }

    #[test]
    fn a_stop_at_any_subagent_depth_leaves_the_rest_reachable_for_a_retry() {
        for stop_at in [CODEX_SPAWNED, CODEX_GRANDCHILD] {
            let data = Data::new("trash-codex-depth");
            let grandchild = data.add_archived_grandchild();
            let opts = data.options();
            let owned = owned_index(&opts)[&(SessionAgent::Codex, CODEX_PAGED.to_string())].clone();
            let position = |id: &str| {
                owned
                    .iter()
                    .position(|p| p.to_string_lossy().ends_with(&format!("{id}.jsonl")))
                    .unwrap()
            };
            assert!(position(CODEX_GRANDCHILD) < position(CODEX_SPAWNED));
            assert!(owned[owned.len() - 2..]
                .iter()
                .all(|p| p.to_string_lossy().contains(CODEX_PAGED)));
            let middle = owned[position(CODEX_SPAWNED)].clone();

            let locked = owned[position(stop_at)].clone();
            let first = FakeTrash::stopping_at(data.dir.path().join("bin"), locked.clone());
            let result = trash_sessions(&opts, &[codex(CODEX_PAGED)], &later, &first);
            assert!(
                result.failed[0].error.contains("file is locked"),
                "{stop_at}"
            );
            assert!(locked.is_file() && middle.is_file(), "{stop_at}");
            assert_eq!(grandchild.exists(), stop_at == CODEX_GRANDCHILD);
            assert!(scan(&opts).sessions.iter().any(|s| s.id == CODEX_PAGED));

            let retry = FakeTrash::moving(data.dir.path().join("bin-retry"));
            let result = trash_sessions(&opts, &[codex(CODEX_PAGED)], &later, &retry);
            assert_eq!(result.trashed, vec![codex(CODEX_PAGED)], "{stop_at}");
            assert!(retry.moved_paths().contains(&locked), "{stop_at}");
            assert!(owned.iter().all(|p| !p.exists()), "{stop_at}");
            assert!(!grandchild.exists());
        }
    }

    #[test]
    fn duplicate_claude_ids_in_two_projects_are_all_owned_and_trashed() {
        let data = Data::new("trash-claude-dup");
        let opts = data.options();
        let other = data.claude_home().join("projects").join("D--work-copy");
        fs::create_dir_all(other.join(CLAUDE_BARE)).unwrap();
        let original = data.claude_project().join(format!("{CLAUDE_BARE}.jsonl"));
        let copy = other.join(format!("{CLAUDE_BARE}.jsonl"));
        fs::copy(&original, &copy).unwrap();
        fs::write(other.join(CLAUDE_BARE).join("note.txt"), "x").unwrap();

        let preview = trash_preview(&opts, &[claude(CLAUDE_BARE)], later());
        assert_eq!(reason(&preview, 0), None);
        assert_eq!(preview.items[0].file_count, 3);

        let backend = FakeTrash::moving(data.dir.path().join("bin"));
        let result = trash_sessions(&opts, &[claude(CLAUDE_BARE)], &later, &backend);
        assert_eq!(result.trashed, vec![claude(CLAUDE_BARE)]);
        let moved = backend.moved_paths();
        assert_eq!(moved.len(), 3);
        assert_eq!(moved[0], other.join(CLAUDE_BARE));
        assert!(moved[1..].contains(&original) && moved[1..].contains(&copy));
        assert!(!original.exists() && !copy.exists());
        assert!(scan(&opts).sessions.iter().all(|s| s.id != CLAUDE_BARE));
    }

    #[test]
    fn success_with_files_left_in_place_is_reported_as_failed() {
        struct ClaimsSuccess;
        impl TrashBackend for ClaimsSuccess {
            fn move_to_trash(&self, _paths: &[PathBuf]) -> Result<(), String> {
                Ok(())
            }
        }
        let data = Data::new("trash-lying");
        let result = trash_sessions(
            &data.options(),
            &[claude(CLAUDE_BARE)],
            &later,
            &ClaimsSuccess,
        );
        assert!(result.trashed.is_empty());
        assert!(result.failed[0].error.contains("still in place"));
        assert!(data
            .claude_project()
            .join(format!("{CLAUDE_BARE}.jsonl"))
            .is_file());
    }

    #[test]
    fn json_shapes_match_the_contract() {
        let data = Data::new("trash-json");
        let preview = trash_preview(
            &data.options(),
            &[claude(CLAUDE_BARE), codex("missing")],
            later(),
        );
        let value = serde_json::to_value(&preview).unwrap();
        let keys = |v: &serde_json::Value| {
            let mut k: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
            k.sort();
            k
        };
        assert_eq!(
            keys(&value),
            ["blockedCount", "fileCount", "items", "totalBytes"]
        );
        assert_eq!(
            keys(&value["items"][0]),
            [
                "agent",
                "blockedReason",
                "fileCount",
                "id",
                "newestModifiedAt",
                "totalBytes"
            ]
        );
        assert_eq!(value["items"][0]["agent"], "claude");
        assert!(value["items"][0]["blockedReason"].is_null());
        assert!(
            time::parse_iso_millis(value["items"][0]["newestModifiedAt"].as_str().unwrap())
                .is_some()
        );
        assert_eq!(value["items"][1]["blockedReason"], "not-found");
        assert!(value["items"][1]["newestModifiedAt"].is_null());

        let result = SessionTrashResult {
            trashed: vec![claude("a")],
            blocked: vec![BlockedSession {
                session: codex("b"),
                reason: TrashBlockReason::RecentlyUpdated,
            }],
            failed: vec![FailedSession {
                session: claude("c"),
                error: "e".into(),
            }],
        };
        assert_eq!(
            serde_json::to_value(&result).unwrap(),
            serde_json::json!({
                "trashed": [{"agent": "claude", "id": "a"}],
                "blocked": [{"agent": "codex", "id": "b", "reason": "recently-updated"}],
                "failed": [{"agent": "claude", "id": "c", "error": "e"}],
            })
        );
        assert_eq!(
            serde_json::to_value(TrashBlockReason::OutsideDataDir).unwrap(),
            "outside-data-dir"
        );
    }
}
