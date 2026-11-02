use std::collections::HashMap;
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use bat_host_support::subprocess::hide_console_window;

use crate::paths::{
    display_name, normalize_path, strip_verbatim_prefix, to_host_separators, PathStyle,
};

const GIT_TIMEOUT: Duration = Duration::from_secs(5);
const GIT_POLL_INTERVAL: Duration = Duration::from_millis(10);
const RESOLVE_CONCURRENCY: usize = 4;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RepoInfo {
    /// Normalized git common dir; identical for a repository and all of its linked worktrees.
    pub key: String,
    pub root: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DirRepo {
    pub repo: RepoInfo,
    /// Normalized top level of the checkout containing the directory; None inside a bare repo or `.git`.
    pub worktree_key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum GitOutcome {
    Found {
        common_dir: String,
        toplevel: Option<String>,
    },
    NoRepository,
    Unavailable,
}

#[derive(Default)]
pub(crate) struct GitResolver {
    /// None records a directory where git found no repository, failed, or timed out.
    resolved: HashMap<String, Option<DirRepo>>,
    available: Option<bool>,
}

impl GitResolver {
    /// Misses are retried only when `retry_misses` is set, so automatic refreshes do not respawn git for them.
    pub(crate) fn resolve_all(
        &mut self,
        dirs: &[&str],
        style: PathStyle,
        retry_misses: bool,
        run: &(dyn Fn(&str) -> GitOutcome + Sync),
    ) {
        if retry_misses {
            self.resolved.retain(|_, info| info.is_some());
            if self.available == Some(false) {
                self.available = None;
            }
        }
        if self.available == Some(false) {
            return;
        }
        let mut pending: Vec<(String, &str)> = Vec::new();
        for dir in dirs {
            let key = normalize_path(dir, style);
            if !self.resolved.contains_key(&key) && !pending.iter().any(|(k, _)| *k == key) {
                pending.push((key, dir));
            }
        }
        let next = AtomicUsize::new(0);
        let outcomes: Vec<(String, GitOutcome)> = thread::scope(|scope| {
            let workers: Vec<_> = (0..RESOLVE_CONCURRENCY.min(pending.len()))
                .map(|_| {
                    scope.spawn(|| {
                        let mut found = Vec::new();
                        while let Some((key, dir)) =
                            pending.get(next.fetch_add(1, Ordering::Relaxed))
                        {
                            found.push((key.clone(), run(dir)));
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
        for (key, outcome) in outcomes {
            match outcome {
                GitOutcome::Found {
                    common_dir,
                    toplevel,
                } => {
                    self.available = Some(true);
                    let common_dir = strip_verbatim_prefix(&common_dir);
                    let toplevel = toplevel.map(|top| strip_verbatim_prefix(&top));
                    let found =
                        to_repo_info(&common_dir, toplevel.as_deref(), style).map(|repo| DirRepo {
                            repo,
                            worktree_key: toplevel.map(|top| normalize_path(&top, style)),
                        });
                    self.resolved.insert(key, found);
                }
                GitOutcome::NoRepository => {
                    self.available = Some(true);
                    self.resolved.insert(key, None);
                }
                GitOutcome::Unavailable => self.available = Some(false),
            }
        }
    }

    pub(crate) fn lookup(&self, dir: &str, style: PathStyle) -> Option<&DirRepo> {
        self.resolved.get(&normalize_path(dir, style))?.as_ref()
    }

    /// Probes `git --version` only when no lookup has shown whether git can be launched.
    pub(crate) fn git_available(&mut self, probe: impl FnOnce() -> bool) -> bool {
        *self.available.get_or_insert_with(probe)
    }
}

pub(crate) fn process_resolver() -> MutexGuard<'static, GitResolver> {
    static RESOLVER: OnceLock<Mutex<GitResolver>> = OnceLock::new();
    RESOLVER
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The root must be a working tree because it becomes a workspace folder. A common dir not named `.git`
/// (submodule, `--separate-git-dir`, bare repo with worktrees) has no checkout beside it, so the directory's own
/// top level is used; with no top level either (inside a bare repo or git dir) the directory is not a repo group.
fn to_repo_info(common_dir: &str, toplevel: Option<&str>, style: PathStyle) -> Option<RepoInfo> {
    let trimmed = common_dir.trim_end_matches(['\\', '/']);
    let checkout_parent = display_name(trimmed)
        .eq_ignore_ascii_case(".git")
        .then(|| trimmed[..trimmed.len() - ".git".len()].trim_end_matches(['\\', '/']))
        .filter(|parent| !parent.is_empty());
    let root = checkout_parent
        .or(toplevel)
        .filter(|root| !root.is_empty())?;
    let root = to_host_separators(root, style);
    Some(RepoInfo {
        key: normalize_path(trimmed, style),
        name: display_name(&root),
        root,
    })
}

fn git_command(args: &[&str]) -> Command {
    let mut command = Command::new("git");
    command
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    hide_console_window(&mut command);
    command
}

/// `--path-format` needs git 2.31+. Inside a bare repo or `.git`, git prints the common dir and then fails on
/// `--show-toplevel`, so the first line is still used.
pub(crate) fn run_git_common_dir(dir: &str) -> GitOutcome {
    let args = [
        "-C",
        dir,
        "rev-parse",
        "--path-format=absolute",
        "--git-common-dir",
        "--show-toplevel",
    ];
    let Ok(mut child) = git_command(&args).spawn() else {
        return GitOutcome::Unavailable;
    };
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < GIT_TIMEOUT => thread::sleep(GIT_POLL_INTERVAL),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return GitOutcome::NoRepository;
            }
        }
    };
    let mut stdout = String::new();
    if let Some(mut pipe) = child.stdout.take() {
        let _ = pipe.read_to_string(&mut stdout);
    }
    parse_rev_parse_output(&stdout, status.success())
}

pub(crate) fn parse_rev_parse_output(stdout: &str, success: bool) -> GitOutcome {
    let mut lines = stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty());
    let Some(common_dir) = lines.next().filter(|line| !line.starts_with('-')) else {
        return GitOutcome::NoRepository;
    };
    let toplevel = if success {
        lines.next().map(str::to_string)
    } else {
        None
    };
    GitOutcome::Found {
        common_dir: common_dir.to_string(),
        toplevel,
    }
}

pub(crate) fn probe_git_version() -> bool {
    git_command(&["--version"])
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::test_support::TempDir;
    use std::fs;
    use std::path::Path;
    use std::sync::atomic::AtomicU32;

    #[test]
    fn resolves_once_per_normalized_dir_and_retries_misses_on_request() {
        let calls = AtomicU32::new(0);
        let run = |dir: &str| {
            calls.fetch_add(1, Ordering::Relaxed);
            if dir
                .to_lowercase()
                .replace('/', "\\")
                .starts_with("d:\\code\\app")
            {
                GitOutcome::Found {
                    common_dir: "D:/code/app/.git".into(),
                    toplevel: Some("D:/code/app".into()),
                }
            } else {
                GitOutcome::NoRepository
            }
        };
        let mut resolver = GitResolver::default();
        resolver.resolve_all(
            &["D:\\code\\app", "d:/code/app/", "D:\\code\\none"],
            PathStyle::Windows,
            false,
            &run,
        );
        assert_eq!(calls.load(Ordering::Relaxed), 2);
        let found = resolver
            .lookup("D:\\CODE\\APP", PathStyle::Windows)
            .unwrap();
        assert_eq!(
            found.repo,
            RepoInfo {
                key: "d:\\code\\app\\.git".into(),
                root: "D:\\code\\app".into(),
                name: "app".into()
            }
        );
        assert_eq!(found.worktree_key.as_deref(), Some("d:\\code\\app"));
        assert_eq!(resolver.lookup("D:\\code\\none", PathStyle::Windows), None);

        resolver.resolve_all(&["D:\\code\\none"], PathStyle::Windows, false, &run);
        assert_eq!(calls.load(Ordering::Relaxed), 2);
        resolver.resolve_all(&["D:\\code\\none"], PathStyle::Windows, true, &run);
        assert_eq!(calls.load(Ordering::Relaxed), 3);
        assert!(resolver.git_available(|| unreachable!()));
    }

    #[test]
    fn unavailable_git_is_not_cached_as_a_miss() {
        let mut resolver = GitResolver::default();
        let posix = PathStyle::Posix {
            case_insensitive: false,
        };
        resolver.resolve_all(&["/a", "/b"], posix, false, &|_| GitOutcome::Unavailable);
        assert!(!resolver.git_available(|| unreachable!()));
        resolver.resolve_all(&["/a"], posix, false, &|_| unreachable!());
        resolver.resolve_all(&["/a"], posix, true, &|_| GitOutcome::NoRepository);
        assert!(resolver.git_available(|| unreachable!()));
    }

    #[test]
    fn rev_parse_output_parsing() {
        assert_eq!(
            parse_rev_parse_output("C:/r/.git\nC:/r\n", true),
            GitOutcome::Found {
                common_dir: "C:/r/.git".into(),
                toplevel: Some("C:/r".into())
            }
        );
        assert_eq!(
            parse_rev_parse_output("C:/r.git\n", false),
            GitOutcome::Found {
                common_dir: "C:/r.git".into(),
                toplevel: None
            }
        );
        assert_eq!(parse_rev_parse_output("", false), GitOutcome::NoRepository);
        assert_eq!(
            parse_rev_parse_output("--path-format=absolute\n", true),
            GitOutcome::NoRepository
        );
    }

    #[test]
    fn root_is_a_working_tree_never_a_git_internal_dir() {
        let posix = PathStyle::Posix {
            case_insensitive: false,
        };
        let root_of = |common: &str, top: Option<&str>| {
            to_repo_info(common, top, posix).map(|info| (info.key, info.root, info.name))
        };
        assert_eq!(
            root_of("/code/app/.git/", Some("/wt/app-fix")),
            Some(("/code/app/.git".into(), "/code/app".into(), "app".into()))
        );
        assert_eq!(
            root_of("/code/app/.git", None),
            Some(("/code/app/.git".into(), "/code/app".into(), "app".into()))
        );
        assert_eq!(
            root_of("/super/.git/modules/lib", Some("/super/lib")),
            Some((
                "/super/.git/modules/lib".into(),
                "/super/lib".into(),
                "lib".into()
            ))
        );
        assert_eq!(
            root_of("/proj/.bare", Some("/proj/feature")),
            Some((
                "/proj/.bare".into(),
                "/proj/feature".into(),
                "feature".into()
            ))
        );
        assert_eq!(root_of("/srv/repo.git/", None), None);
        assert_eq!(
            root_of("/.git", Some("/")),
            Some(("/.git".into(), "/".into(), "/".into()))
        );
        let windows = to_repo_info(
            "C:/super/.git/modules/lib",
            Some("C:/super/lib"),
            PathStyle::Windows,
        )
        .unwrap();
        assert_eq!(windows.key, "c:\\super\\.git\\modules\\lib");
        assert_eq!(windows.root, "C:\\super\\lib");
    }

    #[test]
    fn verbatim_common_dir_is_normalized_like_toplevel() {
        let mut resolver = GitResolver::default();
        resolver.resolve_all(&["C:\\r"], PathStyle::Windows, false, &|_| {
            GitOutcome::Found {
                common_dir: "\\\\?\\C:\\r\\.git".into(),
                toplevel: Some("\\\\?\\C:\\r".into()),
            }
        });
        let found = resolver.lookup("C:\\r", PathStyle::Windows).unwrap();
        assert_eq!(found.repo.key, "c:\\r\\.git");
        assert_eq!(found.repo.root, "C:\\r");
        assert_eq!(found.worktree_key.as_deref(), Some("c:\\r"));
    }

    pub(crate) fn git_unavailable_reason() -> Option<String> {
        match Command::new("git").arg("--version").output() {
            Ok(output) if output.status.success() => None,
            Ok(output) => Some(format!("git --version exited with {}", output.status)),
            Err(error) => Some(format!("git is not runnable: {error}")),
        }
    }

    pub(crate) fn git(cwd: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(cwd)
            .args(args)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed in {}", cwd.display());
    }

    pub(crate) fn make_repo_with_worktree(root: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
        let main = root.join("main");
        let linked = root.join("linked");
        fs::create_dir_all(&main).unwrap();
        git(&main, &["init", "-q", "-b", "main"]);
        fs::write(main.join("readme.txt"), "x").unwrap();
        git(&main, &["add", "readme.txt"]);
        git(
            &main,
            &[
                "-c",
                "user.name=test",
                "-c",
                "user.email=test@example.invalid",
                "commit",
                "-q",
                "-m",
                "init",
            ],
        );
        git(
            &main,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "side",
                linked.to_str().unwrap(),
            ],
        );
        (main, linked)
    }

    #[test]
    fn real_git_worktree_resolves_to_same_repository() {
        if let Some(reason) = git_unavailable_reason() {
            eprintln!("skipping real git test: {reason}");
            return;
        }
        let dir = TempDir::new("git-real");
        let (main, linked) = make_repo_with_worktree(dir.path());
        let style = PathStyle::host();
        let missing = dir.path().join("not-a-repo");
        fs::create_dir_all(&missing).unwrap();
        let paths = [
            main.to_str().unwrap(),
            linked.to_str().unwrap(),
            missing.to_str().unwrap(),
        ];
        let mut resolver = GitResolver::default();
        resolver.resolve_all(&paths, style, false, &run_git_common_dir);

        let main_repo = resolver.lookup(paths[0], style).unwrap().clone();
        let linked_repo = resolver.lookup(paths[1], style).unwrap().clone();
        assert_eq!(main_repo.repo, linked_repo.repo);
        assert_eq!(main_repo.repo.name, "main");
        assert_eq!(
            normalize_path(&main_repo.repo.root, style),
            normalize_path(paths[0], style)
        );
        assert_eq!(
            main_repo.worktree_key,
            Some(normalize_path(paths[0], style))
        );
        assert_eq!(
            linked_repo.worktree_key,
            Some(normalize_path(paths[1], style))
        );
        assert_eq!(resolver.lookup(paths[2], style), None);
        assert!(resolver.git_available(|| unreachable!()));
    }

    fn git_path(path: &Path) -> String {
        path.to_str().unwrap().replace('\\', "/")
    }

    #[test]
    fn real_git_submodule_separate_git_dir_and_bare_worktrees_root_at_a_checkout() {
        if let Some(reason) = git_unavailable_reason() {
            eprintln!("skipping real git test: {reason}");
            return;
        }
        let dir = TempDir::new("git-layouts");
        let root = dir.path();
        let (origin, _) = make_repo_with_worktree(&root.join("origin"));

        let separate_work = root.join("separate-work");
        let separate_meta = root.join("meta-store").join("separate");
        fs::create_dir_all(separate_meta.parent().unwrap()).unwrap();
        git(
            root,
            &[
                "init",
                "-q",
                "--separate-git-dir",
                &git_path(&separate_meta),
                &git_path(&separate_work),
            ],
        );

        let superproject = root.join("super");
        fs::create_dir_all(&superproject).unwrap();
        git(&superproject, &["init", "-q"]);
        git(
            &superproject,
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                "-q",
                &git_path(&origin),
                "lib",
            ],
        );
        let submodule = superproject.join("lib");
        let submodule_nested = submodule.join("nested");
        fs::create_dir_all(&submodule_nested).unwrap();

        let project = root.join("proj");
        let bare = project.join(".bare");
        let feature = project.join("feature");
        let fix = project.join("fix");
        git(
            root,
            &[
                "clone",
                "-q",
                "--bare",
                &git_path(&origin),
                &git_path(&bare),
            ],
        );
        git(&bare, &["worktree", "add", "-q", &git_path(&feature)]);
        git(&bare, &["worktree", "add", "-q", &git_path(&fix)]);

        let style = PathStyle::host();
        let paths = [
            separate_work.to_str().unwrap(),
            submodule_nested.to_str().unwrap(),
            feature.to_str().unwrap(),
            fix.to_str().unwrap(),
            bare.to_str().unwrap(),
        ];
        let mut resolver = GitResolver::default();
        resolver.resolve_all(&paths, style, false, &run_git_common_dir);
        let norm = |path: &Path| normalize_path(path.to_str().unwrap(), style);
        let found = |index: usize| resolver.lookup(paths[index], style).unwrap().clone();

        let separate = found(0);
        assert_eq!(separate.repo.key, norm(&separate_meta));
        assert_eq!(
            normalize_path(&separate.repo.root, style),
            norm(&separate_work)
        );
        assert_eq!(separate.repo.name, "separate-work");

        let sub = found(1);
        assert_eq!(
            sub.repo.key,
            norm(&superproject.join(".git").join("modules").join("lib"))
        );
        assert_eq!(normalize_path(&sub.repo.root, style), norm(&submodule));
        assert_eq!(sub.repo.name, "lib");
        assert_eq!(sub.worktree_key, Some(norm(&submodule)));

        let (feature_repo, fix_repo) = (found(2), found(3));
        assert_eq!(feature_repo.repo.key, norm(&bare));
        assert_eq!(fix_repo.repo.key, feature_repo.repo.key);
        assert_eq!(
            normalize_path(&feature_repo.repo.root, style),
            norm(&feature)
        );
        assert_eq!(normalize_path(&fix_repo.repo.root, style), norm(&fix));
        assert_eq!(resolver.lookup(paths[4], style), None);
    }
}
