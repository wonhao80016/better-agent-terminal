use std::fs;
use std::path::{Path, PathBuf};

pub(crate) const CODEX_SESSIONS_DIR: &str = "sessions";
pub(crate) const CODEX_ARCHIVED_DIR: &str = "archived_sessions";

/// Only `<projects>/<project>/<id>.jsonl` are sessions; deeper files belong to subagents or tool output.
pub(crate) fn list_claude_transcripts(claude_home: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for project in read_dir_safe(&claude_home.join("projects")) {
        if !project.is_dir {
            continue;
        }
        for entry in read_dir_safe(&project.path) {
            if entry.is_file && entry.name.ends_with(".jsonl") {
                files.push(entry.path);
            }
        }
    }
    files.sort();
    files
}

pub(crate) fn list_codex_rollouts(codex_home: &Path) -> Vec<PathBuf> {
    list_rollouts_under(codex_home.join(CODEX_SESSIONS_DIR))
}

pub(crate) fn list_codex_archived_rollouts(codex_home: &Path) -> Vec<PathBuf> {
    list_rollouts_under(codex_home.join(CODEX_ARCHIVED_DIR))
}

fn list_rollouts_under(root: PathBuf) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut pending = vec![root];
    while let Some(dir) = pending.pop() {
        for entry in read_dir_safe(&dir) {
            if entry.is_dir {
                pending.push(entry.path);
            } else if entry.is_file
                && entry.name.starts_with("rollout-")
                && entry.name.ends_with(".jsonl")
            {
                files.push(entry.path);
            }
        }
    }
    files.sort();
    files
}

struct DirEntryInfo {
    path: PathBuf,
    name: String,
    is_dir: bool,
    is_file: bool,
}

/// Symlinks are neither followed nor listed, so a link cycle cannot loop the walk.
fn read_dir_safe(dir: &Path) -> Vec<DirEntryInfo> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let file_type = entry.file_type().ok()?;
            Some(DirEntryInfo {
                name: entry.file_name().to_string_lossy().into_owned(),
                path: entry.path(),
                is_dir: file_type.is_dir(),
                is_file: file_type.is_file(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::fixture;

    #[test]
    fn lists_top_level_claude_transcripts_and_codex_rollouts() {
        let claude = list_claude_transcripts(&fixture(&["claude"]));
        assert_eq!(claude.len(), 3);
        assert!(claude
            .iter()
            .all(|p| !p.to_string_lossy().contains("subagents")));
        let codex = list_codex_rollouts(&fixture(&["codex"]));
        assert_eq!(codex.len(), 7);
        assert!(list_claude_transcripts(&fixture(&["nope"])).is_empty());
        assert!(list_codex_archived_rollouts(&fixture(&["codex"])).is_empty());
    }
}
