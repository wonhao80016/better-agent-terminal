// session-history:* — host-owned scan of Claude and Codex transcripts.
use crate::codex_app_server::active_codex_home;
use crate::commands::app::log_tauri;
use crate::host_context::HostContext;
#[cfg(feature = "desktop")]
use crate::remote_client::RustRemoteClientState;
use bat_session_index::{scan, ScanOptions, SessionHistoryResult, DEFAULT_MAX_LINE_BYTES};
use serde::Deserialize;
#[cfg(feature = "desktop")]
use serde_json::json;
use serde_json::Value;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;
#[cfg(feature = "desktop")]
use tauri::{AppHandle, Manager, WebviewWindow};

pub(crate) const REMOTE_CHANNEL: &str = "session-history:list";
/// A cold scan parses every transcript on the host and can outlast the generic 15 s invoke timeout.
pub(crate) const REMOTE_TIMEOUT: Duration = Duration::from_secs(120);
const CACHE_DIR: &str = "session-history";
const CACHE_FILE: &str = "index-v1.json";

/// `bat_session_index::scan` shares a cache file and process-wide git cache, so callers must serialize.
static SCAN_LOCK: Mutex<()> = Mutex::new(());

#[derive(Debug, Default, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionHistoryListOptions {
    #[serde(default)]
    pub refresh: Option<bool>,
}

impl SessionHistoryListOptions {
    /// Malformed or missing params fall back to a plain, non-refresh scan.
    pub(crate) fn from_params(params: &Value) -> Self {
        serde_json::from_value(params.clone()).unwrap_or_default()
    }

    fn refresh(&self) -> bool {
        self.refresh.unwrap_or(false)
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
                    &format!("[session-history] app data dir unavailable, scanning without cache: {err}"),
                );
                None
            }
        };
        Self {
            claude_home: home.as_ref().map(|home| home.join(".claude")),
            codex_home: resolve_codex_home(
                active_codex_home(ctx),
                std::env::var_os("CODEX_HOME"),
                home.as_deref(),
            ),
            data_dir,
        }
    }

    fn scan_options(&self, refresh: bool) -> ScanOptions {
        ScanOptions {
            claude_home: self.claude_home.clone(),
            codex_home: self.codex_home.clone(),
            cache_path: self
                .data_dir
                .as_ref()
                .map(|dir| dir.join(CACHE_DIR).join(CACHE_FILE)),
            max_line_bytes: DEFAULT_MAX_LINE_BYTES,
            retry_git_misses: refresh,
        }
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
fn scan_sources(sources: &ScanSources, refresh: bool) -> Result<SessionHistoryResult, String> {
    if sources.claude_home.is_none() && sources.codex_home.is_none() {
        return Err("session history: could not resolve the Claude or Codex home directory".into());
    }
    let options = sources.scan_options(refresh);
    let _guard = SCAN_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    Ok(scan(&options))
}

fn log_failure<T>(ctx: &HostContext, result: &Result<T, String>) {
    if let Err(err) = result {
        log_tauri(ctx, &format!("[session-history] list failed: {err}"));
    }
}

/// Remote-server entry for `session-history:list`; blocking.
pub(crate) fn list_for_remote(ctx: &HostContext, params: &Value) -> Result<Value, String> {
    let result = list_value(&ScanSources::for_host(ctx), params);
    log_failure(ctx, &result);
    result
}

pub(crate) fn list_value(sources: &ScanSources, params: &Value) -> Result<Value, String> {
    let refresh = SessionHistoryListOptions::from_params(params).refresh();
    let result = scan_sources(sources, refresh)?;
    serde_json::to_value(result)
        .map_err(|err| format!("{REMOTE_CHANNEL} serialization failed: {err}"))
}

/// Uses its own timeout instead of `fs::remote_invoke_for_window`, which is fixed at 15 s.
#[cfg(feature = "desktop")]
async fn remote_list(
    app: &AppHandle,
    window: &WebviewWindow,
    refresh: bool,
) -> Option<Result<SessionHistoryResult, String>> {
    if !crate::commands::fs::is_remote_profile_window(app, window) {
        return None;
    }
    let remote_client = app.state::<RustRemoteClientState>().inner().clone();
    let window_label = window.label().to_string();
    let result = crate::async_rt::spawn_blocking(move || {
        remote_client.invoke(
            &window_label,
            REMOTE_CHANNEL,
            vec![json!(refresh)],
            REMOTE_TIMEOUT,
        )
    })
    .await
    .map_err(|err| format!("remote.invoke {REMOTE_CHANNEL} worker failed: {err}"))
    .and_then(|result| result)
    .and_then(|value| {
        serde_json::from_value(value)
            .map_err(|err| format!("{REMOTE_CHANNEL}: invalid host response: {err}"))
    });
    Some(result)
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn session_history_list(
    app: AppHandle,
    window: WebviewWindow,
    options: Option<SessionHistoryListOptions>,
) -> Result<SessionHistoryResult, String> {
    let refresh = options.unwrap_or_default().refresh();
    let ctx = HostContext::from_app(app.clone());
    let result = match remote_list(&app, &window, refresh).await {
        Some(result) => result,
        None => {
            let worker_ctx = ctx.clone();
            crate::async_rt::spawn_blocking(move || {
                scan_sources(&ScanSources::for_host(&worker_ctx), refresh)
            })
            .await
            .map_err(|err| format!("session history worker failed: {err}"))
            .and_then(|result| result)
        }
    };
    log_failure(&ctx, &result);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn options_default_to_no_refresh() {
        let parse = |value: Value| {
            serde_json::from_value::<Option<SessionHistoryListOptions>>(value)
                .unwrap()
                .unwrap_or_default()
                .refresh()
        };
        assert!(!parse(Value::Null));
        assert!(!parse(json!({})));
        assert!(!parse(json!({ "refresh": null })));
        assert!(!parse(json!({ "refresh": false })));
        assert!(parse(json!({ "refresh": true })));
    }

    #[test]
    fn remote_params_tolerate_missing_or_malformed_refresh() {
        let refresh = |value: Value| SessionHistoryListOptions::from_params(&value).refresh();
        assert!(!refresh(Value::Null));
        assert!(!refresh(json!({})));
        assert!(!refresh(json!({ "refresh": "yes" })));
        assert!(refresh(json!({ "refresh": true })));
    }

    #[test]
    fn scan_options_place_cache_under_data_dir_and_retry_git_only_on_refresh() {
        let sources = ScanSources {
            claude_home: Some(PathBuf::from("claude-home")),
            codex_home: Some(PathBuf::from("codex-home")),
            data_dir: Some(PathBuf::from("data")),
        };
        let plain = sources.scan_options(false);
        assert_eq!(plain.claude_home, sources.claude_home);
        assert_eq!(plain.codex_home, sources.codex_home);
        assert_eq!(
            plain.cache_path,
            Some(PathBuf::from("data").join("session-history").join("index-v1.json"))
        );
        assert_eq!(plain.max_line_bytes, DEFAULT_MAX_LINE_BYTES);
        assert!(!plain.retry_git_misses);
        assert!(sources.scan_options(true).retry_git_misses);

        let uncached = ScanSources {
            data_dir: None,
            ..sources
        };
        assert_eq!(uncached.scan_options(false).cache_path, None);
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
    fn scan_without_any_home_is_an_error() {
        let err = scan_sources(&ScanSources::default(), false).unwrap_err();
        assert!(err.contains("home directory"), "{err}");
    }
}
