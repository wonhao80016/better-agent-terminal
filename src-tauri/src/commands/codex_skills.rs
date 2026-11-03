// codex:skills-list — host-owned Codex skill catalog for the `/` menu.
use crate::codex_app_server::CodexAppServerState;
use crate::host_context::HostContext;
#[cfg(feature = "desktop")]
use crate::remote_client::RustRemoteClientState;
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};
use std::time::Duration;
#[cfg(feature = "desktop")]
use tauri::{AppHandle, Manager, State, WebviewWindow};

pub(crate) const REMOTE_CHANNEL: &str = "codex:skills-list";
/// Covers a lazy app-server spawn plus the `skills/list` request itself.
pub(crate) const REMOTE_TIMEOUT: Duration = Duration::from_secs(90);
pub(crate) const SKILLS_CHANGED_EVENT: &str = "claude:codex-skills-changed";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SkillsListRequest {
    pub session_id: String,
    pub cwd: Option<String>,
    pub force_reload: bool,
}

impl SkillsListRequest {
    pub(crate) fn from_params(params: &Value) -> Result<Self, String> {
        let session_id = params
            .get("sessionId")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| format!("{REMOTE_CHANNEL}: missing sessionId"))?
            .to_string();
        let cwd = params
            .get("cwd")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        let force_reload = params
            .get("forceReload")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        Ok(Self {
            session_id,
            cwd,
            force_reload,
        })
    }

    #[cfg(any(feature = "desktop", test))]
    fn to_params(&self) -> Value {
        json!({
            "sessionId": self.session_id,
            "cwd": self.cwd,
            "forceReload": self.force_reload,
        })
    }
}

/// An empty `cwds` makes the app-server fall back to the session working directory.
pub(crate) fn skills_list_params(cwd: Option<&str>, force_reload: bool) -> Value {
    let cwds: Vec<&str> = cwd.into_iter().collect();
    json!({ "cwds": cwds, "forceReload": force_reload })
}

fn non_empty_str<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
}

fn summarize_skill(skill: &Value) -> Option<Value> {
    let name = non_empty_str(skill, "name")?;
    let path = non_empty_str(skill, "path")?;
    let interface = skill.get("interface").unwrap_or(&Value::Null);
    // SkillMetadata.shortDescription is the legacy SKILL.md field; the schema
    // documents interface.shortDescription as the preferred source.
    let short_description = non_empty_str(interface, "shortDescription")
        .or_else(|| non_empty_str(skill, "shortDescription"));
    let mut summary = Map::new();
    summary.insert("name".into(), json!(name));
    summary.insert("path".into(), json!(path));
    summary.insert(
        "description".into(),
        json!(non_empty_str(skill, "description").unwrap_or("")),
    );
    summary.insert(
        "enabled".into(),
        json!(skill
            .get("enabled")
            .and_then(Value::as_bool)
            .unwrap_or(false)),
    );
    let optional = [
        ("displayName", non_empty_str(interface, "displayName")),
        ("shortDescription", short_description),
        ("defaultPrompt", non_empty_str(interface, "defaultPrompt")),
        ("pluginId", non_empty_str(skill, "pluginId")),
        ("scope", non_empty_str(skill, "scope")),
    ];
    for (key, value) in optional {
        if let Some(value) = value {
            summary.insert(key.into(), json!(value));
        }
    }
    Some(Value::Object(summary))
}

/// Flattens every `SkillsListEntry` into one path-unique list for the renderer.
pub(crate) fn summarize_skills_list_response(response: &Value) -> Value {
    let mut seen_paths = HashSet::new();
    let mut skills = Vec::new();
    let mut errors = Vec::new();
    let entries = response
        .get("data")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    for entry in entries {
        for skill in entry
            .get("skills")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default()
        {
            if let Some(summary) = summarize_skill(skill) {
                if seen_paths.insert(summary["path"].as_str().unwrap_or_default().to_string()) {
                    skills.push(summary);
                }
            }
        }
        for error in entry
            .get("errors")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default()
        {
            errors.push(json!({
                "path": non_empty_str(error, "path").unwrap_or(""),
                "message": non_empty_str(error, "message").unwrap_or(""),
            }));
        }
    }
    json!({ "ready": true, "skills": skills, "errors": errors })
}

pub(crate) fn not_ready_skills_list() -> Value {
    json!({ "ready": false, "skills": [], "errors": [] })
}

/// Last summarized `skills/list` answer per cwd for one app-server connection.
#[derive(Debug, Default)]
pub(crate) struct SkillsListCache {
    generation: u64,
    by_cwd: HashMap<String, Vec<Value>>,
}

impl SkillsListCache {
    pub(crate) fn get(&self, cwd: &str) -> Option<Vec<Value>> {
        self.by_cwd.get(cwd).cloned()
    }

    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    /// Drops answers requested before the last `clear`, since they may predate a skills/changed.
    pub(crate) fn store(&mut self, cwd: &str, generation: u64, skills: Vec<Value>) -> bool {
        if generation != self.generation {
            return false;
        }
        self.by_cwd.insert(cwd.to_string(), skills);
        true
    }

    pub(crate) fn clear(&mut self) {
        self.generation += 1;
        self.by_cwd.clear();
    }
}

pub(crate) fn summary_skills(summary: &Value) -> Vec<Value> {
    summary["skills"].as_array().cloned().unwrap_or_default()
}

/// Matches app-server errors for a `turn/start` whose `UserInput` has no `skill` variant.
pub(crate) fn is_skill_input_rejection(error: &str) -> bool {
    let error = error.to_ascii_lowercase();
    ["unknown variant", "invalid params", "invalid request"]
        .iter()
        .any(|marker| error.contains(marker))
}

/// Returns `input` without `skill` items, or `None` when it carries none.
pub(crate) fn without_skill_inputs(input: &Value) -> Option<Value> {
    let items = input.as_array()?;
    let kept: Vec<Value> = items
        .iter()
        .filter(|item| item.get("type").and_then(Value::as_str) != Some("skill"))
        .cloned()
        .collect();
    (kept.len() != items.len()).then_some(Value::Array(kept))
}

fn is_mention_name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b':')
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mention<'a> {
    Plain(&'a str),
    Linked { path: &'a str },
}

fn scan_mention_name(bytes: &[u8], start: usize) -> usize {
    let mut end = start;
    while end < bytes.len() && is_mention_name_byte(bytes[end]) {
        end += 1;
    }
    end
}

/// Parses Codex's linked mention `[$name](path)` at `start`; returns the path and the end offset.
fn parse_linked_mention(prompt: &str, start: usize) -> Option<(&str, usize)> {
    let bytes = prompt.as_bytes();
    let name_start = start + 2;
    let name_end = scan_mention_name(bytes, name_start);
    if name_end == name_start || bytes.get(name_end..name_end + 2) != Some(b"](") {
        return None;
    }
    let path_start = name_end + 2;
    let close = path_start + prompt[path_start..].find(')')?;
    let raw_path = &prompt[path_start..close];
    let path = raw_path.trim();
    if path.is_empty() || raw_path.contains('\n') {
        return None;
    }
    Some((path, close + 1))
}

fn mentions(prompt: &str) -> Vec<Mention<'_>> {
    let bytes = prompt.as_bytes();
    let mut found = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'[' && bytes.get(index + 1) == Some(&b'$') {
            if let Some((path, end)) = parse_linked_mention(prompt, index) {
                found.push(Mention::Linked { path });
                index = end;
                continue;
            }
        }
        let preceded_by_name = index > 0 && is_mention_name_byte(bytes[index - 1]);
        if bytes[index] != b'$' || preceded_by_name {
            index += 1;
            continue;
        }
        let start = index + 1;
        let end = scan_mention_name(bytes, start);
        if end > start {
            found.push(Mention::Plain(&prompt[start..end]));
        }
        index = end.max(start);
    }
    found
}

pub(crate) fn prompt_may_mention_skill(prompt: &str) -> bool {
    !mentions(prompt).is_empty()
}

/// Builds `UserInput::Skill` items for enabled skills the prompt mentions.
/// `[$name](path)` selects by exact path, so it also resolves names several skills share;
/// a plain `$name` shared by several enabled skills is skipped instead of BAT picking one.
pub(crate) fn skill_inputs_for_prompt(prompt: &str, skills: &[Value]) -> Vec<Value> {
    let found = mentions(prompt);
    if found.is_empty() {
        return Vec::new();
    }
    let enabled: Vec<(&str, &str)> = skills
        .iter()
        .filter(|skill| skill.get("enabled").and_then(Value::as_bool) == Some(true))
        .filter_map(|skill| Some((non_empty_str(skill, "name")?, non_empty_str(skill, "path")?)))
        .collect();
    let mut selected: Vec<(&str, &str)> = Vec::new();
    let mut plain_names = HashSet::new();
    for mention in found {
        match mention {
            Mention::Linked { path } => {
                if let Some(skill) = enabled.iter().find(|(_, skill_path)| *skill_path == path) {
                    selected.push(*skill);
                }
            }
            Mention::Plain(name) => {
                plain_names.insert(name);
            }
        }
    }
    for name in plain_names {
        let mut matches = enabled.iter().filter(|(skill_name, _)| *skill_name == name);
        if let (Some(skill), None) = (matches.next(), matches.next()) {
            selected.push(*skill);
        }
    }
    selected.sort_unstable();
    selected.dedup_by(|a, b| a.1 == b.1);
    selected
        .into_iter()
        .map(|(name, path)| json!({ "type": "skill", "name": name, "path": path }))
        .collect()
}

/// Remote-server entry for `codex:skills-list`; blocking.
pub(crate) fn list_for_remote(ctx: &HostContext, params: &Value) -> Result<Value, String> {
    let request = SkillsListRequest::from_params(params)?;
    let codex = ctx.state::<CodexAppServerState>();
    codex.list_skills(ctx, &request)
}

#[cfg(feature = "desktop")]
async fn remote_invoke(
    app: &AppHandle,
    window: &WebviewWindow,
    request: &SkillsListRequest,
) -> Option<Result<Value, String>> {
    if !crate::commands::fs::is_remote_profile_window(app, window) {
        return None;
    }
    let remote_client = app.state::<RustRemoteClientState>().inner().clone();
    let window_label = window.label().to_string();
    let params = request.to_params();
    let result = crate::async_rt::spawn_blocking(move || {
        remote_client.invoke(&window_label, REMOTE_CHANNEL, vec![params], REMOTE_TIMEOUT)
    })
    .await
    .map_err(|err| format!("remote.invoke {REMOTE_CHANNEL} worker failed: {err}"))
    .and_then(|result| result);
    Some(result)
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn codex_list_skills(
    app: AppHandle,
    window: WebviewWindow,
    codex: State<'_, CodexAppServerState>,
    session_id: String,
    cwd: Option<String>,
    force_reload: Option<bool>,
) -> Result<Value, String> {
    let request = SkillsListRequest::from_params(&json!({
        "sessionId": session_id,
        "cwd": cwd,
        "forceReload": force_reload,
    }))?;
    if let Some(result) = remote_invoke(&app, &window, &request).await {
        return result;
    }
    let ctx = HostContext::from_app(app.clone());
    crate::commands::notification::ensure_agent_session_access(
        &ctx,
        window.label(),
        &request.session_id,
    )?;
    let codex = codex.inner().clone();
    crate::async_rt::spawn_blocking(move || codex.list_skills(&ctx, &request))
        .await
        .map_err(|err| format!("codex skills worker failed: {err}"))?
}

#[cfg(test)]
mod tests {
    use super::*;

    fn skill(name: &str, path: &str, enabled: bool) -> Value {
        json!({ "name": name, "path": path, "enabled": enabled, "description": "", "scope": "user" })
    }

    #[test]
    fn request_parses_params_and_defaults() {
        let request = SkillsListRequest::from_params(&json!({
            "sessionId": " s-1 ",
            "cwd": "  ",
            "forceReload": true,
        }))
        .unwrap();
        assert_eq!(
            request,
            SkillsListRequest {
                session_id: "s-1".into(),
                cwd: None,
                force_reload: true,
            }
        );
        let request =
            SkillsListRequest::from_params(&json!({ "sessionId": "s-2", "cwd": "/repo" })).unwrap();
        assert_eq!(request.cwd.as_deref(), Some("/repo"));
        assert!(!request.force_reload);
        assert!(SkillsListRequest::from_params(&json!({ "cwd": "/repo" })).is_err());
        assert!(SkillsListRequest::from_params(&Value::Null).is_err());
    }

    #[test]
    fn request_round_trips_through_remote_params() {
        let request = SkillsListRequest {
            session_id: "s-1".into(),
            cwd: Some("/repo".into()),
            force_reload: true,
        };
        assert_eq!(
            SkillsListRequest::from_params(&request.to_params()).unwrap(),
            request
        );
    }

    #[test]
    fn skills_list_params_match_protocol_shape() {
        assert_eq!(
            skills_list_params(Some("/repo"), true),
            json!({ "cwds": ["/repo"], "forceReload": true })
        );
        assert_eq!(
            skills_list_params(None, false),
            json!({ "cwds": [], "forceReload": false })
        );
    }

    #[test]
    fn summarize_flattens_entries_and_prefers_interface_fields() {
        let response = json!({
            "data": [
                {
                    "cwd": "/repo",
                    "skills": [
                        {
                            "name": "review",
                            "path": "/home/.codex/skills/review/SKILL.md",
                            "description": "Review code",
                            "shortDescription": "legacy short",
                            "enabled": true,
                            "scope": "user",
                            "pluginId": "plugin-a",
                            "interface": {
                                "displayName": "Code Review",
                                "shortDescription": "Review the diff",
                                "defaultPrompt": "Use $review on this diff",
                                "brandColor": "#fff"
                            }
                        },
                        {
                            "name": "legacy",
                            "path": "/repo/.codex/skills/legacy/SKILL.md",
                            "description": "Legacy skill",
                            "shortDescription": "legacy short",
                            "enabled": false,
                            "scope": "repo",
                            "interface": null
                        },
                        { "name": "", "path": "/x", "description": "", "enabled": true, "scope": "user" }
                    ],
                    "errors": [{ "path": "/bad/SKILL.md", "message": "invalid frontmatter" }]
                },
                {
                    "cwd": "/other",
                    "skills": [{
                        "name": "review",
                        "path": "/home/.codex/skills/review/SKILL.md",
                        "description": "dup",
                        "enabled": true,
                        "scope": "user"
                    }],
                    "errors": []
                }
            ]
        });
        let summary = summarize_skills_list_response(&response);
        assert_eq!(
            summary,
            json!({
                "ready": true,
                "skills": [
                    {
                        "name": "review",
                        "path": "/home/.codex/skills/review/SKILL.md",
                        "description": "Review code",
                        "enabled": true,
                        "displayName": "Code Review",
                        "shortDescription": "Review the diff",
                        "defaultPrompt": "Use $review on this diff",
                        "pluginId": "plugin-a",
                        "scope": "user"
                    },
                    {
                        "name": "legacy",
                        "path": "/repo/.codex/skills/legacy/SKILL.md",
                        "description": "Legacy skill",
                        "enabled": false,
                        "shortDescription": "legacy short",
                        "scope": "repo"
                    }
                ],
                "errors": [{ "path": "/bad/SKILL.md", "message": "invalid frontmatter" }]
            })
        );
        assert_eq!(
            summarize_skills_list_response(&json!({})),
            json!({ "ready": true, "skills": [], "errors": [] })
        );
    }

    #[test]
    fn mention_detection_requires_a_token_boundary() {
        assert!(prompt_may_mention_skill("$review this"));
        assert!(prompt_may_mention_skill("please use $plugin:review now"));
        assert!(!prompt_may_mention_skill("costs 5$ or $"));
        assert!(!prompt_may_mention_skill("abc$review"));
        assert!(!prompt_may_mention_skill("no mentions"));
    }

    #[test]
    fn skill_inputs_resolve_enabled_unambiguous_mentions() {
        let skills = vec![
            skill("review", "/u/review/SKILL.md", true),
            skill("review-pr", "/u/review-pr/SKILL.md", true),
            skill("off", "/u/off/SKILL.md", false),
            skill("dup", "/u/dup/SKILL.md", true),
            skill("dup", "/r/dup/SKILL.md", true),
        ];
        assert_eq!(
            skill_inputs_for_prompt("$review-pr then $review, $off $dup $missing", &skills),
            vec![
                json!({ "type": "skill", "name": "review", "path": "/u/review/SKILL.md" }),
                json!({ "type": "skill", "name": "review-pr", "path": "/u/review-pr/SKILL.md" }),
            ]
        );
        assert!(skill_inputs_for_prompt("review without dollar", &skills).is_empty());
        assert!(skill_inputs_for_prompt("x$review", &skills).is_empty());
    }

    #[test]
    fn linked_skill_mentions_select_by_path_even_for_shared_names() {
        let skills = vec![
            skill("dup", "/u/dup/SKILL.md", true),
            skill("dup", "C:\\repo (x)\\.codex\\skills\\dup\\SKILL.md", true),
            skill("off", "/u/off/SKILL.md", false),
            skill("review", "/u/review/SKILL.md", true),
        ];
        assert_eq!(
            skill_inputs_for_prompt("[$dup](/u/dup/SKILL.md) check $dup", &skills),
            vec![json!({ "type": "skill", "name": "dup", "path": "/u/dup/SKILL.md" })]
        );
        assert_eq!(
            skill_inputs_for_prompt(
                "use [$dup]( C:\\repo (x\\.codex ) and [$review](/u/review/SKILL.md) $review",
                &skills
            ),
            vec![json!({ "type": "skill", "name": "review", "path": "/u/review/SKILL.md" })]
        );
        assert!(skill_inputs_for_prompt("[$off](/u/off/SKILL.md)", &skills).is_empty());
        assert!(skill_inputs_for_prompt("[$dup](/elsewhere/SKILL.md)", &skills).is_empty());
        assert!(skill_inputs_for_prompt("[$dup](\n/u/dup/SKILL.md)", &skills).is_empty());
        assert!(skill_inputs_for_prompt("[$dup](/u/dup/SKILL.md", &skills).is_empty());
        assert!(prompt_may_mention_skill("[$dup](/u/dup/SKILL.md)"));
        assert!(prompt_may_mention_skill("[$dup] broken link"));
    }

    #[test]
    fn skills_cache_is_per_cwd_and_drops_answers_from_before_a_clear() {
        let mut cache = SkillsListCache::default();
        assert!(cache.get("/repo").is_none());
        let generation = cache.generation();
        assert!(cache.store("/repo", generation, vec![skill("a", "/a", true)]));
        assert_eq!(cache.get("/repo").unwrap().len(), 1);
        assert!(cache.get("/other").is_none());
        cache.clear();
        assert!(cache.get("/repo").is_none());
        assert!(!cache.store("/repo", generation, vec![skill("stale", "/s", true)]));
        assert!(cache.get("/repo").is_none());
        assert!(cache.store("/repo", cache.generation(), Vec::new()));
        assert_eq!(cache.get("/repo"), Some(Vec::new()));
    }

    #[test]
    fn skill_input_rejection_and_text_only_fallback() {
        assert!(is_skill_input_rejection(
            "Invalid request: unknown variant `skill`, expected one of `text`, `image`"
        ));
        assert!(is_skill_input_rejection("Invalid params: missing field `path`"));
        assert!(!is_skill_input_rejection("codex app-server request timed out: turn/start"));
        assert!(!is_skill_input_rejection("codex app-server exited"));
        let input = json!([
            { "type": "localImage", "path": "/tmp/a.png" },
            { "type": "text", "text": "$review" },
            { "type": "skill", "name": "review", "path": "/u/review/SKILL.md" },
        ]);
        assert_eq!(
            without_skill_inputs(&input),
            Some(json!([
                { "type": "localImage", "path": "/tmp/a.png" },
                { "type": "text", "text": "$review" },
            ]))
        );
        assert_eq!(without_skill_inputs(&json!([{ "type": "text", "text": "hi" }])), None);
        assert_eq!(without_skill_inputs(&json!({})), None);
    }
}
