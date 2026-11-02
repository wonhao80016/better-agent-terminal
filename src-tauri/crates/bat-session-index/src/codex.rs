use std::collections::HashMap;
use std::io;
use std::path::Path;

use serde_json::{Map, Value};

use crate::line_reader::{for_each_line_in_file, LineEvent};
use crate::model::{SessionRecord, UNTITLED};
use crate::text::{
    extract_text, looks_injected, non_empty_str, non_empty_string, summarize, REPLY_CHARS,
    TITLE_CHARS,
};
use crate::SessionAgent;

const NON_INTERACTIVE_SOURCES: &[&str] = &["exec"];
const IDE_REQUEST_MARKER: &str = "## My request for Codex:";
const RELEVANT_MARKERS: &[&str] = &[
    "\"session_meta\"",
    "\"turn_context\"",
    "\"role\":\"user\"",
    "\"role\":\"assistant\"",
];
const USED_RECORD_TYPES: &[&str] = &["session_meta", "turn_context", "response_item"];

#[derive(Default)]
struct CodexState {
    id: Option<String>,
    cwd: Option<String>,
    git_branch: Option<String>,
    source: Option<Value>,
    top_level_parent_id: Option<String>,
    first_prompt: Option<String>,
    last_reply: Option<String>,
    model: Option<String>,
    started_at: Option<String>,
    updated_at: Option<String>,
    message_count: u32,
    skipped_lines: u32,
}

impl CodexState {
    fn note_timestamp(&mut self, record: &Map<String, Value>) {
        if let Some(timestamp) = non_empty_str(record, "timestamp") {
            self.started_at.get_or_insert_with(|| timestamp.to_string());
            self.updated_at = Some(timestamp.to_string());
        }
    }

    fn apply(&mut self, record: &Map<String, Value>, payload: &Map<String, Value>) {
        match record.get("type").and_then(Value::as_str) {
            Some("session_meta") => {
                self.note_timestamp(record);
                if self.id.is_none() {
                    self.id = non_empty_string(payload, "id")
                        .or_else(|| non_empty_string(payload, "session_id"));
                }
                if self.cwd.is_none() {
                    self.cwd = non_empty_string(payload, "cwd");
                }
                if self.source.is_none() {
                    self.source = payload
                        .get("source")
                        .filter(|value| !value.is_null())
                        .cloned();
                }
                if self.top_level_parent_id.is_none() {
                    self.top_level_parent_id = non_empty_string(payload, "parent_thread_id");
                }
                if self.git_branch.is_none() {
                    self.git_branch = payload
                        .get("git")
                        .and_then(Value::as_object)
                        .and_then(|git| non_empty_string(git, "branch"));
                }
            }
            Some("turn_context") => {
                self.note_timestamp(record);
                if let Some(model) = non_empty_string(payload, "model") {
                    self.model = Some(model);
                }
                if self.cwd.is_none() {
                    self.cwd = non_empty_string(payload, "cwd");
                }
            }
            Some("response_item") => {
                if payload.get("type").and_then(Value::as_str) != Some("message") {
                    return;
                }
                self.note_timestamp(record);
                match payload.get("role").and_then(Value::as_str) {
                    Some("user") => {
                        if let Some(prompt) = user_prompt_text(payload.get("content")) {
                            self.message_count += 1;
                            self.first_prompt
                                .get_or_insert_with(|| summarize(&prompt, TITLE_CHARS));
                        }
                    }
                    Some("assistant") => {
                        let reply = extract_text(payload.get("content"), &["output_text", "text"]);
                        if !reply.trim().is_empty() {
                            self.message_count += 1;
                            self.last_reply = Some(summarize(&reply, REPLY_CHARS));
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

/// Parses one Codex rollout (`<codexHome>/sessions/YYYY/MM/DD/rollout-*.jsonl`); None when it has no session id.
pub(crate) fn parse_codex_session(
    path: &Path,
    max_line_bytes: u64,
) -> io::Result<Option<SessionRecord>> {
    let mut state = CodexState::default();
    for_each_line_in_file(path, max_line_bytes, |event| {
        let text = match event {
            LineEvent::Skipped { head, .. } => {
                if may_hold_used_record(&head) {
                    state.skipped_lines += 1;
                }
                return;
            }
            LineEvent::Line(text) => text,
        };
        if !RELEVANT_MARKERS.iter().any(|marker| text.contains(marker)) {
            return;
        }
        let Ok(Value::Object(record)) = serde_json::from_str::<Value>(&text) else {
            return;
        };
        if let Some(payload) = record.get("payload").and_then(Value::as_object) {
            state.apply(&record, payload);
        }
    })?;

    let Some(id) = state.id else {
        return Ok(None);
    };
    let source = state.source.as_ref();
    let is_subagent = source
        .and_then(Value::as_object)
        .is_some_and(|s| s.contains_key("subagent"));
    // A top-level parent_thread_id on a regular session must not make it a child of that thread.
    let parent_id = if is_subagent {
        spawned_parent_id(source).or(state.top_level_parent_id)
    } else {
        None
    };
    let non_interactive_source = source
        .and_then(Value::as_str)
        .is_some_and(|s| NON_INTERACTIVE_SOURCES.contains(&s));
    Ok(Some(SessionRecord {
        agent: SessionAgent::Codex,
        title: state
            .first_prompt
            .clone()
            .unwrap_or_else(|| UNTITLED.to_string()),
        id,
        first_prompt: state.first_prompt,
        last_reply: state.last_reply,
        message_count: state.message_count,
        subagent_count: 0,
        model: state.model,
        git_branch: state.git_branch,
        cwd: state.cwd,
        started_at: state.started_at,
        updated_at: state.updated_at,
        file_path: path.to_string_lossy().into_owned(),
        interactive: !is_subagent && !non_interactive_source,
        is_subagent,
        parent_id,
        skipped_lines: state.skipped_lines,
    }))
}

/// Codex writes the top-level `type` before `payload` (and `payload.type` first inside it), so the head of a
/// skipped line shows whether the parser would have used it; an unrecognizable head counts as used.
pub(crate) fn may_hold_used_record(head: &str) -> bool {
    let Some(record_type) = first_quoted_value(head, "\"type\":\"") else {
        return true;
    };
    if !USED_RECORD_TYPES.contains(&record_type) {
        return false;
    }
    if record_type != "response_item" {
        return true;
    }
    first_quoted_value(head, "\"payload\":{\"type\":\"")
        .is_none_or(|payload_type| payload_type == "message")
}

/// Equivalent of the regex `<prefix>([^"]+)"`: the first prefix occurrence followed by a non-empty quoted value.
fn first_quoted_value<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    let mut search = 0;
    while let Some(found) = text[search..].find(prefix) {
        let value_start = search + found + prefix.len();
        if let Some(len) = text[value_start..].find('"') {
            if len > 0 {
                return Some(&text[value_start..value_start + len]);
            }
        }
        search = search + found + 1;
    }
    None
}

/// The VS Code extension wraps the typed request after an IDE-context preamble.
fn user_prompt_text(content: Option<&Value>) -> Option<String> {
    let text = extract_text(content, &["input_text", "text"]);
    let request = match text.find(IDE_REQUEST_MARKER) {
        Some(index) => &text[index + IDE_REQUEST_MARKER.len()..],
        None => text.as_str(),
    };
    if request.trim().is_empty()
        || looks_injected(request)
        || request.trim_start().starts_with("# AGENTS.md")
    {
        return None;
    }
    Some(request.to_string())
}

/// Spawned threads nest the parent id in source; other subagent kinds (for example review) use the top-level field.
fn spawned_parent_id(source: Option<&Value>) -> Option<String> {
    source?
        .get("subagent")?
        .get("thread_spawn")?
        .as_object()
        .and_then(|spawn| non_empty_string(spawn, "parent_thread_id"))
}

/// Later entries win, so a renamed thread shows its newest name; a missing or unreadable index yields no titles.
pub(crate) fn read_codex_title_index(path: &Path, max_line_bytes: u64) -> HashMap<String, String> {
    let mut titles = HashMap::new();
    let _ = for_each_line_in_file(path, max_line_bytes, |event| {
        let LineEvent::Line(text) = event else {
            return;
        };
        let Ok(Value::Object(entry)) = serde_json::from_str::<Value>(&text) else {
            return;
        };
        if let (Some(id), Some(name)) = (
            non_empty_string(&entry, "id"),
            non_empty_string(&entry, "thread_name"),
        ) {
            titles.insert(id, name);
        }
    });
    titles
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{fixture, TempDir};
    use crate::DEFAULT_MAX_LINE_BYTES;
    use std::fs;
    use std::path::PathBuf;

    fn rollout(day: &str, name: &str) -> PathBuf {
        fixture(&["codex", "sessions", "2026", "10", day, name])
    }

    fn parse(path: &Path) -> SessionRecord {
        parse_codex_session(path, DEFAULT_MAX_LINE_BYTES)
            .unwrap()
            .unwrap()
    }

    #[test]
    fn title_index_keeps_newest_name_and_ignores_bad_entries() {
        let titles = read_codex_title_index(
            &fixture(&["codex", "session_index.jsonl"]),
            DEFAULT_MAX_LINE_BYTES,
        );
        assert_eq!(titles.len(), 1);
        assert_eq!(
            titles["0aaaaaaa-0000-7000-8000-000000000001"],
            "Rename config loader"
        );
    }

    #[test]
    fn missing_title_index_is_empty() {
        assert!(read_codex_title_index(
            &fixture(&["codex", "missing.jsonl"]),
            DEFAULT_MAX_LINE_BYTES
        )
        .is_empty());
    }

    #[test]
    fn interactive_vscode_session_skips_injected_and_unwraps_ide_request() {
        let session = parse(&rollout(
            "03",
            "rollout-2026-10-03T10-00-00-0aaaaaaa-0000-7000-8000-000000000001.jsonl",
        ));
        assert_eq!(session.agent, SessionAgent::Codex);
        assert_eq!(session.id, "0aaaaaaa-0000-7000-8000-000000000001");
        assert_eq!(session.title, "Rename the config loader");
        assert_eq!(
            session.first_prompt.as_deref(),
            Some("Rename the config loader")
        );
        assert_eq!(
            session.last_reply.as_deref(),
            Some("Docs updated in README.")
        );
        assert_eq!(session.message_count, 4);
        assert_eq!(session.model.as_deref(), Some("gpt-5.6-sol"));
        assert_eq!(session.git_branch.as_deref(), Some("feat/config-loader"));
        assert_eq!(session.cwd.as_deref(), Some("D:\\work\\demo"));
        assert_eq!(
            session.started_at.as_deref(),
            Some("2026-10-03T10:00:00.000Z")
        );
        assert_eq!(
            session.updated_at.as_deref(),
            Some("2026-10-03T10:05:30.000Z")
        );
        assert!(session.interactive);
        assert_eq!(session.parent_id, None);
    }

    #[test]
    fn exec_session_is_non_interactive() {
        let session = parse(&rollout(
            "04",
            "rollout-2026-10-04T11-00-00-0aaaaaaa-0000-7000-8000-000000000002.jsonl",
        ));
        assert!(!session.interactive);
        assert_eq!(session.title, "Review the diff");
        assert_eq!(session.message_count, 1);
    }

    #[test]
    fn thread_spawn_subagent_records_parent() {
        let session = parse(&rollout(
            "04",
            "rollout-2026-10-04T11-30-00-0aaaaaaa-0000-7000-8000-000000000003.jsonl",
        ));
        assert!(session.is_subagent);
        assert_eq!(
            session.parent_id.as_deref(),
            Some("0aaaaaaa-0000-7000-8000-000000000001")
        );
        assert!(!session.interactive);
    }

    #[test]
    fn review_subagent_without_parent_is_non_interactive() {
        let session = parse(&rollout(
            "04",
            "rollout-2026-10-04T12-00-00-0aaaaaaa-0000-7000-8000-000000000004.jsonl",
        ));
        assert!(session.is_subagent);
        assert_eq!(session.parent_id, None);
        assert!(!session.interactive);
    }

    #[test]
    fn review_subagent_uses_top_level_parent_thread_id() {
        let session = parse(&rollout(
            "04",
            "rollout-2026-10-04T12-30-00-0aaaaaaa-0000-7000-8000-000000000005.jsonl",
        ));
        assert!(session.is_subagent);
        assert_eq!(
            session.parent_id.as_deref(),
            Some("0aaaaaaa-0000-7000-8000-000000000001")
        );
    }

    #[test]
    fn regular_session_with_top_level_parent_thread_id_is_not_a_child() {
        let dir = TempDir::new("codex-regular");
        let path = dir.path().join("rollout-regular.jsonl");
        let meta = serde_json::json!({
            "timestamp": "2026-10-08T00:00:00.000Z", "type": "session_meta",
            "payload": {"id": "0aaaaaaa-0000-7000-8000-000000000006", "cwd": "D:\\work\\demo", "source": "vscode",
                        "parent_thread_id": "0aaaaaaa-0000-7000-8000-000000000001"}
        });
        fs::write(&path, format!("{meta}\n")).unwrap();
        let session = parse(&path);
        assert!(!session.is_subagent);
        assert_eq!(session.parent_id, None);
        assert!(session.interactive);
    }

    #[test]
    fn oversized_lines_count_only_when_head_shows_a_used_record() {
        assert!(!may_hold_used_record(
            "{\"timestamp\":\"t\",\"type\":\"event_msg\",\"payload\":{\"type\":\"item_completed\""
        ));
        assert!(!may_hold_used_record(
            "{\"timestamp\":\"t\",\"type\":\"response_item\",\"payload\":{\"type\":\"function_call_output\",\"call_id\""
        ));
        assert!(!may_hold_used_record(
            "{\"timestamp\":\"t\",\"type\":\"compacted\",\"payload\":{"
        ));
        assert!(may_hold_used_record(
            "{\"timestamp\":\"t\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\""
        ));
        assert!(may_hold_used_record(
            "{\"timestamp\":\"t\",\"type\":\"response_item\",\"payload\":{\"role\":\"user\",\"type\":\"message\""
        ));
        assert!(may_hold_used_record(
            "{\"timestamp\":\"t\",\"type\":\"session_meta\",\"payload\":{"
        ));
        assert!(may_hold_used_record("{\"timestamp\":\"t\",\"payload\":"));

        let dir = TempDir::new("codex-oversized");
        let path = dir.path().join("rollout-oversized.jsonl");
        let padding = "p".repeat(2_000);
        // Literal lines: key order matters to the head check, and json! may reorder keys.
        let lines = [
            r#"{"timestamp":"2026-10-08T00:00:00.000Z","type":"session_meta","payload":{"id":"0aaaaaaa-0000-7000-8000-000000000007","cwd":"D:\\work\\demo","source":"vscode"}}"#.to_string(),
            format!(r#"{{"timestamp":"2026-10-08T00:00:01.000Z","type":"event_msg","payload":{{"type":"item_completed","image":"{padding}"}}}}"#),
            format!(r#"{{"timestamp":"2026-10-08T00:00:02.000Z","type":"response_item","payload":{{"type":"function_call_output","output":"{padding}"}}}}"#),
            format!(r#"{{"timestamp":"2026-10-08T00:00:03.000Z","type":"response_item","payload":{{"type":"message","role":"user","content":[{{"type":"input_text","text":"{padding}"}}]}}}}"#),
        ];
        fs::write(&path, lines.join("\n") + "\n").unwrap();
        let session = parse_codex_session(&path, 1_000).unwrap().unwrap();
        assert_eq!(session.skipped_lines, 1);
    }

    #[test]
    fn rollout_without_session_meta_is_rejected() {
        let path = rollout("05", "rollout-2026-10-05T12-00-00-no-meta.jsonl");
        assert_eq!(
            parse_codex_session(&path, DEFAULT_MAX_LINE_BYTES).unwrap(),
            None
        );
    }
}
