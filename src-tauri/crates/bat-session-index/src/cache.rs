use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::model::SessionRecord;

/// Bump when SessionRecord or parser semantics change so stale entries are discarded.
pub(crate) const CACHE_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CacheEntry {
    /// Nanoseconds since the Unix epoch; negative for older files.
    pub mtime_ns: i64,
    pub size: u64,
    /// None when the file was parsed but held no session.
    pub record: Option<SessionRecord>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CacheFile {
    version: u32,
    max_line_bytes: u64,
    entries: HashMap<String, CacheEntry>,
}

pub(crate) fn mtime_ns(modified: SystemTime) -> i64 {
    match modified.duration_since(UNIX_EPOCH) {
        Ok(after) => i64::try_from(after.as_nanos()).unwrap_or(i64::MAX),
        Err(before) => i64::try_from(before.duration().as_nanos()).map_or(i64::MIN, |n| -n),
    }
}

/// Any unreadable, malformed, or mismatched cache yields an empty map rather than an error.
pub(crate) fn load_cache(path: &Path, max_line_bytes: u64) -> HashMap<String, CacheEntry> {
    match load_cache_any_limit(path) {
        Some((limit, entries)) if limit == max_line_bytes => entries,
        _ => HashMap::new(),
    }
}

/// Returns the entries with the line limit they were parsed under; None for an unusable cache.
pub(crate) fn load_cache_any_limit(path: &Path) -> Option<(u64, HashMap<String, CacheEntry>)> {
    let text = fs::read_to_string(path).ok()?;
    let file = serde_json::from_str::<CacheFile>(&text).ok()?;
    (file.version == CACHE_SCHEMA_VERSION).then_some((file.max_line_bytes, file.entries))
}

/// Drops the given keys and rewrites the cache only when one of them was present.
pub(crate) fn forget_cache_entries(path: &Path, keys: &[String]) -> io::Result<usize> {
    let Some((limit, mut entries)) = load_cache_any_limit(path) else {
        return Ok(0);
    };
    let removed = keys
        .iter()
        .filter(|key| entries.remove(key.as_str()).is_some())
        .count();
    if removed > 0 {
        save_cache(path, limit, entries)?;
    }
    Ok(removed)
}

/// Writes through a uniquely named temp file and a rename so readers never see a partial cache.
pub(crate) fn save_cache(
    path: &Path,
    max_line_bytes: u64,
    entries: HashMap<String, CacheEntry>,
) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let body = serde_json::to_vec(&CacheFile {
        version: CACHE_SCHEMA_VERSION,
        max_line_bytes,
        entries,
    })
    .map_err(io::Error::other)?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let temp = path.with_file_name(format!(
        "{file_name}.{}.{nonce:08x}.tmp",
        std::process::id()
    ));
    let result = fs::write(&temp, body).and_then(|()| fs::rename(&temp, path));
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;

    fn entry() -> CacheEntry {
        CacheEntry {
            mtime_ns: 1,
            size: 2,
            record: None,
        }
    }

    #[test]
    fn round_trips_and_rejects_mismatches() {
        let dir = TempDir::new("cache-unit");
        let path = dir.path().join("nested").join("index-v1.json");
        save_cache(&path, 100, HashMap::from([("a".to_string(), entry())])).unwrap();
        assert_eq!(load_cache(&path, 100).get("a"), Some(&entry()));
        assert!(load_cache(&path, 200).is_empty());

        fs::write(&path, r#"{"version":0,"maxLineBytes":100,"entries":{}}"#).unwrap();
        assert!(load_cache(&path, 100).is_empty());
        fs::write(&path, "{not json").unwrap();
        assert!(load_cache(&path, 100).is_empty());
        assert!(load_cache(&dir.path().join("absent.json"), 100).is_empty());
        let leftovers = fs::read_dir(path.parent().unwrap()).unwrap().count();
        assert_eq!(leftovers, 1);
    }

    #[test]
    fn forgetting_entries_keeps_the_stored_limit_and_other_entries() {
        let dir = TempDir::new("cache-forget");
        let path = dir.path().join("index-v1.json");
        assert_eq!(forget_cache_entries(&path, &["a".to_string()]).unwrap(), 0);
        let entries = HashMap::from([("a".to_string(), entry()), ("b".to_string(), entry())]);
        save_cache(&path, 300, entries).unwrap();
        assert_eq!(
            forget_cache_entries(&path, &["a".to_string(), "x".to_string()]).unwrap(),
            1
        );
        let (limit, left) = load_cache_any_limit(&path).unwrap();
        assert_eq!(limit, 300);
        assert_eq!(left.keys().collect::<Vec<_>>(), vec!["b"]);
    }

    #[test]
    fn mtime_before_epoch_is_negative() {
        let before = UNIX_EPOCH - std::time::Duration::from_secs(5);
        assert_eq!(mtime_ns(before), -5_000_000_000);
    }
}
