use std::collections::{HashMap, HashSet};

use crate::model::{SessionRecord, UNTITLED};
use crate::time::time_value;
use crate::SessionAgent;

/// Returns interactive, non-subagent sessions, newest first. Codex thread names are applied here rather than
/// cached, so a rename needs no reparse.
pub(crate) fn finalize_sessions(
    records: Vec<SessionRecord>,
    codex_titles: &HashMap<String, String>,
) -> Vec<SessionRecord> {
    let sessions = merge_pages(records);
    let mut children: HashMap<&str, Vec<&str>> = HashMap::new();
    for record in &sessions {
        if record.agent == SessionAgent::Codex {
            if let Some(parent) = &record.parent_id {
                children
                    .entry(parent.as_str())
                    .or_default()
                    .push(record.id.as_str());
            }
        }
    }
    let descendant_counts: HashMap<String, u32> = sessions
        .iter()
        .filter(|r| r.agent == SessionAgent::Codex && !r.is_subagent && r.interactive)
        .map(|r| (r.id.clone(), count_descendants(&r.id, &children)))
        .collect();

    let mut visible: Vec<SessionRecord> = sessions
        .into_iter()
        .filter(|record| !record.is_subagent && record.interactive)
        .map(|mut record| {
            if record.agent == SessionAgent::Codex {
                record.title = codex_titles
                    .get(&record.id)
                    .cloned()
                    .or_else(|| record.first_prompt.clone())
                    .unwrap_or_else(|| UNTITLED.to_string());
                record.subagent_count = descendant_counts.get(&record.id).copied().unwrap_or(0);
            }
            record
        })
        .collect();
    visible.sort_by(compare_by_updated_desc);
    visible
}

/// Ties fall back to agent and id so the order does not depend on which worker parsed a file first.
pub(crate) fn compare_by_updated_desc(a: &SessionRecord, b: &SessionRecord) -> std::cmp::Ordering {
    time_value(b.updated_at.as_deref())
        .cmp(&time_value(a.updated_at.as_deref()))
        .then_with(|| a.agent.cmp(&b.agent))
        .then_with(|| a.id.cmp(&b.id))
}

/// Codex paginated history splits one thread across `rollout-…-<id>.jsonl` and `rollout-…-<id>_<page>.jsonl`;
/// every page repeats the thread id in session_meta and holds only turns after the previous page.
pub(crate) fn merge_pages(records: Vec<SessionRecord>) -> Vec<SessionRecord> {
    let mut order: Vec<(SessionAgent, String)> = Vec::new();
    let mut by_key: HashMap<(SessionAgent, String), Vec<SessionRecord>> = HashMap::new();
    for record in records {
        let key = (record.agent, record.id.clone());
        if !by_key.contains_key(&key) {
            order.push(key.clone());
        }
        by_key.entry(key).or_default().push(record);
    }
    order
        .into_iter()
        .filter_map(|key| by_key.remove(&key))
        .map(|pages| {
            if pages.len() == 1 {
                pages.into_iter().next().unwrap()
            } else {
                merge_ordered_pages(pages)
            }
        })
        .collect()
}

fn merge_ordered_pages(mut pages: Vec<SessionRecord>) -> SessionRecord {
    pages.sort_by(|a, b| {
        time_value(a.started_at.as_deref())
            .cmp(&time_value(b.started_at.as_deref()))
            .then_with(|| a.file_path.cmp(&b.file_path))
    });
    let oldest_with =
        |pick: fn(&SessionRecord) -> &Option<String>| pages.iter().find_map(|p| pick(p).clone());
    let newest_with = |pick: fn(&SessionRecord) -> &Option<String>| {
        pages.iter().rev().find_map(|p| pick(p).clone())
    };
    let first_prompt = oldest_with(|p| &p.first_prompt);
    let git_branch = oldest_with(|p| &p.git_branch);
    let cwd = oldest_with(|p| &p.cwd);
    let last_reply = newest_with(|p| &p.last_reply);
    let model = newest_with(|p| &p.model);
    let updated_at = newest_with(|p| &p.updated_at);
    let file_path = pages
        .last()
        .map(|p| p.file_path.clone())
        .unwrap_or_default();
    let message_count = pages.iter().map(|p| p.message_count).sum();
    let skipped_lines = pages.iter().map(|p| p.skipped_lines).sum();
    let first = pages.swap_remove(0);
    SessionRecord {
        first_prompt,
        last_reply,
        model,
        git_branch,
        cwd,
        updated_at,
        file_path,
        message_count,
        skipped_lines,
        ..first
    }
}

/// Subagents can spawn their own; the seen set guards against a parent cycle in corrupt data.
fn count_descendants(id: &str, children: &HashMap<&str, Vec<&str>>) -> u32 {
    let mut seen: HashSet<&str> = HashSet::from([id]);
    let mut stack: Vec<&str> = children.get(id).cloned().unwrap_or_default();
    let mut count = 0;
    while let Some(child) = stack.pop() {
        if !seen.insert(child) {
            continue;
        }
        count += 1;
        if let Some(grandchildren) = children.get(child) {
            stack.extend(grandchildren);
        }
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;

    fn codex(id: &str, parent: Option<&str>, interactive: bool) -> SessionRecord {
        SessionRecord {
            agent: SessionAgent::Codex,
            id: id.into(),
            title: UNTITLED.into(),
            first_prompt: None,
            last_reply: None,
            message_count: 1,
            subagent_count: 0,
            model: None,
            git_branch: None,
            cwd: None,
            started_at: None,
            updated_at: None,
            file_path: format!("{id}.jsonl"),
            interactive,
            is_subagent: parent.is_some(),
            parent_id: parent.map(str::to_string),
            skipped_lines: 0,
        }
    }

    #[test]
    fn counts_all_descendants_and_survives_cycles() {
        let records = vec![
            codex("root", None, true),
            codex("child", Some("root"), false),
            codex("grandchild", Some("child"), false),
            codex("great", Some("grandchild"), false),
            codex("loop-a", Some("loop-b"), false),
            codex("loop-b", Some("loop-a"), false),
            codex("other", None, true),
            codex("exec", None, false),
        ];
        let visible = finalize_sessions(records, &HashMap::new());
        let counts: Vec<(&str, u32)> = visible
            .iter()
            .map(|r| (r.id.as_str(), r.subagent_count))
            .collect();
        assert_eq!(counts, vec![("other", 0), ("root", 3)]);
    }

    #[test]
    fn merged_pages_take_oldest_prompt_and_newest_reply() {
        let mut first = codex("t", None, true);
        first.started_at = Some("2026-10-03T10:00:00Z".into());
        first.updated_at = Some("2026-10-03T11:00:00Z".into());
        first.first_prompt = Some("first".into());
        first.last_reply = Some("old reply".into());
        first.git_branch = Some("main".into());
        let mut later = codex("t", None, true);
        later.file_path = "t_page.jsonl".into();
        later.started_at = Some("2026-10-07T09:00:00Z".into());
        later.updated_at = Some("2026-10-07T09:01:00Z".into());
        later.first_prompt = Some("later".into());
        later.last_reply = Some("new reply".into());
        later.git_branch = Some("other".into());
        let merged = merge_pages(vec![later, first]);
        assert_eq!(merged.len(), 1);
        let m = &merged[0];
        assert_eq!(m.first_prompt.as_deref(), Some("first"));
        assert_eq!(m.last_reply.as_deref(), Some("new reply"));
        assert_eq!(m.git_branch.as_deref(), Some("main"));
        assert_eq!(m.started_at.as_deref(), Some("2026-10-03T10:00:00Z"));
        assert_eq!(m.updated_at.as_deref(), Some("2026-10-07T09:01:00Z"));
        assert_eq!(m.message_count, 2);
        assert_eq!(m.file_path, "t_page.jsonl");
    }
}
