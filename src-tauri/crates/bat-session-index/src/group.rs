use std::collections::{HashMap, HashSet};

use crate::dir_probe::DirState;
use crate::git::DirRepo;
use crate::model::SessionRecord;
use crate::paths::{display_name, normalize_path, PathStyle};
use crate::time::time_value;
use crate::{SessionGroupKind, SessionHistoryEntry, SessionHistoryGroup};

const UNKNOWN_KEY: &str = "unknown";
const UNKNOWN_NAME: &str = "(unknown)";

pub(crate) struct GroupContext<'a> {
    pub style: PathStyle,
    pub repo_of: &'a dyn Fn(&str) -> Option<DirRepo>,
    pub dir_state: &'a dyn Fn(&str) -> DirState,
}

struct GroupBuilder {
    group: SessionHistoryGroup,
    first_index: usize,
    latest_value: i64,
    worktrees: HashSet<String>,
}

/// Sessions must already be sorted newest first; a repository and its linked worktrees share one group.
pub(crate) fn group_sessions(
    sessions: Vec<SessionRecord>,
    ctx: &GroupContext<'_>,
) -> (Vec<SessionHistoryEntry>, Vec<SessionHistoryGroup>) {
    let mut builders: HashMap<String, GroupBuilder> = HashMap::new();
    let mut entries = Vec::with_capacity(sessions.len());
    for (index, record) in sessions.into_iter().enumerate() {
        let state = record.cwd.as_deref().map(ctx.dir_state);
        let repo = record.cwd.as_deref().and_then(ctx.repo_of);
        let (key, kind, root) = match (&record.cwd, &repo) {
            (None, _) => (UNKNOWN_KEY.to_string(), SessionGroupKind::Unknown, None),
            (Some(_), Some(found)) => (
                format!("repo:{}", found.repo.key),
                SessionGroupKind::Repo,
                Some(found.repo.root.clone()),
            ),
            (Some(cwd), None) => {
                let kind = if state == Some(DirState::Missing) {
                    SessionGroupKind::Missing
                } else {
                    SessionGroupKind::Dir
                };
                (
                    format!("dir:{}", normalize_path(cwd, ctx.style)),
                    kind,
                    Some(cwd.clone()),
                )
            }
        };
        let builder = builders.entry(key.clone()).or_insert_with(|| GroupBuilder {
            group: SessionHistoryGroup {
                key: key.clone(),
                kind,
                name: root
                    .as_deref()
                    .map_or_else(|| UNKNOWN_NAME.to_string(), display_name),
                normalized_root_path: root.as_deref().map(|r| normalize_path(r, ctx.style)),
                root_path: root,
                worktree_count: 0,
                session_count: 0,
                latest_updated_at: None,
            },
            first_index: index,
            latest_value: i64::MIN,
            worktrees: HashSet::new(),
        });
        builder.group.session_count += 1;
        let updated = time_value(record.updated_at.as_deref());
        if updated > builder.latest_value {
            builder.latest_value = updated;
            builder.group.latest_updated_at = record.updated_at.clone();
        }
        if let Some(worktree) = repo.and_then(|found| found.worktree_key) {
            if Some(&worktree) != builder.group.normalized_root_path.as_ref() {
                builder.worktrees.insert(worktree);
            }
        }
        entries.push(SessionHistoryEntry::from_record(
            record,
            key,
            state == Some(DirState::Exists),
        ));
    }

    let mut builders: Vec<GroupBuilder> = builders.into_values().collect();
    builders.sort_by(|a, b| {
        kind_rank(a.group.kind)
            .cmp(&kind_rank(b.group.kind))
            .then_with(|| b.latest_value.cmp(&a.latest_value))
            .then_with(|| a.first_index.cmp(&b.first_index))
    });
    let groups = builders
        .into_iter()
        .map(|mut builder| {
            builder.group.worktree_count = builder.worktrees.len() as u32;
            builder.group
        })
        .collect();
    (entries, groups)
}

fn kind_rank(kind: SessionGroupKind) -> u8 {
    match kind {
        SessionGroupKind::Repo | SessionGroupKind::Dir => 0,
        SessionGroupKind::Missing => 1,
        SessionGroupKind::Unknown => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::RepoInfo;
    use crate::model::UNTITLED;
    use crate::SessionAgent;

    fn session(id: &str, cwd: Option<&str>, updated_at: &str) -> SessionRecord {
        SessionRecord {
            agent: SessionAgent::Claude,
            id: id.into(),
            title: UNTITLED.into(),
            first_prompt: None,
            last_reply: None,
            message_count: 1,
            subagent_count: 0,
            model: None,
            git_branch: None,
            cwd: cwd.map(str::to_string),
            started_at: None,
            updated_at: Some(updated_at.into()),
            file_path: format!("{id}.jsonl"),
            interactive: true,
            is_subagent: false,
            parent_id: None,
            skipped_lines: 0,
            archived: false,
            owned_paths: Vec::new(),
        }
    }

    fn repo_of(dir: &str) -> Option<DirRepo> {
        let key = normalize_path(dir, PathStyle::Windows);
        let worktree_key = match key.as_str() {
            "d:\\code\\app" | "d:\\code\\app\\src" => "d:\\code\\app",
            "d:\\code\\app\\.bat-worktrees\\fix" => "d:\\code\\app\\.bat-worktrees\\fix",
            "c:\\worktrees\\app-fix" => "c:\\worktrees\\app-fix",
            _ => return None,
        };
        Some(DirRepo {
            repo: RepoInfo {
                key: "d:\\code\\app\\.git".into(),
                root: "D:\\code\\app".into(),
                name: "app".into(),
            },
            worktree_key: Some(worktree_key.into()),
        })
    }

    fn dir_state(dir: &str) -> DirState {
        if dir.to_lowercase().contains("gone") {
            DirState::Missing
        } else {
            DirState::Exists
        }
    }

    #[test]
    fn groups_repo_worktrees_and_orders_missing_and_unknown_last() {
        let sessions = vec![
            session("no-cwd", None, "2026-10-10T00:00:00Z"),
            session("gone", Some("D:\\code\\gone"), "2026-10-09T00:00:00Z"),
            session("other", Some("D:\\code\\other"), "2026-10-05T00:00:00Z"),
            session(
                "linked",
                Some("C:\\worktrees\\app-fix"),
                "2026-10-04T00:00:00Z",
            ),
            session(
                "bat-wt",
                Some("D:\\code\\app\\.bat-worktrees\\fix"),
                "2026-10-03T00:00:00Z",
            ),
            session("nested", Some("d:/code/app/src"), "2026-10-02T00:00:00Z"),
            session("root", Some("D:\\code\\app"), "2026-10-01T00:00:00Z"),
            session(
                "other-older",
                Some("d:/CODE/other/"),
                "2026-09-01T00:00:00Z",
            ),
        ];
        let ctx = GroupContext {
            style: PathStyle::Windows,
            repo_of: &repo_of,
            dir_state: &dir_state,
        };
        let (entries, groups) = group_sessions(sessions, &ctx);
        let summary: Vec<(&str, SessionGroupKind, u32, u32)> = groups
            .iter()
            .map(|g| (g.name.as_str(), g.kind, g.session_count, g.worktree_count))
            .collect();
        assert_eq!(
            summary,
            vec![
                ("other", SessionGroupKind::Dir, 2, 0),
                ("app", SessionGroupKind::Repo, 4, 2),
                ("gone", SessionGroupKind::Missing, 1, 0),
                ("(unknown)", SessionGroupKind::Unknown, 1, 0),
            ]
        );
        let app = &groups[1];
        assert_eq!(app.key, "repo:d:\\code\\app\\.git");
        assert_eq!(app.root_path.as_deref(), Some("D:\\code\\app"));
        assert_eq!(app.normalized_root_path.as_deref(), Some("d:\\code\\app"));
        assert_eq!(
            app.latest_updated_at.as_deref(),
            Some("2026-10-04T00:00:00Z")
        );
        assert_eq!(groups[0].key, "dir:d:\\code\\other");
        assert_eq!(groups[3].root_path, None);
        assert_eq!(groups[3].normalized_root_path, None);

        let by_id: HashMap<&str, &SessionHistoryEntry> =
            entries.iter().map(|e| (e.id.as_str(), e)).collect();
        assert_eq!(by_id["nested"].group_key, app.key);
        assert!(by_id["nested"].cwd_exists);
        assert!(!by_id["gone"].cwd_exists);
        assert!(!by_id["no-cwd"].cwd_exists);
        assert_eq!(by_id["no-cwd"].group_key, "unknown");
    }

    #[test]
    fn unverified_dir_is_a_plain_dir_group_not_missing() {
        let ctx = GroupContext {
            style: PathStyle::Windows,
            repo_of: &|_| None,
            dir_state: &|_| DirState::Unknown,
        };
        let sessions = vec![session(
            "offline",
            Some("\\\\server\\share\\proj"),
            "2026-10-01T00:00:00Z",
        )];
        let (entries, groups) = group_sessions(sessions, &ctx);
        assert_eq!(groups[0].kind, SessionGroupKind::Dir);
        assert_eq!(
            groups[0].root_path.as_deref(),
            Some("\\\\server\\share\\proj")
        );
        assert!(!entries[0].cwd_exists);
    }
}
