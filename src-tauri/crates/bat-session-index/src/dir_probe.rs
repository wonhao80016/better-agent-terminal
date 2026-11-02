use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, TryRecvError};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::thread;
use std::time::Duration;

use crate::paths::{normalize_host_path, normalize_path, PathStyle};

const PROBE_TIMEOUT: Duration = Duration::from_secs(2);
const PROBE_CONCURRENCY: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DirState {
    Exists,
    /// Only a definite not-found; I/O errors and probe timeouts are `Unknown` so the group is not shown as gone.
    Missing,
    Unknown,
}

pub(crate) fn probe_dir(dir: &str) -> DirState {
    match fs::metadata(dir) {
        Ok(meta) if meta.is_dir() => DirState::Exists,
        Ok(_) => DirState::Unknown,
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
            ) =>
        {
            DirState::Missing
        }
        Err(_) => DirState::Unknown,
    }
}

#[derive(Default)]
struct ProbeMemo {
    /// Kept until a refresh: probing a UNC path can block for the SMB timeout or start a WSL distro.
    network: HashMap<String, DirState>,
    /// Paths whose probe thread outlived its deadline and is still blocked in the OS call, with their UNC share.
    stalled: HashMap<String, Option<String>>,
}

impl ProbeMemo {
    fn share_is_stalled(&self, share: &str) -> bool {
        self.stalled
            .values()
            .any(|stalled| stalled.as_deref() == Some(share))
    }
}

pub(crate) struct DirProber {
    memo: Arc<Mutex<ProbeMemo>>,
    probe: fn(&str) -> DirState,
    timeout: Duration,
}

impl DirProber {
    fn new(probe: fn(&str) -> DirState, timeout: Duration) -> Self {
        DirProber {
            memo: Arc::default(),
            probe,
            timeout,
        }
    }

    /// Returns states keyed by normalized path. A probe that misses its deadline is `Unknown`; while it stays
    /// blocked, that path and other paths on the same UNC share are reported `Unknown` without a new probe.
    pub(crate) fn probe_all(
        &self,
        dirs: &[&str],
        style: PathStyle,
        refresh: bool,
    ) -> HashMap<String, DirState> {
        let mut states = HashMap::new();
        let mut pending: Vec<(String, &str)> = Vec::new();
        {
            let mut memo = lock(&self.memo);
            if refresh {
                memo.network.clear();
            }
            let mut seen = HashSet::new();
            for &dir in dirs {
                let key = normalize_path(dir, style);
                if !seen.insert(key.clone()) {
                    continue;
                }
                if memo.stalled.contains_key(&key) {
                    states.insert(key, DirState::Unknown);
                } else if let Some(state) = memo.network.get(&key) {
                    states.insert(key, *state);
                } else {
                    pending.push((key, dir));
                }
            }
        }
        let next = AtomicUsize::new(0);
        let probed: Vec<(String, DirState)> = thread::scope(|scope| {
            let workers: Vec<_> = (0..PROBE_CONCURRENCY.min(pending.len()))
                .map(|_| {
                    scope.spawn(|| {
                        let mut found = Vec::new();
                        while let Some((key, dir)) =
                            pending.get(next.fetch_add(1, Ordering::Relaxed))
                        {
                            found.push((key.clone(), self.probe_with_deadline(key, dir)));
                        }
                        found
                    })
                })
                .collect();
            workers
                .into_iter()
                .flat_map(|w| w.join().unwrap_or_default())
                .collect()
        });
        states.extend(probed);
        states
    }

    fn probe_with_deadline(&self, key: &str, dir: &str) -> DirState {
        let share = unc_share(dir);
        if let Some(share) = &share {
            if lock(&self.memo).share_is_stalled(share) {
                return DirState::Unknown;
            }
        }
        // The thread and the waiter decide under the memo lock whether the result is delivered now or recorded
        // later, so a result that lands right at the deadline is neither lost nor left marked as stalled.
        let (sender, receiver) = mpsc::channel();
        let memo = Arc::clone(&self.memo);
        let probe = self.probe;
        let owned_key = key.to_string();
        let owned_dir = dir.to_string();
        let spawned = thread::Builder::new()
            .name("bat-session-dir-probe".into())
            .spawn(move || {
                let state = probe(&owned_dir);
                let mut memo = lock(&memo);
                match memo.stalled.remove(&owned_key) {
                    Some(Some(_)) => {
                        memo.network.insert(owned_key, state);
                    }
                    Some(None) => {}
                    None => {
                        let _ = sender.send(state);
                    }
                }
            });
        if spawned.is_err() {
            return DirState::Unknown;
        }
        let mut memo = match receiver.recv_timeout(self.timeout) {
            Ok(state) => return self.remember(key, share, state),
            Err(_) => lock(&self.memo),
        };
        match receiver.try_recv() {
            Ok(state) => {
                drop(memo);
                self.remember(key, share, state)
            }
            Err(TryRecvError::Empty) => {
                memo.stalled.insert(key.to_string(), share);
                DirState::Unknown
            }
            Err(TryRecvError::Disconnected) => DirState::Unknown,
        }
    }

    fn remember(&self, key: &str, share: Option<String>, state: DirState) -> DirState {
        if share.is_some() {
            lock(&self.memo).network.insert(key.to_string(), state);
        }
        state
    }
}

pub(crate) fn process_prober() -> &'static DirProber {
    static PROBER: OnceLock<DirProber> = OnceLock::new();
    PROBER.get_or_init(|| DirProber::new(probe_dir, PROBE_TIMEOUT))
}

fn lock(memo: &Mutex<ProbeMemo>) -> MutexGuard<'_, ProbeMemo> {
    memo.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The normalized `\\server\share` of a UNC path written with either separator; None for any other path.
fn unc_share(dir: &str) -> Option<String> {
    let bytes = dir.as_bytes();
    if bytes.len() < 2 || !matches!(bytes[0], b'\\' | b'/') || !matches!(bytes[1], b'\\' | b'/') {
        return None;
    }
    let normalized = normalize_host_path(&dir.replace('/', "\\"), true);
    let rest = &normalized[2..];
    let end = rest
        .match_indices('\\')
        .nth(1)
        .map_or(rest.len(), |(i, _)| i);
    Some(normalized[..2 + end].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU32;
    use std::time::Instant;

    const DEADLINE: Duration = Duration::from_millis(50);
    const SLOW: Duration = Duration::from_millis(400);

    fn wait_until_settled(prober: &DirProber) {
        let started = Instant::now();
        while !lock(&prober.memo).stalled.is_empty() {
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "probe never finished"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn probe_dir_distinguishes_missing() {
        let temp = std::env::temp_dir();
        assert_eq!(probe_dir(temp.to_str().unwrap()), DirState::Exists);
        let gone = temp.join("bat-session-index-does-not-exist-7f3c");
        assert_eq!(probe_dir(gone.to_str().unwrap()), DirState::Missing);
    }

    #[test]
    fn unc_share_detection() {
        assert_eq!(
            unc_share("\\\\Server\\Share\\dir").as_deref(),
            Some("\\\\server\\share")
        );
        assert_eq!(
            unc_share("//wsl.localhost/Ubuntu/home/a").as_deref(),
            Some("\\\\wsl.localhost\\ubuntu")
        );
        assert_eq!(unc_share("\\\\server").as_deref(), Some("\\\\server"));
        assert_eq!(unc_share("C:\\code"), None);
        assert_eq!(unc_share("/home/a"), None);
    }

    #[test]
    fn slow_probes_time_out_as_unknown_in_parallel_not_missing() {
        static CALLS: AtomicU32 = AtomicU32::new(0);
        let prober = DirProber::new(
            |dir| {
                CALLS.fetch_add(1, Ordering::Relaxed);
                if dir.contains("slow") {
                    thread::sleep(SLOW);
                }
                if dir.contains("gone") {
                    DirState::Missing
                } else {
                    DirState::Exists
                }
            },
            DEADLINE,
        );
        let dirs = [
            "D:\\slow1",
            "D:\\slow2",
            "D:\\slow3",
            "D:\\slow4",
            "D:\\slow-gone",
            "D:\\ok",
            "D:\\gone",
        ];
        let started = Instant::now();
        let states = prober.probe_all(&dirs, PathStyle::Windows, false);
        assert!(
            started.elapsed() < SLOW,
            "slow probes must run in parallel under the deadline, took {:?}",
            started.elapsed()
        );
        for slow in [
            "d:\\slow1",
            "d:\\slow2",
            "d:\\slow3",
            "d:\\slow4",
            "d:\\slow-gone",
        ] {
            assert_eq!(states[slow], DirState::Unknown, "{slow}");
        }
        assert_eq!(states["d:\\ok"], DirState::Exists);
        assert_eq!(states["d:\\gone"], DirState::Missing);
        assert_eq!(CALLS.load(Ordering::Relaxed), 7);

        let again = prober.probe_all(&["D:\\slow1"], PathStyle::Windows, true);
        assert_eq!(again["d:\\slow1"], DirState::Unknown);
        assert_eq!(
            CALLS.load(Ordering::Relaxed),
            7,
            "a still-blocked probe is not started twice, even on refresh"
        );

        wait_until_settled(&prober);
        let local = prober.probe_all(&["D:\\ok"], PathStyle::Windows, false);
        assert_eq!(local["d:\\ok"], DirState::Exists);
        assert_eq!(
            CALLS.load(Ordering::Relaxed),
            8,
            "local paths are probed on every scan"
        );
    }

    #[test]
    fn network_results_are_cached_until_refresh() {
        static CALLS: AtomicU32 = AtomicU32::new(0);
        let prober = DirProber::new(
            |dir| {
                CALLS.fetch_add(1, Ordering::Relaxed);
                if dir.contains("gone") {
                    DirState::Missing
                } else {
                    DirState::Exists
                }
            },
            DEADLINE,
        );
        let dirs = [
            "\\\\srv\\share\\a",
            "//srv/share/gone",
            "\\\\wsl.localhost\\Ubuntu\\home\\me",
            "D:\\local",
        ];
        let first = prober.probe_all(&dirs, PathStyle::Windows, false);
        assert_eq!(CALLS.load(Ordering::Relaxed), 4);
        let second = prober.probe_all(&dirs, PathStyle::Windows, false);
        assert_eq!(
            CALLS.load(Ordering::Relaxed),
            5,
            "only the local path is re-probed"
        );
        assert_eq!(first, second);
        prober.probe_all(&dirs, PathStyle::Windows, true);
        assert_eq!(
            CALLS.load(Ordering::Relaxed),
            9,
            "refresh re-probes network paths"
        );
    }

    #[test]
    fn stalled_share_skips_sibling_probes_and_caches_the_late_result() {
        static CALLS: AtomicU32 = AtomicU32::new(0);
        let prober = DirProber::new(
            |dir| {
                CALLS.fetch_add(1, Ordering::Relaxed);
                if dir.ends_with("slow") {
                    thread::sleep(SLOW);
                }
                DirState::Exists
            },
            DEADLINE,
        );
        let style = PathStyle::Windows;
        let first = prober.probe_all(&["\\\\srv\\share\\slow"], style, false);
        assert_eq!(first["\\\\srv\\share\\slow"], DirState::Unknown);

        let started = Instant::now();
        let during = prober.probe_all(
            &[
                "\\\\srv\\share\\slow",
                "\\\\SRV\\Share\\other",
                "\\\\srv\\other-share\\x",
            ],
            style,
            false,
        );
        assert!(started.elapsed() < SLOW);
        assert_eq!(during["\\\\srv\\share\\slow"], DirState::Unknown);
        assert_eq!(during["\\\\srv\\share\\other"], DirState::Unknown);
        assert_eq!(during["\\\\srv\\other-share\\x"], DirState::Exists);
        assert_eq!(CALLS.load(Ordering::Relaxed), 2);

        wait_until_settled(&prober);
        let after = prober.probe_all(
            &["\\\\srv\\share\\slow", "\\\\srv\\share\\other"],
            style,
            false,
        );
        assert_eq!(
            after["\\\\srv\\share\\slow"],
            DirState::Exists,
            "late result is cached"
        );
        assert_eq!(after["\\\\srv\\share\\other"], DirState::Exists);
        assert_eq!(CALLS.load(Ordering::Relaxed), 3);
    }
}
