use serde_json::{Map, Value};

pub(crate) const TITLE_CHARS: usize = 120;
pub(crate) const REPLY_CHARS: usize = 200;

pub(crate) fn collapse_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(crate) fn truncate(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max_chars.saturating_sub(1)).collect();
    out.push('…');
    out
}

pub(crate) fn summarize(text: &str, max_chars: usize) -> String {
    truncate(&collapse_whitespace(text), max_chars)
}

/// Joins the text of content blocks whose type is in `block_types`; plain-string content is returned as-is.
pub(crate) fn extract_text(content: Option<&Value>, block_types: &[&str]) -> String {
    match content {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter_map(Value::as_object)
            .filter(|block| {
                block
                    .get("type")
                    .and_then(Value::as_str)
                    .is_some_and(|kind| block_types.contains(&kind))
            })
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

pub(crate) fn non_empty_str<'a>(object: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    object
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
}

pub(crate) fn non_empty_string(object: &Map<String, Value>, key: &str) -> Option<String> {
    non_empty_str(object, key).map(str::to_string)
}

/// Text that CLIs inject into the user role (command echoes, notifications, environment blocks) starts with a tag.
pub(crate) fn looks_injected(text: &str) -> bool {
    text.trim_start().starts_with('<')
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn truncate_counts_characters_and_appends_ellipsis() {
        assert_eq!(truncate("abc", 3), "abc");
        assert_eq!(truncate("abcd", 3), "ab…");
        assert_eq!(truncate("測試測試", 3), "測試…");
    }

    #[test]
    fn summarize_collapses_whitespace() {
        assert_eq!(summarize("  a \n\t b  ", 10), "a b");
    }

    #[test]
    fn extract_text_joins_matching_blocks() {
        let content = json!([{"type": "text", "text": "a"}, {"type": "tool_use"}, {"type": "text", "text": "b"}]);
        assert_eq!(extract_text(Some(&content), &["text"]), "a\nb");
        assert_eq!(extract_text(Some(&json!("plain")), &["text"]), "plain");
        assert_eq!(extract_text(Some(&json!(1)), &["text"]), "");
    }
}
