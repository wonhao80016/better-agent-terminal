// session-history:* — host-owned scan of Claude and Codex transcripts and moves to the OS trash.
use crate::codex_app_server::active_codex_home;
use crate::commands::app::log_tauri;
use crate::host_context::HostContext;
#[cfg(feature = "desktop")]
use crate::remote_client::RustRemoteClientState;
use bat_session_index::{
    clamp_max_line_bytes, resolve_claude_home, scan, trash_preview, trash_sessions, ScanOptions,
    SessionHistoryResult, SessionRef, SessionTrashPreview, SessionTrashResult, SystemTrash,
    TrashBackend, DEFAULT_MAX_LINE_BYTES, MAX_TRASH_SESSIONS_PER_REQUEST,
};
#[cfg(feature = "desktop")]
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Value};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, SystemTime};
#[cfg(feature = "desktop")]
use tauri::{AppHandle, Manager, WebviewWindow};

pub(crate) const REMOTE_CHANNEL: &str = "session-history:list";
pub(crate) const REMOTE_TRASH_PREVIEW_CHANNEL: &str = "session-history:trash-preview";
pub(crate) const REMOTE_TRASH_CHANNEL: &str = "session-history:trash";
/// A cold scan parses every transcript on the host and can outlast the generic 15 s invoke timeout;
/// trash requests resolve owned files with the same scan.
pub(crate) const REMOTE_TIMEOUT: Duration = Duration::from_secs(120);
const CACHE_DIR: &str = "session-history";
const CACHE_FILE: &str = "index-v1.json";

/// Scans and trash requests share a cache file and process-wide git cache, so callers must serialize.
static SCAN_LOCK: Mutex<()> = Mutex::new(());

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionHistoryListOptions {
    pub refresh: bool,
    pub include_non_interactive: bool,
    pub include_codex_archived: bool,
    /// Clamped to 1 MiB ..= 1 GiB.
    pub max_line_bytes: u64,
}

impl Default for SessionHistoryListOptions {
    fn default() -> Self {
        Self {
            refresh: false,
            include_non_interactive: false,
            include_codex_archived: false,
            max_line_bytes: DEFAULT_MAX_LINE_BYTES,
        }
    }
}

impl SessionHistoryListOptions {
    /// Each missing or malformed field falls back to its default on its own.
    pub(crate) fn from_params(params: &Value) -> Self {
        let flag = |key: &str| params.get(key).and_then(Value::as_bool).unwrap_or(false);
        let max_line_bytes = params
            .get("maxLineBytes")
            .and_then(|value| {
                value.as_u64().or_else(|| {
                    value
                        .as_f64()
                        .filter(|bytes| bytes.is_finite() && *bytes >= 0.0)
                        .map(|bytes| bytes as u64)
                })
            })
            .map_or(DEFAULT_MAX_LINE_BYTES, clamp_max_line_bytes);
        Self {
            refresh: flag("refresh"),
            include_non_interactive: flag("includeNonInteractive"),
            include_codex_archived: flag("includeCodexArchived"),
            max_line_bytes,
        }
    }

    #[cfg_attr(not(feature = "desktop"), allow(dead_code))]
    fn to_params(&self) -> Value {
        json!({
            "refresh": self.refresh,
            "includeNonInteractive": self.include_non_interactive,
            "includeCodexArchived": self.include_codex_archived,
            "maxLineBytes": self.max_line_bytes,
        })
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ScanSources {
    pub claude_home: Option<PathBuf>,
    pub codex_home: Option<PathBuf>,
    pub data_dir: Option<PathBuf>,
}

impl ScanSources {
    fn for_host(ctx: &HostContext) -> Self {
        let home = ctx.home_dir();
        let data_dir = match ctx.data_dir() {
            Ok(dir) => Some(dir),
            Err(err) => {
                log_tauri(
                    ctx,
                    &format!(
                        "[session-history] app data dir unavailable, scanning without cache: {err}"
                    ),
                );
                None
            }
        };
        Self {
            claude_home: resolve_claude_home(
                std::env::var_os("CLAUDE_CONFIG_DIR"),
                home.as_deref(),
            ),
            codex_home: resolve_codex_home(
                active_codex_home(ctx),
                std::env::var_os("CODEX_HOME"),
                home.as_deref(),
            ),
            data_dir,
        }
    }

    fn scan_options(&self, options: &SessionHistoryListOptions) -> ScanOptions {
        ScanOptions {
            claude_home: self.claude_home.clone(),
            codex_home: self.codex_home.clone(),
            cache_path: self
                .data_dir
                .as_ref()
                .map(|dir| dir.join(CACHE_DIR).join(CACHE_FILE)),
            max_line_bytes: options.max_line_bytes,
            retry_git_misses: options.refresh,
            include_non_interactive: options.include_non_interactive,
            include_codex_archived: options.include_codex_archived,
        }
    }

    fn require_home(&self) -> Result<(), String> {
        if self.claude_home.is_none() && self.codex_home.is_none() {
            return Err(
                "session history: could not resolve the Claude or Codex home directory".into(),
            );
        }
        Ok(())
    }
}

fn resolve_codex_home(
    active: Option<PathBuf>,
    env_codex_home: Option<OsString>,
    home: Option<&Path>,
) -> Option<PathBuf> {
    active
        .or_else(|| {
            env_codex_home
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
        })
        .or_else(|| home.map(|home| home.join(".codex")))
}

/// Blocking; run on a worker thread, never on the async runtime.
fn scan_sources(
    sources: &ScanSources,
    options: &SessionHistoryListOptions,
) -> Result<SessionHistoryResult, String> {
    sources.require_home()?;
    let scan_options = sources.scan_options(options);
    let _guard = SCAN_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    Ok(scan(&scan_options))
}

fn check_request_size(sessions: &[SessionRef]) -> Result<(), String> {
    if sessions.len() > MAX_TRASH_SESSIONS_PER_REQUEST {
        return Err(format!(
            "session history: at most {MAX_TRASH_SESSIONS_PER_REQUEST} sessions can be moved to the trash per request; got {}",
            sessions.len()
        ));
    }
    Ok(())
}

/// Blocking; never modifies any file.
fn preview_sources(
    sources: &ScanSources,
    sessions: &[SessionRef],
    now: SystemTime,
) -> Result<SessionTrashPreview, String> {
    sources.require_home()?;
    check_request_size(sessions)?;
    let scan_options = sources.scan_options(&SessionHistoryListOptions::default());
    let _guard = SCAN_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    Ok(trash_preview(&scan_options, sessions, now))
}

/// Blocking; owned files are resolved from the host's own index, never from the request.
fn trash_from_sources(
    sources: &ScanSources,
    sessions: &[SessionRef],
    now: &dyn Fn() -> SystemTime,
    backend: &dyn TrashBackend,
) -> Result<SessionTrashResult, String> {
    sources.require_home()?;
    check_request_size(sessions)?;
    let scan_options = sources.scan_options(&SessionHistoryListOptions::default());
    let _guard = SCAN_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    Ok(trash_sessions(&scan_options, sessions, now, backend))
}

#[derive(Deserialize)]
struct TrashParams {
    sessions: Vec<SessionRef>,
}

pub(crate) fn sessions_from_params(
    params: &Value,
    channel: &str,
) -> Result<Vec<SessionRef>, String> {
    serde_json::from_value::<TrashParams>(params.clone())
        .map(|params| params.sessions)
        .map_err(|err| format!("{channel}: invalid sessions: {err}"))
}

pub(crate) fn install_diagnostics(ctx: HostContext) {
    bat_session_index::set_diagnostic_sink(move |message| {
        log_tauri(&ctx, &format!("[session-history] {message}"));
    });
}

fn log_failure<T>(ctx: &HostContext, action: &str, result: &Result<T, String>) {
    if let Err(err) = result {
        log_tauri(ctx, &format!("[session-history] {action} failed: {err}"));
    }
}

/// Counts only; session ids, paths and transcript contents stay out of the log.
fn log_preview(ctx: &HostContext, requested: usize, result: &Result<SessionTrashPreview, String>) {
    match result {
        Ok(preview) => log_tauri(
            ctx,
            &format!(
                "[session-history] trash-preview requested={requested} blocked={} files={} bytes={}",
                preview.blocked_count, preview.file_count, preview.total_bytes
            ),
        ),
        Err(_) => log_failure(ctx, "trash-preview", result),
    }
}

fn log_trash(ctx: &HostContext, requested: usize, result: &Result<SessionTrashResult, String>) {
    match result {
        Ok(outcome) => log_tauri(
            ctx,
            &format!(
                "[session-history] trash requested={requested} trashed={} blocked={} failed={}",
                outcome.trashed.len(),
                outcome.blocked.len(),
                outcome.failed.len()
            ),
        ),
        Err(_) => log_failure(ctx, "trash", result),
    }
}

/// Remote-server entry for `session-history:list`; blocking.
pub(crate) fn list_for_remote(ctx: &HostContext, params: &Value) -> Result<Value, String> {
    let result = list_value(&ScanSources::for_host(ctx), params);
    log_failure(ctx, "list", &result);
    result
}

pub(crate) fn list_value(sources: &ScanSources, params: &Value) -> Result<Value, String> {
    let options = SessionHistoryListOptions::from_params(params);
    let result = scan_sources(sources, &options)?;
    serde_json::to_value(result)
        .map_err(|err| format!("{REMOTE_CHANNEL} serialization failed: {err}"))
}

/// Remote-server entry for `session-history:trash-preview`; blocking.
pub(crate) fn trash_preview_for_remote(ctx: &HostContext, params: &Value) -> Result<Value, String> {
    let sessions = sessions_from_params(params, REMOTE_TRASH_PREVIEW_CHANNEL);
    let requested = sessions.as_ref().map_or(0, Vec::len);
    let result = sessions.and_then(|sessions| {
        preview_sources(&ScanSources::for_host(ctx), &sessions, SystemTime::now())
    });
    log_preview(ctx, requested, &result);
    result.and_then(|preview| {
        serde_json::to_value(preview)
            .map_err(|err| format!("{REMOTE_TRASH_PREVIEW_CHANNEL} serialization failed: {err}"))
    })
}

/// Remote-server entry for `session-history:trash`; blocking.
pub(crate) fn trash_for_remote(ctx: &HostContext, params: &Value) -> Result<Value, String> {
    let sessions = sessions_from_params(params, REMOTE_TRASH_CHANNEL);
    let requested = sessions.as_ref().map_or(0, Vec::len);
    let result = sessions.and_then(|sessions| {
        trash_from_sources(
            &ScanSources::for_host(ctx),
            &sessions,
            &SystemTime::now,
            &SystemTrash,
        )
    });
    log_trash(ctx, requested, &result);
    result.and_then(|outcome| {
        serde_json::to_value(outcome)
            .map_err(|err| format!("{REMOTE_TRASH_CHANNEL} serialization failed: {err}"))
    })
}

/// Uses its own timeout instead of `fs::remote_invoke_for_window`, which is fixed at 15 s.
#[cfg(feature = "desktop")]
async fn remote_invoke<T: DeserializeOwned + Send + 'static>(
    app: &AppHandle,
    window: &WebviewWindow,
    channel: &'static str,
    params: Value,
) -> Option<Result<T, String>> {
    if !crate::commands::fs::is_remote_profile_window(app, window) {
        return None;
    }
    let remote_client = app.state::<RustRemoteClientState>().inner().clone();
    let window_label = window.label().to_string();
    let result = crate::async_rt::spawn_blocking(move || {
        remote_client.invoke(&window_label, channel, vec![params], REMOTE_TIMEOUT)
    })
    .await
    .map_err(|err| format!("remote.invoke {channel} worker failed: {err}"))
    .and_then(|result| result)
    .and_then(|value| {
        serde_json::from_value(value)
            .map_err(|err| format!("{channel}: invalid host response: {err}"))
    });
    Some(result)
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn session_history_list(
    app: AppHandle,
    window: WebviewWindow,
    options: Option<Value>,
) -> Result<SessionHistoryResult, String> {
    let options = SessionHistoryListOptions::from_params(&options.unwrap_or(Value::Null));
    let ctx = HostContext::from_app(app.clone());
    let result = match remote_invoke(&app, &window, REMOTE_CHANNEL, options.to_params()).await {
        Some(result) => result,
        None => {
            let worker_ctx = ctx.clone();
            crate::async_rt::spawn_blocking(move || {
                scan_sources(&ScanSources::for_host(&worker_ctx), &options)
            })
            .await
            .map_err(|err| format!("session history worker failed: {err}"))
            .and_then(|result| result)
        }
    };
    log_failure(&ctx, "list", &result);
    result
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn session_history_trash_preview(
    app: AppHandle,
    window: WebviewWindow,
    sessions: Vec<SessionRef>,
) -> Result<SessionTrashPreview, String> {
    let ctx = HostContext::from_app(app.clone());
    let params = json!({ "sessions": sessions });
    if let Some(result) = remote_invoke(&app, &window, REMOTE_TRASH_PREVIEW_CHANNEL, params).await {
        log_failure(&ctx, "trash-preview", &result);
        return result;
    }
    let worker_ctx = ctx.clone();
    let requested = sessions.len();
    let result = crate::async_rt::spawn_blocking(move || {
        preview_sources(
            &ScanSources::for_host(&worker_ctx),
            &sessions,
            SystemTime::now(),
        )
    })
    .await
    .map_err(|err| format!("session history worker failed: {err}"))
    .and_then(|result| result);
    log_preview(&ctx, requested, &result);
    result
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn session_history_trash(
    app: AppHandle,
    window: WebviewWindow,
    sessions: Vec<SessionRef>,
) -> Result<SessionTrashResult, String> {
    let ctx = HostContext::from_app(app.clone());
    let params = json!({ "sessions": sessions });
    if let Some(result) = remote_invoke(&app, &window, REMOTE_TRASH_CHANNEL, params).await {
        log_failure(&ctx, "trash", &result);
        return result;
    }
    let worker_ctx = ctx.clone();
    let requested = sessions.len();
    let result = crate::async_rt::spawn_blocking(move || {
        trash_from_sources(
            &ScanSources::for_host(&worker_ctx),
            &sessions,
            &SystemTime::now,
            &SystemTrash,
        )
    })
    .await
    .map_err(|err| format!("session history worker failed: {err}"))
    .and_then(|result| result);
    log_trash(&ctx, requested, &result);
    result
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use std::cell::RefCell;

    /// Records each request and fails it; never touches the file system.
    #[derive(Default)]
    pub(crate) struct RecordingTrash {
        pub calls: RefCell<Vec<Vec<PathBuf>>>,
    }

    impl TrashBackend for RecordingTrash {
        fn move_to_trash(&self, paths: &[PathBuf]) -> Result<(), String> {
            self.calls.borrow_mut().push(paths.to_vec());
            Err("trash disabled in tests".into())
        }
    }

    pub(crate) fn preview_value(
        sources: &ScanSources,
        params: &Value,
        now: SystemTime,
    ) -> Result<Value, String> {
        let sessions = sessions_from_params(params, REMOTE_TRASH_PREVIEW_CHANNEL)?;
        preview_sources(sources, &sessions, now).map(|preview| json!(preview))
    }

    pub(crate) fn trash_value(
        sources: &ScanSources,
        params: &Value,
        now: &dyn Fn() -> SystemTime,
        backend: &dyn TrashBackend,
    ) -> Result<Value, String> {
        let sessions = sessions_from_params(params, REMOTE_TRASH_CHANNEL)?;
        trash_from_sources(sources, &sessions, now, backend).map(|outcome| json!(outcome))
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::RecordingTrash;
    use super::*;
    use bat_session_index::SessionAgent;

    fn fixtures() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("crates")
            .join("bat-session-index")
            .join("fixtures")
    }

    fn fixture_sources() -> ScanSources {
        ScanSources {
            claude_home: Some(fixtures().join("claude")),
            codex_home: Some(fixtures().join("codex")),
            data_dir: None,
        }
    }

    #[test]
    fn options_default_to_plain_interactive_scan() {
        let defaults = SessionHistoryListOptions::from_params(&Value::Null);
        assert_eq!(defaults, SessionHistoryListOptions::default());
        assert!(!defaults.refresh);
        assert!(!defaults.include_non_interactive);
        assert!(!defaults.include_codex_archived);
        assert_eq!(defaults.max_line_bytes, DEFAULT_MAX_LINE_BYTES);
        assert_eq!(SessionHistoryListOptions::from_params(&json!({})), defaults);
        assert_eq!(
            SessionHistoryListOptions::from_params(&json!({ "refresh": null })),
            defaults
        );
    }

    #[test]
    fn options_parse_each_field_and_tolerate_malformed_values() {
        let parsed = SessionHistoryListOptions::from_params(&json!({
            "refresh": true,
            "includeNonInteractive": true,
            "includeCodexArchived": true,
            "maxLineBytes": 2_097_152,
        }));
        assert_eq!(
            parsed,
            SessionHistoryListOptions {
                refresh: true,
                include_non_interactive: true,
                include_codex_archived: true,
                max_line_bytes: 2_097_152,
            }
        );
        assert_eq!(
            SessionHistoryListOptions::from_params(&parsed.to_params()),
            parsed
        );
        let malformed = SessionHistoryListOptions::from_params(&json!({
            "refresh": "yes",
            "includeNonInteractive": 1,
            "includeCodexArchived": true,
            "maxLineBytes": -5,
        }));
        assert!(!malformed.refresh);
        assert!(!malformed.include_non_interactive);
        assert!(malformed.include_codex_archived);
        assert_eq!(malformed.max_line_bytes, DEFAULT_MAX_LINE_BYTES);
        assert_eq!(
            SessionHistoryListOptions::from_params(&json!({ "maxLineBytes": 3_145_728.0 }))
                .max_line_bytes,
            3_145_728
        );
        let max_line_bytes = |value: Value| {
            SessionHistoryListOptions::from_params(&json!({ "maxLineBytes": value })).max_line_bytes
        };
        assert_eq!(max_line_bytes(json!(1e18)), 1024 * 1024 * 1024);
        assert_eq!(max_line_bytes(json!(u64::MAX)), 1024 * 1024 * 1024);
        assert_eq!(max_line_bytes(json!(1)), 1024 * 1024);
    }

    #[test]
    fn scan_options_carry_list_options_and_place_cache_under_data_dir() {
        let sources = ScanSources {
            claude_home: Some(PathBuf::from("claude-home")),
            codex_home: Some(PathBuf::from("codex-home")),
            data_dir: Some(PathBuf::from("data")),
        };
        let plain = sources.scan_options(&SessionHistoryListOptions::default());
        assert_eq!(plain.claude_home, sources.claude_home);
        assert_eq!(plain.codex_home, sources.codex_home);
        assert_eq!(
            plain.cache_path,
            Some(
                PathBuf::from("data")
                    .join("session-history")
                    .join("index-v1.json")
            )
        );
        assert_eq!(plain.max_line_bytes, DEFAULT_MAX_LINE_BYTES);
        assert!(!plain.retry_git_misses);
        assert!(!plain.include_non_interactive);
        assert!(!plain.include_codex_archived);

        let custom = sources.scan_options(&SessionHistoryListOptions {
            refresh: true,
            include_non_interactive: true,
            include_codex_archived: true,
            max_line_bytes: 5,
        });
        assert!(custom.retry_git_misses);
        assert!(custom.include_non_interactive);
        assert!(custom.include_codex_archived);
        assert_eq!(custom.max_line_bytes, 5);

        let uncached = ScanSources {
            data_dir: None,
            ..sources
        };
        assert_eq!(
            uncached
                .scan_options(&SessionHistoryListOptions::default())
                .cache_path,
            None
        );
    }

    #[test]
    fn codex_home_prefers_active_then_env_then_home() {
        let home = Path::new("home");
        assert_eq!(
            resolve_codex_home(
                Some(PathBuf::from("active")),
                Some(OsString::from("env")),
                Some(home)
            ),
            Some(PathBuf::from("active"))
        );
        assert_eq!(
            resolve_codex_home(None, Some(OsString::from("env")), Some(home)),
            Some(PathBuf::from("env"))
        );
        assert_eq!(
            resolve_codex_home(None, Some(OsString::new()), Some(home)),
            Some(home.join(".codex"))
        );
        assert_eq!(resolve_codex_home(None, None, None), None);
    }

    #[test]
    fn claude_home_follows_claude_config_dir() {
        let home = Path::new("home");
        assert_eq!(
            resolve_claude_home(Some(OsString::from("custom-claude")), Some(home)),
            Some(PathBuf::from("custom-claude"))
        );
        assert_eq!(
            resolve_claude_home(Some(OsString::new()), Some(home)),
            Some(home.join(".claude"))
        );
        assert_eq!(
            resolve_claude_home(None, Some(home)),
            Some(home.join(".claude"))
        );
    }

    #[test]
    fn requests_without_any_home_are_errors() {
        let none = ScanSources::default();
        let err = scan_sources(&none, &SessionHistoryListOptions::default()).unwrap_err();
        assert!(err.contains("home directory"), "{err}");
        assert!(preview_sources(&none, &[], SystemTime::now()).is_err());
        let backend = RecordingTrash::default();
        assert!(trash_from_sources(&none, &[], &SystemTime::now, &backend).is_err());
        assert!(backend.calls.borrow().is_empty());
    }

    #[test]
    fn list_honours_non_interactive_option() {
        let sources = fixture_sources();
        let plain = list_value(&sources, &Value::Null).unwrap();
        let all = list_value(&sources, &json!({ "includeNonInteractive": true })).unwrap();
        let count = |value: &Value| value["sessions"].as_array().unwrap().len();
        assert_eq!(count(&all), count(&plain) + 2);
        assert!(all["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["interactive"] == json!(false)));
    }

    #[test]
    fn trash_params_accept_only_agent_and_id() {
        let sessions = sessions_from_params(
            &json!({ "sessions": [{ "agent": "claude", "id": "a" }, { "agent": "codex", "id": "b" }] }),
            REMOTE_TRASH_CHANNEL,
        )
        .unwrap();
        assert_eq!(sessions[0].agent, SessionAgent::Claude);
        assert_eq!(sessions[1].id, "b");
        for bad in [
            Value::Null,
            json!({}),
            json!({ "sessions": [{ "agent": "gemini", "id": "a" }] }),
            json!({ "sessions": [{ "agent": "claude" }] }),
            json!({ "sessions": "a" }),
        ] {
            assert!(
                sessions_from_params(&bad, REMOTE_TRASH_CHANNEL).is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn trash_preview_and_trash_use_the_host_index_and_injected_backend() {
        let sources = fixture_sources();
        let later = || SystemTime::now() + Duration::from_secs(24 * 60 * 60);
        let params = json!({ "sessions": [
            { "agent": "claude", "id": "33333333-aaaa-4bbb-8ccc-000000000003" },
            { "agent": "codex", "id": "missing" }
        ] });
        let preview = test_support::preview_value(&sources, &params, later()).unwrap();
        assert_eq!(preview["items"][0]["blockedReason"], Value::Null);
        assert_eq!(preview["items"][1]["blockedReason"], json!("not-found"));
        assert_eq!(preview["blockedCount"], json!(1));
        assert_eq!(preview["fileCount"], json!(1));

        let backend = RecordingTrash::default();
        let result = test_support::trash_value(&sources, &params, &later, &backend).unwrap();
        assert_eq!(result["trashed"], json!([]));
        assert_eq!(
            result["blocked"],
            json!([{ "agent": "codex", "id": "missing", "reason": "not-found" }])
        );
        assert_eq!(result["failed"][0]["agent"], json!("claude"));
        assert!(result["failed"][0]["error"]
            .as_str()
            .unwrap()
            .contains("trash disabled in tests"));
        let calls = backend.calls.borrow();
        assert_eq!(calls.len(), 1);
        let transcript = calls[0].last().unwrap();
        assert!(calls[0]
            .iter()
            .all(|path| path.starts_with(fixtures().join("claude"))));
        assert!(transcript.is_file());
    }

    #[test]
    fn oversized_trash_requests_are_rejected_before_any_move() {
        let sources = fixture_sources();
        let sessions: Vec<Value> = (0..=MAX_TRASH_SESSIONS_PER_REQUEST)
            .map(|index| json!({ "agent": "claude", "id": format!("id-{index}") }))
            .collect();
        let params = json!({ "sessions": sessions });
        let preview =
            test_support::preview_value(&sources, &params, SystemTime::now()).unwrap_err();
        assert!(preview.contains("at most 200 sessions"), "{preview}");
        let backend = RecordingTrash::default();
        let trash =
            test_support::trash_value(&sources, &params, &SystemTime::now, &backend).unwrap_err();
        assert!(trash.contains("at most 200 sessions"), "{trash}");
        assert!(backend.calls.borrow().is_empty());

        let at_limit = json!({ "sessions": &params["sessions"].as_array().unwrap()[1..] });
        assert!(test_support::preview_value(&sources, &at_limit, SystemTime::now()).is_ok());
    }

    #[test]
    fn list_reports_the_host_platform() {
        let value = list_value(&fixture_sources(), &Value::Null).unwrap();
        let expected = if cfg!(windows) {
            "windows"
        } else if cfg!(target_os = "macos") {
            "macos"
        } else {
            "linux"
        };
        assert_eq!(value["hostPlatform"], json!(expected));
    }
}
