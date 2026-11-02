use serde::{Deserialize, Serialize};

use crate::SessionAgent;

pub(crate) const UNTITLED: &str = "(untitled)";

/// `^[A-Za-z0-9][A-Za-z0-9._-]*$`; must match `isValidSessionId` in renderer/src/utils/session-history-actions.ts.
pub(crate) fn is_valid_session_id(id: &str) -> bool {
    let mut chars = id.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// One parsed transcript file; persisted in the cache, so changing it requires a cache schema bump.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SessionRecord {
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
    pub started_at: Option<String>,
    pub updated_at: Option<String>,
    pub file_path: String,
    pub interactive: bool,
    pub is_subagent: bool,
    pub parent_id: Option<String>,
    pub skipped_lines: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_ids_must_be_plain_tokens() {
        for valid in [
            "3f2a9c1e-7b4d-4e0a-9c2f-1d8e6a5b4c3d",
            "0aaaaaaa-0000-7000-8000-000000000001",
            "a",
            "A1._-z",
        ] {
            assert!(is_valid_session_id(valid), "{valid}");
        }
        for invalid in [
            "",
            "-rf",
            ".hidden",
            "_x",
            "abc;rm -rf",
            "a b",
            "a&b",
            "$(id)",
            "a`b`",
            "a|b",
            "a/b",
            "a\\b",
            "\u{e9}t\u{e9}",
            "a\nb",
        ] {
            assert!(!is_valid_session_id(invalid), "{invalid:?}");
        }
    }
}
