//! Reads the current branch from a repository's files, without starting git.

use std::collections::HashSet;
use std::fs;
use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf};

/// The outcome of resolving `git rev-parse --abbrev-ref HEAD` from the repository files.
#[derive(Debug, PartialEq, Eq)]
pub enum HeadLookup {
    /// What git prints: the branch name, or "HEAD" when detached.
    Name(String),
    /// git fails too: no repository, or HEAD names a branch that has no commits yet.
    Missing,
    /// Anything this reader cannot answer with certainty; run git instead.
    AskGit,
}

const GIT_ENV_OVERRIDES: [&str; 5] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_CEILING_DIRECTORIES",
    "GIT_DISCOVERY_ACROSS_FILESYSTEM",
];

pub fn lookup_head(cwd: &Path) -> HeadLookup {
    if GIT_ENV_OVERRIDES
        .iter()
        .any(|name| std::env::var_os(name).is_some())
    {
        return HeadLookup::AskGit;
    }
    lookup_head_below(cwd, None)
}

// `ceiling` stops discovery before it is examined, so tests stay inside a fixture.
fn lookup_head_below(cwd: &Path, ceiling: Option<&Path>) -> HeadLookup {
    // git resolves its working directory to the real path (junctions, symlinks, `..`).
    let cwd = match fs::canonicalize(cwd) {
        Ok(real) => real,
        Err(error) if error.kind() == ErrorKind::NotFound => return HeadLookup::Missing,
        Err(_) => return HeadLookup::AskGit,
    };
    if !cwd.is_dir() {
        return HeadLookup::Missing;
    }
    // A "HEAD" entry makes git read the argument as a path or reject it as ambiguous.
    if fs::symlink_metadata(cwd.join("HEAD")).is_ok() {
        return HeadLookup::AskGit;
    }
    let ceiling = ceiling.and_then(|path| fs::canonicalize(path).ok());
    match discover_git_dir(&cwd, ceiling.as_deref()) {
        Discovery::Found(git_dir) => read_head(&git_dir),
        Discovery::NotFound => HeadLookup::Missing,
        Discovery::Unsure => HeadLookup::AskGit,
    }
}

enum Discovery {
    Found(PathBuf),
    NotFound,
    Unsure,
}

// Follows git's discovery: at each level a `.git` directory or `gitdir:` file, then the
// directory itself as a git dir. Bare repositories are left to git (safe.bareRepository).
fn discover_git_dir(cwd: &Path, ceiling: Option<&Path>) -> Discovery {
    #[cfg(unix)]
    let start_device = device_of(cwd);
    for dir in cwd.ancestors() {
        if ceiling == Some(dir) {
            break;
        }
        // Without GIT_DISCOVERY_ACROSS_FILESYSTEM, git stops at a mount point.
        #[cfg(unix)]
        if device_of(dir) != start_device {
            break;
        }
        let dot_git = dir.join(".git");
        if let Ok(metadata) = fs::metadata(&dot_git) {
            if metadata.is_dir() {
                if is_git_dir(&dot_git) {
                    return Discovery::Found(dot_git);
                }
            } else {
                return match read_gitdir_file(&dot_git, dir) {
                    Some(git_dir) if is_git_dir(&git_dir) => Discovery::Found(git_dir),
                    _ => Discovery::Unsure,
                };
            }
        }
        if is_git_dir(dir) {
            return if dir.file_name().is_some_and(|name| name == ".git") {
                Discovery::Found(dir.to_path_buf())
            } else {
                Discovery::Unsure
            };
        }
    }
    Discovery::NotFound
}

#[cfg(unix)]
fn device_of(path: &Path) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    fs::metadata(path).ok().map(|metadata| metadata.dev())
}

// git accepts only "gitdir: <path>"; anything else is an invalid gitfile to git.
fn read_gitdir_file(dot_git: &Path, dir: &Path) -> Option<PathBuf> {
    let content = fs::read_to_string(dot_git).ok()?;
    let target = content.strip_prefix("gitdir: ")?.trim_end();
    (!target.is_empty()).then(|| join_relative(dir, target))
}

fn is_git_dir(dir: &Path) -> bool {
    if !dir.join("HEAD").is_file() {
        return false;
    }
    let common = common_dir(dir);
    common.join("objects").is_dir() && common.join("refs").is_dir()
}

// A linked worktree's git dir names the shared repository in `commondir`.
fn common_dir(git_dir: &Path) -> PathBuf {
    match fs::read_to_string(git_dir.join("commondir")) {
        Ok(content) if !content.trim().is_empty() => join_relative(git_dir, content.trim()),
        _ => git_dir.to_path_buf(),
    }
}

// Resolves `relative` lexically. Canonical Windows paths are verbatim (`\\?\`), where the
// file system would take `..` and `/` literally instead of as navigation and separators.
fn join_relative(base: &Path, relative: &str) -> PathBuf {
    let relative_path = Path::new(relative);
    if relative_path.has_root() {
        return relative_path.to_path_buf();
    }
    let mut joined = base.to_path_buf();
    for component in relative_path.components() {
        match component {
            Component::ParentDir => {
                joined.pop();
            }
            Component::Normal(part) => joined.push(part),
            _ => {}
        }
    }
    joined
}

fn ref_path(base: &Path, refname: &str) -> PathBuf {
    let mut path = base.to_path_buf();
    for segment in refname.split('/') {
        path.push(segment);
    }
    path
}

fn read_head(git_dir: &Path) -> HeadLookup {
    let common = common_dir(git_dir);
    let Some(object_id_len) = object_id_len(&common) else {
        return HeadLookup::AskGit;
    };
    if common.join("reftable").is_dir() {
        return HeadLookup::AskGit;
    }
    let head_path = git_dir.join("HEAD");
    let is_symlink = fs::symlink_metadata(&head_path)
        .map(|metadata| metadata.file_type().is_symlink())
        .unwrap_or(true);
    if is_symlink {
        return HeadLookup::AskGit;
    }
    let Ok(head) = fs::read_to_string(&head_path) else {
        return HeadLookup::AskGit;
    };
    let head = head.trim();
    let Some(target) = head.strip_prefix("ref:").map(str::trim) else {
        return if is_object_id(head, object_id_len) {
            HeadLookup::Name("HEAD".into())
        } else {
            HeadLookup::AskGit
        };
    };
    let Some(name) = target.strip_prefix("refs/heads/").filter(|name| !name.is_empty()) else {
        return HeadLookup::AskGit;
    };
    let Some(packed) = read_packed_ref_names(&common) else {
        return HeadLookup::AskGit;
    };
    let loose = ref_path(&common, target);
    if loose.is_file() {
        // A symbolic, empty or corrupt loose ref makes git resolve elsewhere or fail.
        let valid = fs::read_to_string(&loose)
            .is_ok_and(|content| is_object_id(content.trim(), object_id_len));
        if !valid {
            return HeadLookup::AskGit;
        }
    } else if !packed.contains(target) {
        return HeadLookup::Missing;
    }
    let exists = |refname: &str| ref_path(&common, refname).exists() || packed.contains(refname);
    // `--abbrev-ref` prints "heads/<name>" when <name> also resolves another way; let git decide.
    let ambiguous = ref_path(git_dir, name).exists()
        || [
            format!("refs/{name}"),
            format!("refs/tags/{name}"),
            format!("refs/remotes/{name}"),
            format!("refs/remotes/{name}/HEAD"),
        ]
        .iter()
        .any(|refname| exists(refname));
    if ambiguous {
        return HeadLookup::AskGit;
    }
    HeadLookup::Name(name.to_string())
}

// Object id length from the repository config. Unknown versions or extensions (including
// ref storage) make git behave differently or refuse the repository, so they return None.
fn object_id_len(common: &Path) -> Option<usize> {
    let config = match fs::read_to_string(common.join("config")) {
        Ok(config) => config,
        Err(error) if error.kind() == ErrorKind::NotFound => return Some(40),
        Err(_) => return None,
    };
    let mut section = String::new();
    let mut version = 0u32;
    let mut object_format = String::from("sha1");
    for line in config.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(header) = line.strip_prefix('[') {
            section = header
                .split(|c: char| c == ']' || c.is_whitespace())
                .next()
                .unwrap_or_default()
                .to_ascii_lowercase();
            continue;
        }
        let (key, value) = match line.split_once('=') {
            Some((key, value)) => (key.trim().to_ascii_lowercase(), value.trim().to_ascii_lowercase()),
            None => (line.to_ascii_lowercase(), String::from("true")),
        };
        match (section.as_str(), key.as_str()) {
            ("core", "repositoryformatversion") => version = value.parse().ok()?,
            ("extensions", "objectformat") => object_format = value,
            ("extensions", "worktreeconfig" | "partialclone" | "preciousobjects" | "noop") => {}
            ("extensions", _) => return None,
            _ => {}
        }
    }
    if version > 1 {
        return None;
    }
    match object_format.as_str() {
        "sha1" => Some(40),
        "sha256" => Some(64),
        _ => None,
    }
}

fn read_packed_ref_names(common: &Path) -> Option<HashSet<String>> {
    match fs::read_to_string(common.join("packed-refs")) {
        Ok(content) => Some(
            content
                .lines()
                .filter(|line| !line.starts_with('#') && !line.starts_with('^'))
                .filter_map(|line| line.split_once(' ').map(|(_, name)| name.trim().to_string()))
                .collect(),
        ),
        Err(error) if error.kind() == ErrorKind::NotFound => Some(HashSet::new()),
        Err(_) => None,
    }
}

fn is_object_id(value: &str, len: usize) -> bool {
    value.len() == len && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::run_git;
    use std::process::{Command, Stdio};
    use std::time::Duration;

    const HASH: &str = "0123456789abcdef0123456789abcdef01234567";

    struct Fixture {
        root: PathBuf,
    }

    impl Fixture {
        fn new(label: &str) -> Self {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or(0);
            let root = std::env::temp_dir().join(format!(
                "bat-head-{label}-{}-{nanos}",
                std::process::id()
            ));
            fs::create_dir_all(&root).expect("create fixture root");
            Fixture { root }
        }

        fn write(&self, relative: &str, content: &str) {
            let path = self.root.join(relative);
            fs::create_dir_all(path.parent().expect("parent")).expect("create parent");
            fs::write(path, content).expect("write fixture file");
        }

        fn git_dir(&self, relative: &str, head: &str) {
            self.write(&format!("{relative}/HEAD"), head);
            fs::create_dir_all(self.root.join(relative).join("objects")).expect("objects");
            fs::create_dir_all(self.root.join(relative).join("refs/heads")).expect("refs");
        }

        fn lookup(&self, relative: &str) -> HeadLookup {
            let cwd = self.root.join(relative);
            fs::create_dir_all(&cwd).expect("create cwd");
            lookup_head_below(&cwd, Some(&self.root))
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn name(value: &str) -> HeadLookup {
        HeadLookup::Name(value.into())
    }

    fn repo_on_main(label: &str) -> Fixture {
        let fixture = Fixture::new(label);
        fixture.git_dir("repo/.git", "ref: refs/heads/main\n");
        fixture.write("repo/.git/refs/heads/main", &format!("{HASH}\n"));
        fixture
    }

    #[test]
    fn reads_a_branch_with_a_loose_ref_from_any_subdirectory() {
        let fixture = Fixture::new("loose");
        fixture.git_dir("repo/.git", "ref: refs/heads/feat/git-graph\n");
        fixture.write("repo/.git/refs/heads/feat/git-graph", &format!("{HASH}\n"));
        assert_eq!(fixture.lookup("repo"), name("feat/git-graph"));
        assert_eq!(fixture.lookup("repo/src/deep"), name("feat/git-graph"));
    }

    #[test]
    fn reads_a_branch_that_only_exists_in_packed_refs() {
        let fixture = Fixture::new("packed");
        fixture.git_dir("repo/.git", "ref: refs/heads/main\n");
        fixture.write(
            "repo/.git/packed-refs",
            &format!("# pack-refs with: peeled fully-peeled sorted\n{HASH} refs/heads/main\n^{HASH}\n"),
        );
        assert_eq!(fixture.lookup("repo"), name("main"));
    }

    #[test]
    fn a_branch_without_commits_is_missing_like_git() {
        let fixture = Fixture::new("unborn");
        fixture.git_dir("repo/.git", "ref: refs/heads/main\n");
        assert_eq!(fixture.lookup("repo"), HeadLookup::Missing);
    }

    #[test]
    fn a_detached_head_matches_the_repository_hash_length() {
        let sha1 = Fixture::new("detached");
        sha1.git_dir("repo/.git", &format!("{HASH}\n"));
        assert_eq!(sha1.lookup("repo"), name("HEAD"));

        let wrong_len = Fixture::new("detached-long");
        wrong_len.git_dir("repo/.git", &"a".repeat(64));
        assert_eq!(wrong_len.lookup("repo"), HeadLookup::AskGit);

        let sha256 = Fixture::new("detached256");
        sha256.git_dir("repo/.git", &"a".repeat(64));
        sha256.write(
            "repo/.git/config",
            "[core]\n\trepositoryformatversion = 1\n[extensions]\n\tobjectformat = sha256\n",
        );
        assert_eq!(sha256.lookup("repo"), name("HEAD"));
    }

    #[test]
    fn follows_a_worktree_gitdir_file_to_the_common_refs() {
        let fixture = Fixture::new("worktree");
        fixture.git_dir("repo/.git", "ref: refs/heads/main\n");
        fixture.write("repo/.git/refs/heads/main", HASH);
        fixture.write("repo/.git/refs/heads/topic", HASH);
        fixture.write("repo/.git/worktrees/wt/HEAD", "ref: refs/heads/topic\n");
        fixture.write("repo/.git/worktrees/wt/commondir", "../..\n");
        fixture.write("wt/.git", "gitdir: ../repo/.git/worktrees/wt\n");
        assert_eq!(fixture.lookup("wt/sub"), name("topic"));
    }

    #[test]
    fn a_cwd_inside_dot_git_reads_its_repository() {
        let fixture = Fixture::new("inside");
        fixture.git_dir("repo/.git", "ref: refs/heads/dev\n");
        fixture.write("repo/.git/refs/heads/dev", HASH);
        assert_eq!(fixture.lookup("repo/.git/refs"), name("dev"));
    }

    #[test]
    fn resolves_paths_with_parent_components_to_the_real_directory() {
        let fixture = repo_on_main("dotdot");
        fs::create_dir_all(fixture.root.join("repo/a")).expect("a");
        fs::create_dir_all(fixture.root.join("outside")).expect("outside");
        assert_eq!(fixture.lookup("repo/a/../a"), name("main"));
        assert_eq!(fixture.lookup("repo/../outside"), HeadLookup::Missing);
    }

    #[cfg(windows)]
    #[test]
    fn follows_a_junction_to_the_repository_it_points_into() {
        let fixture = repo_on_main("junction");
        fixture.git_dir("other/.git", "ref: refs/heads/topic\n");
        fixture.write("other/.git/refs/heads/topic", HASH);
        let target = fixture.root.join("repo").join("src");
        fs::create_dir_all(&target).expect("src");
        let link = fixture.root.join("other").join("link");
        let made = Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(&link)
            .arg(&target)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        if !made {
            eprintln!("skipping: could not create a junction");
            return;
        }
        assert_eq!(fixture.lookup("other/link"), name("main"));
        let _ = fs::remove_dir(&link);
    }

    #[test]
    fn leaves_uncertain_layouts_to_git() {
        let tag = repo_on_main("ambiguous");
        tag.write("repo/.git/packed-refs", &format!("{HASH} refs/tags/main\n"));
        assert_eq!(tag.lookup("repo"), HeadLookup::AskGit, "branch shadowed by a tag");

        let head_entry = repo_on_main("headentry");
        head_entry.write("repo/HEAD", "");
        assert_eq!(head_entry.lookup("repo"), HeadLookup::AskGit, "HEAD file in cwd");

        let symbolic = repo_on_main("symref");
        symbolic.write("repo/.git/refs/heads/main", "ref: refs/heads/other\n");
        assert_eq!(symbolic.lookup("repo"), HeadLookup::AskGit, "symbolic branch ref");

        let empty = repo_on_main("emptyref");
        empty.write("repo/.git/refs/heads/main", "");
        assert_eq!(empty.lookup("repo"), HeadLookup::AskGit, "empty loose ref");

        let reftable = Fixture::new("reftable");
        reftable.git_dir("repo/.git", "ref: refs/heads/.invalid\n");
        fs::create_dir_all(reftable.root.join("repo/.git/reftable")).expect("reftable");
        assert_eq!(reftable.lookup("repo"), HeadLookup::AskGit, "reftable");

        let extension = repo_on_main("extension");
        extension.write(
            "repo/.git/config",
            "[core]\n\trepositoryformatversion = 1\n[extensions]\n\tunknownext = true\n",
        );
        assert_eq!(extension.lookup("repo"), HeadLookup::AskGit, "unknown extension");

        let other = Fixture::new("otherref");
        other.git_dir("repo/.git", "ref: refs/remotes/origin/main\n");
        assert_eq!(other.lookup("repo"), HeadLookup::AskGit, "HEAD outside refs/heads");

        let broken = Fixture::new("brokenlink");
        broken.write("repo/.git", "gitdir: ../missing\n");
        assert_eq!(broken.lookup("repo"), HeadLookup::AskGit, "gitdir to nowhere");

        let nospace = repo_on_main("nospace");
        nospace.write("wt/.git", "gitdir:../repo/.git\n");
        assert_eq!(nospace.lookup("wt"), HeadLookup::AskGit, "gitdir without a space");

        let bare = Fixture::new("bare");
        bare.git_dir("bare.git", "ref: refs/heads/main\n");
        bare.write("bare.git/refs/heads/main", HASH);
        assert_eq!(bare.lookup("bare.git/refs"), HeadLookup::AskGit, "bare repository");
    }

    #[test]
    fn a_directory_outside_any_repository_is_missing() {
        let fixture = Fixture::new("none");
        fixture.write("plain/readme.txt", "not a repo");
        fs::create_dir_all(fixture.root.join("plain/.git")).expect("invalid .git dir");
        assert_eq!(fixture.lookup("plain"), HeadLookup::Missing);
        assert_eq!(
            lookup_head_below(&fixture.root.join("does-not-exist"), Some(&fixture.root)),
            HeadLookup::Missing
        );
    }

    #[test]
    fn agrees_with_git_rev_parse_in_a_real_repository() {
        let git_available = Command::new("git")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        if !git_available {
            eprintln!("skipping: git is not installed on this machine");
            return;
        }
        let fixture = Fixture::new("real");
        let repo = fixture.root.join("repo");
        fs::create_dir_all(repo.join("src")).expect("create repo");
        let hooks = format!("core.hooksPath={}", fixture.root.join("no-hooks").display());
        let timeout = Duration::from_secs(60);
        let git = |cwd: &Path, args: &[&str]| {
            let mut full = vec![
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "init.defaultBranch=main",
                "-c",
                hooks.as_str(),
            ];
            full.extend_from_slice(args);
            run_git(cwd.to_str().expect("utf-8 path"), &full, timeout)
        };
        let rev_parse = |cwd: &Path| {
            run_git(
                cwd.to_str().expect("utf-8 path"),
                &["rev-parse", "--abbrev-ref", "HEAD"],
                timeout,
            )
            .map(|out| out.trim().to_string())
        };
        let lookup = |cwd: &Path| lookup_head_below(cwd, Some(&fixture.root));
        let expect_same = |cwd: &Path, label: &str| match (lookup(cwd), rev_parse(cwd)) {
            (HeadLookup::Name(native), Some(git_name)) => assert_eq!(native, git_name, "{label}"),
            (HeadLookup::Missing, None) => {}
            (native, git_name) => panic!("{label}: native {native:?}, git {git_name:?}"),
        };

        assert!(git(&repo, &["init", "-q"]).is_some(), "git init");
        expect_same(&repo, "unborn branch");
        assert!(git(&repo, &["commit", "-q", "--allow-empty", "-m", "first"]).is_some());
        expect_same(&repo, "main");
        expect_same(&repo.join("src"), "subdirectory");
        assert!(git(&repo, &["switch", "-q", "-c", "feat/x"]).is_some());
        expect_same(&repo, "nested branch name");
        assert!(git(&repo, &["pack-refs", "--all"]).is_some());
        expect_same(&repo, "packed ref");
        let worktree = fixture.root.join("wt");
        let worktree_arg = worktree.to_str().expect("utf-8 path");
        assert!(git(&repo, &["worktree", "add", "-q", "-b", "wt-branch", worktree_arg]).is_some());
        expect_same(&worktree, "linked worktree");
        assert!(git(&repo, &["switch", "-q", "--detach"]).is_some());
        expect_same(&repo, "detached HEAD");
        assert!(git(&repo, &["switch", "-q", "feat/x"]).is_some());
        assert!(git(&repo, &["tag", "feat/x"]).is_some());
        assert_eq!(lookup(&repo), HeadLookup::AskGit, "branch name shadowed by a tag");
        assert_eq!(rev_parse(&repo).as_deref(), Some("heads/feat/x"));
        fs::write(repo.join("HEAD"), "").expect("write HEAD file");
        assert_eq!(lookup(&repo), HeadLookup::AskGit, "HEAD file in the working tree");

        let unborn = fixture.root.join("unborn");
        fs::create_dir_all(&unborn).expect("create unborn repo");
        assert!(git(&unborn, &["init", "-q"]).is_some(), "git init unborn");
        fs::write(unborn.join("HEAD"), "").expect("write HEAD file");
        assert_eq!(lookup(&unborn), HeadLookup::AskGit, "unborn branch with a HEAD file");
        assert_eq!(rev_parse(&unborn).as_deref(), Some("HEAD"), "git reads HEAD as a path");
    }
}
