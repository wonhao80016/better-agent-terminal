use std::fs;
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

const NON_INTERACTIVE_ENTRYPOINTS: &[&str] = &["sdk-cli"];
const RELEVANT_TYPES: &[&str] = &[
    "\"type\":\"user\"",
    "\"type\":\"assistant\"",
    "\"type\":\"ai-title\"",
];
const INTERRUPTED_FOR_TOOL_USE: &str = "[Request interrupted by user for tool use]";
const COMPACT_SUMMARY_PREAMBLE: &str =
    "this session is being continued from a previous conversation";
const TASK_NOTIFICATION_OPEN: &str = "<task-notification>";
const TASK_NOTIFICATION_CLOSE: &str = "</task-notification>";

/// Parses one top-level Claude Code transcript (`<projects>/<encoded-cwd>/<sessionId>.jsonl`).
pub(crate) fn parse_claude_session(path: &Path, max_line_bytes: u64) -> io::Result<SessionRecord> {
    let id = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut ai_title: Option<String> = None;
    let mut first_prompt: Option<String> = None;
    let mut last_reply: Option<String> = None;
    let mut model: Option<String> = None;
    let mut git_branch: Option<String> = None;
    let mut cwd: Option<String> = None;
    let mut entrypoint: Option<String> = None;
    let mut started_at: Option<String> = None;
    let mut updated_at: Option<String> = None;
    let mut message_count = 0u32;
    let mut skipped_lines = 0u32;

    for_each_line_in_file(path, max_line_bytes, |event| {
        let text = match event {
            // Assistant records put `type` after `message`, so a skipped line's head cannot tell whether it mattered.
            LineEvent::Skipped { .. } => {
                skipped_lines += 1;
                return;
            }
            LineEvent::Line(text) => text,
        };
        if !RELEVANT_TYPES.iter().any(|marker| text.contains(marker)) {
            return;
        }
        let Ok(Value::Object(record)) = serde_json::from_str::<Value>(&text) else {
            return;
        };
        let record_type = record.get("type").and_then(Value::as_str);
        if record_type == Some("ai-title") {
            if let Some(title) = non_empty_string(&record, "aiTitle") {
                ai_title = Some(title);
            }
            return;
        }
        let is_user = match record_type {
            Some("user") => true,
            Some("assistant") => false,
            _ => return,
        };

        if let Some(timestamp) = non_empty_str(&record, "timestamp") {
            started_at.get_or_insert_with(|| timestamp.to_string());
            updated_at = Some(timestamp.to_string());
        }
        if cwd.is_none() {
            cwd = non_empty_string(&record, "cwd");
        }
        if let Some(branch) = non_empty_string(&record, "gitBranch") {
            git_branch = Some(branch);
        }
        if entrypoint.is_none() {
            entrypoint = non_empty_string(&record, "entrypoint");
        }

        let message = record.get("message").and_then(Value::as_object);
        if is_user {
            if let Some(prompt) = user_prompt_text(&record, message) {
                message_count += 1;
                first_prompt.get_or_insert_with(|| summarize(&prompt, TITLE_CHARS));
            }
            return;
        }

        let reply = extract_text(message.and_then(|m| m.get("content")), &["text"]);
        if !reply.trim().is_empty() {
            message_count += 1;
            last_reply = Some(summarize(&reply, REPLY_CHARS));
        }
        if let Some(reply_model) = message.and_then(|m| non_empty_str(m, "model")) {
            if !reply_model.starts_with('<') {
                model = Some(reply_model.to_string());
            }
        }
    })?;

    let interactive = entrypoint
        .as_deref()
        .is_none_or(|value| !NON_INTERACTIVE_ENTRYPOINTS.contains(&value));
    Ok(SessionRecord {
        agent: SessionAgent::Claude,
        title: ai_title
            .or_else(|| first_prompt.clone())
            .unwrap_or_else(|| UNTITLED.to_string()),
        subagent_count: count_subagents(path),
        id,
        first_prompt,
        last_reply,
        message_count,
        model,
        git_branch,
        cwd,
        started_at,
        updated_at,
        file_path: path.to_string_lossy().into_owned(),
        interactive,
        is_subagent: false,
        parent_id: None,
        skipped_lines,
        archived: false,
        owned_paths: Vec::new(),
    })
}

/// Returns the prompt a person typed, or None for tool results, meta records, and harness-injected text.
fn user_prompt_text(
    record: &Map<String, Value>,
    message: Option<&Map<String, Value>>,
) -> Option<String> {
    let flag = |key: &str| record.get(key).and_then(Value::as_bool) == Some(true);
    if flag("isMeta")
        || flag("isSidechain")
        || record.get("promptSource").and_then(Value::as_str) == Some("system")
    {
        return None;
    }
    let text = strip_task_notifications(&extract_text(
        message.and_then(|m| m.get("content")),
        &["text"],
    ));
    if text.is_empty() || looks_injected(&text) || is_harness_noise(&text) {
        return None;
    }
    Some(text)
}

/// Matches `normalize_session_hint_text` in `src/commands/claude.rs`, so both lists hide the same prompts.
fn is_harness_noise(text: &str) -> bool {
    let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
    normalized == INTERRUPTED_FOR_TOOL_USE
        || normalized
            .get(..COMPACT_SUMMARY_PREAMBLE.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(COMPACT_SUMMARY_PREAMBLE))
}

/// An unterminated block swallows the rest of the text, matching `strip_task_notifications` in claude.rs.
fn strip_task_notifications(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(TASK_NOTIFICATION_OPEN) {
        out.push_str(&rest[..start]);
        let after_open = &rest[start + TASK_NOTIFICATION_OPEN.len()..];
        match after_open.find(TASK_NOTIFICATION_CLOSE) {
            Some(end) => rest = &after_open[end + TASK_NOTIFICATION_CLOSE.len()..],
            None => return out.trim().to_string(),
        }
    }
    out.push_str(rest);
    out.trim().to_string()
}

fn count_subagents(transcript: &Path) -> u32 {
    let Ok(entries) = fs::read_dir(transcript.with_extension("").join("subagents")) else {
        return 0;
    };
    entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().ends_with(".jsonl"))
        .count() as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{fixture, TempDir};
    use crate::DEFAULT_MAX_LINE_BYTES;

    fn demo(name: &str) -> std::path::PathBuf {
        fixture(&["claude", "projects", "D--work-demo", name])
    }

    #[test]
    fn interactive_session_title_counts_model_branch_subagents() {
        let session = parse_claude_session(
            &demo("11111111-aaaa-4bbb-8ccc-000000000001.jsonl"),
            DEFAULT_MAX_LINE_BYTES,
        )
        .unwrap();
        assert_eq!(session.agent, SessionAgent::Claude);
        assert_eq!(session.id, "11111111-aaaa-4bbb-8ccc-000000000001");
        assert_eq!(session.title, "Fetch helper retry and tests");
        assert_eq!(
            session.first_prompt.as_deref(),
            Some("Add a retry flag to the fetch helper")
        );
        assert_eq!(
            session.last_reply.as_deref(),
            Some("Tests added and passing.")
        );
        assert_eq!(session.message_count, 4);
        assert_eq!(session.model.as_deref(), Some("claude-sonnet-5-5"));
        assert_eq!(session.git_branch.as_deref(), Some("feat/retry"));
        assert_eq!(session.cwd.as_deref(), Some("D:\\work\\demo"));
        assert_eq!(
            session.started_at.as_deref(),
            Some("2026-10-01T08:00:00.000Z")
        );
        assert_eq!(
            session.updated_at.as_deref(),
            Some("2026-10-01T08:04:00.000Z")
        );
        assert_eq!(session.subagent_count, 2);
        assert!(session.interactive);
        assert_eq!(session.skipped_lines, 0);
    }

    #[test]
    fn print_mode_session_is_non_interactive_and_titled_by_first_prompt() {
        let session = parse_claude_session(
            &demo("22222222-aaaa-4bbb-8ccc-000000000002.jsonl"),
            DEFAULT_MAX_LINE_BYTES,
        )
        .unwrap();
        assert!(!session.interactive);
        assert_eq!(session.message_count, 2);
        assert_eq!(session.title.chars().count(), 120);
        assert!(session
            .title
            .starts_with("Summarize the open review findings"));
        assert!(session.title.ends_with('…'));
        assert_eq!(session.subagent_count, 0);
        assert_eq!(session.git_branch, None);
    }

    #[test]
    fn session_without_title_or_prompt_is_untitled() {
        let session = parse_claude_session(
            &demo("33333333-aaaa-4bbb-8ccc-000000000003.jsonl"),
            DEFAULT_MAX_LINE_BYTES,
        )
        .unwrap();
        assert_eq!(session.title, UNTITLED);
        assert_eq!(session.first_prompt, None);
        assert_eq!(session.last_reply, None);
        assert_eq!(session.message_count, 0);
        assert_eq!(session.model.as_deref(), Some("claude-opus-5-5"));
        assert!(session.interactive);
    }

    #[test]
    fn oversized_lines_are_skipped_and_counted() {
        let session =
            parse_claude_session(&demo("11111111-aaaa-4bbb-8ccc-000000000001.jsonl"), 200).unwrap();
        assert_eq!(session.skipped_lines, 8);
        assert_eq!(session.title, "Fetch helper retry and tests");
        assert_eq!(session.message_count, 0);
    }

    #[test]
    fn bat_harness_noise_is_not_a_prompt() {
        let dir = TempDir::new("claude-noise");
        let path = dir
            .path()
            .join("44444444-aaaa-4bbb-8ccc-000000000004.jsonl");
        let user = |text: &str| {
            serde_json::json!({"type": "user", "timestamp": "2026-10-01T08:00:00.000Z", "cwd": "D:\\w", "message": {"role": "user", "content": text}})
                .to_string()
        };
        let lines = [
            user("[Request interrupted by user for tool use]"),
            user("This session is being continued from a previous conversation that ran out of context."),
            user("<local-command-caveat>Caveat</local-command-caveat>"),
            user("<task-notification>done</task-notification>"),
            user("<task-notification>done</task-notification>  Fix   the parser"),
            user("<task-notification>unterminated"),
        ];
        fs::write(&path, lines.join("\n")).unwrap();
        let session = parse_claude_session(&path, DEFAULT_MAX_LINE_BYTES).unwrap();
        assert_eq!(session.first_prompt.as_deref(), Some("Fix the parser"));
        assert_eq!(session.title, "Fix the parser");
        assert_eq!(session.message_count, 1);
    }
}
