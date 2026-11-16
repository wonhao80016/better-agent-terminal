//! Text diffs for UTF-16 files, which git itself reports as binary.

use crate::git::{
    build_no_index_diff_args, build_show_file_args, decode_text_blob, has_utf16_bom,
    is_safe_worktree_path, rewrite_no_index_diff, run_git, run_git_output, MAX_OUTPUT_BYTES,
};
use bat_host_support::path_guard::{is_sensitive_path, resolve_transfer_path};
use std::ffi::{OsStr, OsString};
use std::fs::{File, OpenOptions};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const SIDE_TIMEOUT: Duration = Duration::from_secs(10);

/// Outer None: the side cannot be compared (too large or unreadable); inner None: it does not exist.
pub type DiffSide = Option<Option<Vec<u8>>>;

fn read_blob_side(cwd: &str, rev: &str, path: &str, max_bytes: usize) -> DiffSide {
    let argv = build_show_file_args(rev, path)?;
    let refs: Vec<&str> = argv.iter().map(String::as_str).collect();
    match run_git_output(cwd, &refs, SIDE_TIMEOUT, max_bytes)? {
        (Some(0), bytes) => Some(Some(bytes)),
        _ => Some(None),
    }
}

// Canonical names are compared exactly, except that Windows matches ASCII letters in either case
// (drive letters, and names that may come back differently cased); no Unicode case folding.
fn same_name(a: &OsStr, b: &OsStr) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let fold = |unit: u16| match u8::try_from(unit) {
            Ok(byte) => u16::from(byte.to_ascii_lowercase()),
            Err(_) => unit,
        };
        a.encode_wide().map(fold).eq(b.encode_wide().map(fold))
    }
    #[cfg(not(windows))]
    {
        a == b
    }
}

/// Components of `target` below `top` (both canonical), or None unless `target` is strictly inside.
pub fn components_inside(top: &Path, target: &Path) -> Option<Vec<OsString>> {
    let mut rest = target.components();
    for part in top.components() {
        if !same_name(part.as_os_str(), rest.next()?.as_os_str()) {
            return None;
        }
    }
    let inside: Vec<OsString> = rest.map(|part| part.as_os_str().to_os_string()).collect();
    (!inside.is_empty()).then_some(inside)
}

fn is_same_or_inside(dir: &Path, target: &Path) -> bool {
    components_inside(dir, target).is_some()
        || (dir.components().count() == target.components().count()
            && dir
                .components()
                .zip(target.components())
                .all(|(a, b)| same_name(a.as_os_str(), b.as_os_str())))
}

/// Windows ignores trailing dots and spaces, so `.git.` and `.GIT ` name the same directory.
pub fn names_git_dir(parts: &[OsString]) -> bool {
    parts.iter().any(|part| {
        part.to_string_lossy()
            .trim_end_matches(['.', ' '])
            .eq_ignore_ascii_case(".git")
    })
}

/// Canonical work tree root plus the repository's git and common dirs, which may live inside it
/// under any name (`git init --separate-git-dir`) or elsewhere (linked worktrees).
pub struct WorktreeLayout {
    pub top: PathBuf,
    pub git_dirs: Vec<PathBuf>,
}

// One `git rev-parse` prints the toplevel, the absolute git dir and the common dir, one per line;
// the common dir can be relative to `cwd`. Only line terminators are trimmed, so names keep spaces.
pub fn worktree_layout(cwd: &str) -> Option<WorktreeLayout> {
    let output = run_git(
        cwd,
        &["rev-parse", "--show-toplevel", "--absolute-git-dir", "--git-common-dir"],
        SIDE_TIMEOUT,
    )?;
    let mut lines = output.lines().map(|line| line.trim_end_matches('\r'));
    let (top, git_dir, common_dir) = (lines.next()?, lines.next()?, lines.next()?);
    if top.is_empty() || git_dir.is_empty() || common_dir.is_empty() {
        return None;
    }
    let common_dir = Path::new(common_dir);
    let common_dir = if common_dir.is_absolute() {
        common_dir.to_path_buf()
    } else {
        Path::new(cwd).join(common_dir)
    };
    Some(WorktreeLayout {
        top: std::fs::canonicalize(top).ok()?,
        git_dirs: vec![
            std::fs::canonicalize(git_dir).ok()?,
            std::fs::canonicalize(common_dir).ok()?,
        ],
    })
}

// The fs:readFile deny-list, checked on the given name and on its resolved name.
fn is_allowed_target(layout: &WorktreeLayout, target: &Path) -> bool {
    match components_inside(&layout.top, target) {
        Some(parts) => {
            !names_git_dir(&parts)
                && !layout.git_dirs.iter().any(|dir| is_same_or_inside(dir, target))
                && !is_sensitive_path(&target.to_string_lossy())
                && resolve_transfer_path(target).is_ok()
        }
        None => false,
    }
}

#[cfg(windows)]
fn open_without_following(path: &Path) -> std::io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;
    OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
}

// O_NONBLOCK keeps a FIFO from blocking the open before the regular-file check; it does not
// change reads of regular files.
#[cfg(unix)]
fn open_without_following(path: &Path) -> std::io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
}

// The path the open handle refers to, which directory junctions or symlinks swapped in after
// the containment check would change.
#[cfg(windows)]
fn opened_file_path(file: &File, _requested: &Path) -> Option<PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        GetFinalPathNameByHandleW, FILE_NAME_NORMALIZED, VOLUME_NAME_DOS,
    };
    let mut buffer = vec![0u16; 512];
    loop {
        // SAFETY: the handle stays open for the call and `buffer` holds `buffer.len()` units.
        let length = unsafe {
            GetFinalPathNameByHandleW(
                file.as_raw_handle(),
                buffer.as_mut_ptr(),
                buffer.len() as u32,
                FILE_NAME_NORMALIZED | VOLUME_NAME_DOS,
            )
        } as usize;
        if length == 0 {
            return None;
        }
        if length < buffer.len() {
            return Some(PathBuf::from(OsString::from_wide(&buffer[..length])));
        }
        buffer.resize(length + 1, 0);
    }
}

// Unix has no handle-to-path call: re-resolve the name and require it to be the opened inode.
#[cfg(unix)]
fn opened_file_path(file: &File, requested: &Path) -> Option<PathBuf> {
    use std::os::unix::fs::MetadataExt;
    let opened = file.metadata().ok()?;
    let resolved = std::fs::canonicalize(requested).ok()?;
    let named = std::fs::metadata(&resolved).ok()?;
    (opened.dev() == named.dev() && opened.ino() == named.ino()).then_some(resolved)
}

/// Reads repository-relative `path` from the work tree of `cwd` if its canonical target is a
/// regular file of at most `max_bytes` inside the work tree, outside every git dir and `.git`
/// name, and allowed by the fs:readFile deny-list; the opened handle's path is checked again.
pub fn read_worktree_file(cwd: &str, path: &str, max_bytes: usize) -> DiffSide {
    if !is_safe_worktree_path(path) {
        return None;
    }
    let layout = worktree_layout(cwd)?;
    let canonical = match std::fs::canonicalize(layout.top.join(path)) {
        Ok(canonical) => canonical,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Some(None),
        Err(_) => return None,
    };
    if !is_allowed_target(&layout, &canonical) {
        return None;
    }
    let file = open_without_following(&canonical).ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > max_bytes as u64 {
        return None;
    }
    let opened = opened_file_path(&file, &canonical)?;
    if !is_allowed_target(&layout, &opened) {
        return None;
    }
    let mut bytes = Vec::new();
    (&file).take(max_bytes as u64 + 1).read_to_end(&mut bytes).ok()?;
    (bytes.len() <= max_bytes).then_some(Some(bytes))
}

fn create_private_dir(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new().mode(0o700).create(path)
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir(path)
    }
}

// Unique per call, created in the OS temp dir (owner-only on Unix) and removed on drop; never
// named after repo paths.
struct TempDiffDir(PathBuf);

impl TempDiffDir {
    fn create() -> Option<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default();
        for _ in 0..8 {
            let sequence = NEXT.fetch_add(1, Ordering::Relaxed);
            let name = format!("bat-git-text-diff-{}-{nanos}-{sequence}", std::process::id());
            let path = std::env::temp_dir().join(name);
            match create_private_dir(&path) {
                Ok(()) => return Some(Self(path)),
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(_) => return None,
            }
        }
        None
    }

    fn write(&self, name: &str, text: &str) -> Option<String> {
        let path = self.0.join(name);
        std::fs::write(&path, text).ok()?;
        Some(path.to_string_lossy().into_owned())
    }
}

impl Drop for TempDiffDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// When either side of `file` (paired with `old_file` for a rename or copy) starts with a UTF-16
/// BOM, diffs the decoded texts, the same decoding the File view uses, so its change marks line
/// up. None when neither side is UTF-16 or a side is unreadable; the caller keeps git's output.
pub fn decoded_text_diff(
    cwd: &str,
    commit_hash: Option<&str>,
    file: &str,
    old_file: Option<&str>,
    max_bytes: usize,
) -> Option<String> {
    let old_path = old_file.filter(|path| !path.is_empty()).unwrap_or(file);
    let commit = commit_hash.filter(|hash| !hash.is_empty() && *hash != "working");
    let (old_side, new_side) = match commit {
        Some(hash) => (
            read_blob_side(cwd, &format!("{hash}~1"), old_path, max_bytes)?,
            read_blob_side(cwd, hash, file, max_bytes)?,
        ),
        None => (
            read_blob_side(cwd, "HEAD", old_path, max_bytes)?,
            read_worktree_file(cwd, file, max_bytes)?,
        ),
    };
    let is_utf16 = |side: &Option<Vec<u8>>| side.as_deref().is_some_and(has_utf16_bom);
    if !is_utf16(&old_side) && !is_utf16(&new_side) {
        return None;
    }
    let decode = |side: Option<Vec<u8>>| match side {
        Some(bytes) => decode_text_blob(bytes, max_bytes).map(Some),
        None => Some(None),
    };
    let old_text = decode(old_side)?;
    let new_text = decode(new_side)?;
    let dir = TempDiffDir::create()?;
    let old_temp = dir.write("old.txt", old_text.as_deref().unwrap_or_default())?;
    let new_temp = dir.write("new.txt", new_text.as_deref().unwrap_or_default())?;
    let attributes = dir.write("attributes", "")?;
    let argv = build_no_index_diff_args(&old_temp, &new_temp, &attributes);
    let refs: Vec<&str> = argv.iter().map(String::as_str).collect();
    let dir_path = dir.0.to_string_lossy().into_owned();
    let (code, output) = run_git_output(&dir_path, &refs, SIDE_TIMEOUT, MAX_OUTPUT_BYTES)?;
    if !matches!(code, Some(0) | Some(1)) {
        return None;
    }
    Some(rewrite_no_index_diff(
        &String::from_utf8_lossy(&output),
        old_text.is_some().then_some(old_path),
        new_text.is_some().then_some(file),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temp_diff_dir_is_unique_and_removed_on_drop() {
        let first = TempDiffDir::create().expect("temp dir");
        let second = TempDiffDir::create().expect("temp dir");
        assert_ne!(first.0, second.0);
        assert!(first.0.starts_with(std::env::temp_dir()));
        let written = first.write("old.txt", "a\r\nb\n").expect("write");
        assert_eq!(std::fs::read_to_string(&written).unwrap(), "a\r\nb\n");
        let path = first.0.clone();
        drop(first);
        assert!(!path.exists());
        drop(second);
    }

    fn os(parts: &[&str]) -> Vec<OsString> {
        parts.iter().map(OsString::from).collect()
    }

    #[test]
    fn components_inside_requires_a_strict_descendant() {
        let top = Path::new("/repo/work");
        assert_eq!(components_inside(top, Path::new("/repo/work/a/b.txt")), Some(os(&["a", "b.txt"])));
        assert_eq!(components_inside(top, Path::new("/repo/work")), None);
        assert_eq!(components_inside(top, Path::new("/repo/worktree/a.txt")), None);
        assert_eq!(components_inside(top, Path::new("/repo/other/a.txt")), None);
        assert_eq!(components_inside(top, Path::new("/repo")), None);
    }

    #[cfg(windows)]
    #[test]
    fn components_inside_ignores_case_on_windows() {
        let top = Path::new(r"\\?\C:\Repo\Work");
        assert_eq!(
            components_inside(top, Path::new(r"\\?\c:\repo\WORK\Sub\a.inf")),
            Some(os(&["Sub", "a.inf"]))
        );
        assert_eq!(components_inside(top, Path::new(r"C:\Repo\Work\a.inf")), None);
    }

    #[test]
    fn names_git_dir_matches_every_spelling() {
        for parts in [&[".git", "config"][..], &["sub", ".GIT", "HEAD"], &[".git.", "config"], &[".Git ", "x"], &[".git"]] {
            assert!(names_git_dir(&os(parts)), "{parts:?}");
        }
        for parts in [&["a.git", "x"][..], &[".github", "workflows", "ci.yml"], &[".gitignore"], &["git", "x"]] {
            assert!(!names_git_dir(&os(parts)), "{parts:?}");
        }
    }

    struct Fixture {
        root: PathBuf,
        links: Vec<PathBuf>,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            // Links go first and are removed as links, so cleanup never walks into their targets.
            for link in &self.links {
                let _ = std::fs::remove_dir(link).or_else(|_| std::fs::remove_file(link));
            }
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn fixture() -> (Fixture, String) {
        let dir = TempDiffDir::create().expect("temp dir");
        let root = dir.0.clone();
        std::mem::forget(dir);
        let repo = root.join("repo");
        let outside = root.join("outside");
        std::fs::create_dir_all(repo.join("sub")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret.txt"), "outside secret").unwrap();
        std::fs::write(repo.join("sub").join("ok.txt"), "inside").unwrap();
        let status = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&repo)
            .status()
            .unwrap();
        assert!(status.success());
        (Fixture { root, links: Vec::new() }, repo.to_string_lossy().into_owned())
    }

    #[test]
    fn read_worktree_file_reads_inside_and_reports_missing_files() {
        let (_fixture, repo) = fixture();
        assert_eq!(read_worktree_file(&repo, "sub/ok.txt", 64), Some(Some(b"inside".to_vec())));
        assert_eq!(read_worktree_file(&repo, "sub/missing.txt", 64), Some(None));
        assert_eq!(read_worktree_file(&repo, "sub/ok.txt", 3), None);
    }

    #[test]
    fn read_worktree_file_refuses_git_metadata_in_any_spelling() {
        let (_fixture, repo) = fixture();
        for path in [".git/config", ".GIT/config", ".git\\config", ".git/HEAD", ".git./config", "sub/../.git/config"] {
            let read = read_worktree_file(&repo, path, 4096);
            assert!(!matches!(read, Some(Some(_))), "{path} returned content: {read:?}");
        }
    }

    #[test]
    fn read_worktree_file_refuses_lexical_escapes_and_streams() {
        let (_fixture, repo) = fixture();
        for path in ["../outside/secret.txt", "C:/Windows/win.ini", "/etc/passwd", "sub/ok.txt:stream", ""] {
            assert_eq!(read_worktree_file(&repo, path, 4096), None, "{path}");
        }
    }

    #[cfg(windows)]
    #[test]
    fn read_worktree_file_refuses_junction_and_symlink_escapes() {
        let (mut fixture, repo) = fixture();
        let repo_path = PathBuf::from(&repo);
        let outside = fixture.root.join("outside");
        let junction = repo_path.join("jn");
        let status = std::process::Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(&junction)
            .arg(&outside)
            .stdout(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "mklink /J");
        fixture.links.push(junction);
        assert_eq!(read_worktree_file(&repo, "jn/secret.txt", 4096), None);

        let dir_link = repo_path.join("lnd");
        if std::os::windows::fs::symlink_dir(&outside, &dir_link).is_ok() {
            fixture.links.push(dir_link);
            assert_eq!(read_worktree_file(&repo, "lnd/secret.txt", 4096), None);
        }
        let file_link = repo_path.join("lnf.txt");
        if std::os::windows::fs::symlink_file(outside.join("secret.txt"), &file_link).is_ok() {
            fixture.links.push(file_link);
            assert_eq!(read_worktree_file(&repo, "lnf.txt", 4096), None);
        }
        let inner_link = repo_path.join("lni.txt");
        if std::os::windows::fs::symlink_file(repo_path.join("sub").join("ok.txt"), &inner_link).is_ok() {
            fixture.links.push(inner_link);
            assert_eq!(read_worktree_file(&repo, "lni.txt", 4096), Some(Some(b"inside".to_vec())));
        }
    }

    #[cfg(unix)]
    #[test]
    fn read_worktree_file_refuses_symlink_escapes() {
        let (mut fixture, repo) = fixture();
        let repo_path = PathBuf::from(&repo);
        let outside = fixture.root.join("outside");
        let dir_link = repo_path.join("lnd");
        std::os::unix::fs::symlink(&outside, &dir_link).unwrap();
        fixture.links.push(dir_link);
        assert_eq!(read_worktree_file(&repo, "lnd/secret.txt", 4096), None);
        let file_link = repo_path.join("lnf.txt");
        std::os::unix::fs::symlink(outside.join("secret.txt"), &file_link).unwrap();
        fixture.links.push(file_link);
        assert_eq!(read_worktree_file(&repo, "lnf.txt", 4096), None);
    }

    fn git(dir: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .args(["-c", "user.email=t@t", "-c", "user.name=t", "-c", "core.autocrlf=false"])
            .args(args)
            .current_dir(dir)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    }

    #[test]
    fn same_name_does_not_fold_unicode_case() {
        assert!(!same_name(OsStr::new("kr"), OsStr::new("\u{212A}r")));
        assert!(!same_name(OsStr::new("straße"), OsStr::new("STRASSE")));
        #[cfg(windows)]
        assert!(same_name(OsStr::new("Repo"), OsStr::new("rEPO")));
    }

    #[test]
    fn read_worktree_file_refuses_a_separate_git_dir_inside_the_work_tree() {
        let dir = TempDiffDir::create().expect("temp dir");
        let repo = dir.0.join("top");
        std::fs::create_dir_all(repo.join("sub")).unwrap();
        std::fs::write(repo.join("sub").join("ok.txt"), "inside").unwrap();
        let meta = repo.join("meta");
        git(&repo, &["init", "-q", &format!("--separate-git-dir={}", meta.display())]);
        let cwd = repo.to_string_lossy().into_owned();
        for path in ["meta/config", "meta/HEAD", "META/config", "meta./config"] {
            let read = read_worktree_file(&cwd, path, 4096);
            assert!(!matches!(read, Some(Some(_))), "{path} returned content: {read:?}");
        }
        assert_eq!(read_worktree_file(&cwd, "sub/ok.txt", 4096), Some(Some(b"inside".to_vec())));
    }

    #[test]
    fn read_worktree_file_works_in_a_linked_worktree() {
        let dir = TempDiffDir::create().expect("temp dir");
        let main = dir.0.join("main");
        std::fs::create_dir_all(&main).unwrap();
        std::fs::write(main.join("a.txt"), "main").unwrap();
        git(&main, &["init", "-q"]);
        git(&main, &["add", "a.txt"]);
        git(&main, &["commit", "-q", "-m", "x"]);
        let linked = dir.0.join("linked");
        git(&main, &["worktree", "add", "-q", "-b", "side", &linked.to_string_lossy()]);
        std::fs::write(linked.join("a.txt"), "linked").unwrap();
        let cwd = linked.to_string_lossy().into_owned();
        assert_eq!(read_worktree_file(&cwd, "a.txt", 4096), Some(Some(b"linked".to_vec())));
        assert_eq!(read_worktree_file(&cwd, ".git", 4096), None);
        git(&main, &["worktree", "remove", "--force", &linked.to_string_lossy()]);
    }

    #[cfg(windows)]
    #[test]
    fn read_worktree_file_refuses_a_kelvin_sign_sibling_through_a_junction() {
        let (mut fixture, _repo) = fixture();
        let sibling = fixture.root.join("\u{212A}repo");
        let kelvin_repo = fixture.root.join("krepo");
        std::fs::create_dir_all(&sibling).unwrap();
        std::fs::write(sibling.join("secret.txt"), "outside sibling").unwrap();
        std::fs::create_dir_all(&kelvin_repo).unwrap();
        git(&kelvin_repo, &["init", "-q"]);
        let junction = kelvin_repo.join("jn");
        let status = std::process::Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(&junction)
            .arg(&sibling)
            .stdout(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "mklink /J");
        fixture.links.push(junction);
        let cwd = kelvin_repo.to_string_lossy().into_owned();
        assert_eq!(read_worktree_file(&cwd, "jn/secret.txt", 4096), None);
    }

    fn utf16_le(text: &str) -> Vec<u8> {
        let mut bytes = vec![0xFF, 0xFE];
        for unit in text.encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        bytes
    }

    #[test]
    fn decoded_text_diff_keeps_a_line_ending_only_change() {
        let dir = TempDiffDir::create().expect("temp dir");
        let repo = dir.0.join("eol");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q"]);
        std::fs::write(repo.join("x.inf"), utf16_le("one\r\ntwo\r\nthree\r\n")).unwrap();
        git(&repo, &["add", "x.inf"]);
        git(&repo, &["commit", "-q", "-m", "crlf"]);
        std::fs::write(repo.join("x.inf"), utf16_le("one\r\ntwo\nthree\r\n")).unwrap();
        git(&repo, &["commit", "-q", "-a", "-m", "lf"]);
        let cwd = repo.to_string_lossy().into_owned();
        let diff = decoded_text_diff(&cwd, Some("HEAD"), "x.inf", None, 4096).expect("decoded diff");
        assert!(diff.contains("\n-two\r\n"), "{diff:?}");
        assert!(diff.contains("\n+two\n"), "{diff:?}");
        assert!(diff.contains(" one\r\n"), "{diff:?}");
    }

    #[cfg(unix)]
    #[test]
    fn temp_diff_dir_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDiffDir::create().expect("temp dir");
        assert_eq!(std::fs::metadata(&dir.0).unwrap().permissions().mode() & 0o777, 0o700);
    }
}
