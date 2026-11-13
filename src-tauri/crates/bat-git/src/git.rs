use bat_host_support::subprocess::ProcessTree;
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

// Hard upper bound for log/status/diff output. Mirrors the Electron
// maxBuffer (5 MiB) so a runaway repo can't OOM the renderer.
const MAX_OUTPUT_BYTES: usize = 5 * 1024 * 1024;

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq, Clone)]
pub struct GitLogEntry {
    pub hash: String,
    pub author: String,
    pub date: String,
    pub message: String,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq, Clone)]
pub struct GitFileEntry {
    pub status: String,
    pub file: String,
    /// Source path of a rename or copy; `file` is the destination.
    #[serde(rename = "oldFile", default, skip_serializing_if = "Option::is_none")]
    pub old_file: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq, Clone)]
#[serde(rename_all = "camelCase")]
pub struct GitCommitDetails {
    pub hash: String,
    pub parents: Vec<String>,
    pub author_name: String,
    pub author_email: String,
    pub author_date: String,
    pub committer_name: String,
    pub committer_email: String,
    pub committer_date: String,
    pub message: String,
}

/// A ref decorating a graph log commit. `kind` is one of "head" (detached
/// HEAD), "branch", "remote", "tag" or "other"; `current` marks the branch
/// HEAD points at.
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq, Clone)]
pub struct GitGraphRef {
    pub kind: String,
    pub name: String,
    #[serde(default)]
    pub current: bool,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq, Clone)]
pub struct GitGraphLogEntry {
    pub hash: String,
    pub parents: Vec<String>,
    pub refs: Vec<GitGraphRef>,
    pub author: String,
    pub date: String,
    pub message: String,
}

// Run git in `cwd` and return stdout as UTF-8. Any failure (non-zero exit, missing binary,
// timeout, output over MAX_OUTPUT_BYTES) collapses to None so callers apply their own default.
pub fn run_git(cwd: &str, args: &[&str], timeout: Duration) -> Option<String> {
    let stdout = run_git_bytes(cwd, args, timeout, MAX_OUTPUT_BYTES)?;
    Some(String::from_utf8_lossy(&stdout).into_owned())
}

// Raw stdout of `git`, or None on any failure or when it exceeds `max_bytes`.
// stdout is drained on a reader thread so output beyond the pipe buffer cannot stall git.
pub fn run_git_bytes(
    cwd: &str,
    args: &[&str],
    timeout: Duration,
    max_bytes: usize,
) -> Option<Vec<u8>> {
    if cwd.trim().is_empty() || !Path::new(cwd).is_dir() {
        return None;
    }
    let mut command = Command::new("git");
    command
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut tree = ProcessTree::spawn(&mut command).ok()?;
    let Some(stdout) = tree.child().stdout.take() else {
        tree.kill();
        return None;
    };
    let reader = std::thread::spawn(move || {
        let mut buffer = Vec::new();
        stdout
            .take(max_bytes as u64 + 1)
            .read_to_end(&mut buffer)
            .map(|_| buffer)
    });

    let start = std::time::Instant::now();
    let status = loop {
        match tree.child().try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if start.elapsed() >= timeout => {
                tree.kill();
                // Do not join the reader: without a job object a descendant can
                // outlive the kill and keep the pipe open until it ends.
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(25)),
            Err(_) => {
                tree.kill();
                return None;
            }
        }
    };

    let stdout = reader.join().ok()?.ok()?;
    if !status.success() || stdout.len() > max_bytes {
        return None;
    }
    Some(stdout)
}

// Text for display, or None when over `max_bytes`, binary (contains NUL), or not UTF-8.
pub fn decode_text_blob(bytes: Vec<u8>, max_bytes: usize) -> Option<String> {
    if bytes.len() > max_bytes || bytes.contains(&0) {
        return None;
    }
    String::from_utf8(bytes).ok()
}

// `git remote get-url origin` returns the remote URL; we only
// translate github.com remotes so the GitHubPanel / Sidebar links
// resolve to a browsable HTTPS URL.
pub fn parse_github_url(remote: &str) -> Option<String> {
    let s = remote.trim();
    // git@github.com:owner/repo(.git)?
    if let Some(rest) = s.strip_prefix("git@github.com:") {
        let owner_repo = rest.strip_suffix(".git").unwrap_or(rest);
        if owner_repo.is_empty() {
            return None;
        }
        return Some(format!("https://github.com/{owner_repo}"));
    }
    // https?://github.com/owner/repo(.git)?
    for prefix in ["https://github.com/", "http://github.com/"] {
        if let Some(rest) = s.strip_prefix(prefix) {
            let owner_repo = rest.strip_suffix(".git").unwrap_or(rest);
            if owner_repo.is_empty() {
                return None;
            }
            return Some(format!("https://github.com/{owner_repo}"));
        }
    }
    None
}

// `git log --pretty=format:%H||%an||%aI||%s` — keep the delimiter
// in sync with the Electron handler so the parser is bit-for-bit
// the same. Use strict ISO-8601 dates so WebViews can parse them
// consistently across platforms.
pub fn parse_log(raw: &str) -> Vec<GitLogEntry> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    trimmed
        .lines()
        .map(|line| {
            let parts: Vec<&str> = line.splitn(4, "||").collect();
            GitLogEntry {
                hash: parts.first().copied().unwrap_or("").to_string(),
                author: parts.get(1).copied().unwrap_or("").to_string(),
                date: parts.get(2).copied().unwrap_or("").to_string(),
                message: parts.get(3).copied().unwrap_or("").to_string(),
            }
        })
        .collect()
}

// Parse `git diff --name-status -z [range]`: "<status>\0<path>\0", except renames and
// copies ("R<score>" / "C<score>"): "<status>\0<old>\0<new>\0". With -z, paths are
// verbatim (never C-quoted) and repository-root relative.
pub fn parse_diff_files(raw: &str) -> Vec<GitFileEntry> {
    let mut fields = raw.split('\0');
    let mut entries = Vec::new();
    while let Some(status) = fields.next() {
        let status = status.trim();
        if status.is_empty() {
            continue;
        }
        let Some(path) = fields.next() else { break };
        let entry = if status.starts_with(['R', 'C']) {
            let Some(new) = fields.next() else { break };
            GitFileEntry {
                status: status.to_string(),
                file: new.to_string(),
                old_file: Some(path.to_string()),
            }
        } else {
            GitFileEntry {
                status: status.to_string(),
                file: path.to_string(),
                old_file: None,
            }
        };
        entries.push(entry);
    }
    entries
}

// Parse `git status --porcelain -z --untracked-files=all`: "XY <path>\0", and for a
// rename or copy "XY <new>\0<old>\0". Paths are verbatim and repository-root relative.
// The status field is trimmed, like the Electron parser.
pub fn parse_status(raw: &str) -> Vec<GitFileEntry> {
    let mut fields = raw.split('\0');
    let mut entries = Vec::new();
    while let Some(record) = fields.next() {
        let (Some(xy), Some(path)) = (record.get(..2), record.get(3..)) else {
            continue;
        };
        let old_file = if xy.contains(['R', 'C']) {
            fields.next().map(str::to_string)
        } else {
            None
        };
        entries.push(GitFileEntry {
            status: xy.trim().to_string(),
            file: path.to_string(),
            old_file,
        });
    }
    entries
}

// A revision argument that git cannot mistake for an option.
fn is_safe_revision(rev: &str) -> bool {
    !rev.is_empty() && !rev.starts_with('-') && !rev.contains(['\0', '\n', '\r'])
}

fn is_safe_path(path: &str) -> bool {
    !path.is_empty() && !path.contains('\0')
}

// Paths from git output are root relative, but a plain pathspec resolves against the cwd
// (a workspace may be a repo subdirectory) and would glob `*`, `?` and `[`.
fn root_literal_pathspec(path: &str) -> String {
    format!(":(top,literal){path}")
}

// "working" means uncommitted changes vs HEAD. A commit's rename or copy is diffed
// blob to blob, so changes to the source path in the same commit are not mixed in.
pub fn build_diff_args<'a>(
    commit_hash: Option<&'a str>,
    file_path: Option<&'a str>,
    old_file_path: Option<&'a str>,
) -> Option<Vec<String>> {
    let file_path = file_path.filter(|p| !p.is_empty());
    let old_file_path = old_file_path.filter(|p| !p.is_empty() && Some(*p) != file_path);
    let commit = commit_hash.filter(|hash| !hash.is_empty() && *hash != "working");
    if commit.is_some_and(|hash| !is_safe_revision(hash)) {
        return None;
    }
    if let (Some(hash), Some(new), Some(old)) = (commit, file_path, old_file_path) {
        if !is_safe_path(new) || !is_safe_path(old) {
            return None;
        }
        return Some(vec![
            "diff".into(),
            format!("{hash}~1:{old}"),
            format!("{hash}:{new}"),
        ]);
    }
    let mut args: Vec<String> = vec!["diff".into()];
    if file_path.is_some() && old_file_path.is_some() {
        args.push("-M".into());
    }
    match commit {
        Some(hash) => args.push(format!("{hash}~1..{hash}")),
        None => args.push("HEAD".into()),
    }
    if let Some(p) = file_path {
        if !is_safe_path(p) || old_file_path.is_some_and(|old| !is_safe_path(old)) {
            return None;
        }
        args.push("--".into());
        if let Some(old) = old_file_path {
            args.push(root_literal_pathspec(old));
        }
        args.push(root_literal_pathspec(p));
    }
    Some(args)
}

// `cat-file blob` prints the stored content without checkout filters (EOL
// conversion, smudge), which is the text `git diff` hunks refer to.
pub fn build_show_file_args(rev: &str, file_path: &str) -> Option<Vec<String>> {
    if !is_safe_revision(rev) || !is_safe_path(file_path) {
        return None;
    }
    Some(vec![
        "cat-file".into(),
        "blob".into(),
        format!("{rev}:{file_path}"),
    ])
}

pub fn build_diff_files_args<'a>(commit_hash: Option<&'a str>) -> Option<Vec<String>> {
    match commit_hash {
        Some(hash) if !hash.is_empty() && hash != "working" => {
            if !is_safe_revision(hash) {
                return None;
            }
            Some(vec![
                "diff".into(),
                "--name-status".into(),
                "-z".into(),
                format!("{hash}~1..{hash}"),
            ])
        }
        _ => Some(vec![
            "diff".into(),
            "--name-status".into(),
            "-z".into(),
            "HEAD".into(),
        ]),
    }
}

// NUL-separated because names and the message may contain any other character; git rejects
// NUL in commit messages, and the message (%B) is last so it needs no escaping.
const COMMIT_DETAILS_FORMAT: &str = "%H%x00%P%x00%an%x00%ae%x00%aI%x00%cn%x00%ce%x00%cI%x00%B";
const COMMIT_DETAILS_FIELDS: usize = 9;
// Log output is decoded as UTF-8, so it must override i18n.logOutputEncoding (e.g. big5).
pub const LOG_OUTPUT_ENCODING: &str = "--encoding=UTF-8";

pub fn build_commit_details_args(commit_hash: &str) -> Option<Vec<String>> {
    if !is_safe_revision(commit_hash) || commit_hash == "working" {
        return None;
    }
    Some(vec![
        "log".into(),
        "-1".into(),
        "--no-show-signature".into(),
        "--no-color".into(),
        LOG_OUTPUT_ENCODING.into(),
        format!("--format={COMMIT_DETAILS_FORMAT}"),
        commit_hash.into(),
        "--".into(),
    ])
}

pub fn parse_commit_details(raw: &str) -> Option<GitCommitDetails> {
    let fields: Vec<&str> = raw.splitn(COMMIT_DETAILS_FIELDS, '\0').collect();
    let [
        hash,
        parents,
        author_name,
        author_email,
        author_date,
        committer_name,
        committer_email,
        committer_date,
        message,
    ] = fields.as_slice()
    else {
        return None;
    };
    if !is_object_id(hash) {
        return None;
    }
    let parents: Vec<String> = parents.split_whitespace().map(str::to_string).collect();
    if !parents.iter().all(|parent| is_object_id(parent)) {
        return None;
    }
    Some(GitCommitDetails {
        hash: hash.to_string(),
        parents,
        author_name: author_name.to_string(),
        author_email: author_email.to_string(),
        author_date: author_date.to_string(),
        committer_name: committer_name.to_string(),
        committer_email: committer_email.to_string(),
        committer_date: committer_date.to_string(),
        message: message.trim_end_matches(['\n', '\r']).to_string(),
    })
}

pub fn build_status_args() -> Vec<String> {
    vec![
        "status".into(),
        "--porcelain".into(),
        "-z".into(),
        "--untracked-files=all".into(),
    ]
}

pub fn clamp_log_count(count: Option<i64>) -> u32 {
    let raw = count.unwrap_or(50);
    raw.clamp(1, 500) as u32
}

const GRAPH_FIELD_SEPARATOR: char = '\x1f';

/// Argv for the graph log. `scope` "all" walks local branches, remote-tracking
/// branches, tags and HEAD; anything else walks HEAD only, like `git log`.
pub fn build_graph_log_args(count: Option<i64>, scope: Option<&str>) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "log".into(),
        "--date-order".into(),
        "--decorate=full".into(),
        "--no-show-signature".into(),
        "--pretty=format:%H%x1f%P%x1f%D%x1f%an%x1f%aI%x1f%s".into(),
        "-n".into(),
        clamp_log_count(count).to_string(),
        LOG_OUTPUT_ENCODING.into(),
        // Must precede the revisions; it lets an unborn HEAD log nothing instead of failing.
        "--ignore-missing".into(),
    ];
    if scope == Some("all") {
        for arg in ["--branches", "--remotes", "--tags"] {
            args.push(arg.into());
        }
    }
    args.push("HEAD".into());
    args.push("--".into());
    args
}

/// Parse `build_graph_log_args` output. Lines that are not commit records,
/// such as gpg output from `log.showSignature`, are skipped.
pub fn parse_graph_log(raw: &str) -> Vec<GitGraphLogEntry> {
    raw.lines().filter_map(parse_graph_log_line).collect()
}

fn parse_graph_log_line(line: &str) -> Option<GitGraphLogEntry> {
    let mut fields = line.splitn(6, GRAPH_FIELD_SEPARATOR);
    let hash = fields.next()?;
    let parents = fields.next()?;
    let decorations = fields.next()?;
    let author = fields.next()?;
    let date = fields.next()?;
    let message = fields.next()?;
    if !is_object_id(hash) {
        return None;
    }
    Some(GitGraphLogEntry {
        hash: hash.to_string(),
        parents: parents.split_whitespace().map(str::to_string).collect(),
        refs: parse_graph_decorations(decorations),
        author: author.to_string(),
        date: date.to_string(),
        message: message.to_string(),
    })
}

fn is_object_id(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

// `%D` with `--decorate=full`. Ref names cannot contain spaces, so ", " only
// ever separates entries.
fn parse_graph_decorations(raw: &str) -> Vec<GitGraphRef> {
    raw.split(", ")
        .filter_map(|entry| {
            let entry = entry.trim();
            if entry == "HEAD" {
                return Some(GitGraphRef {
                    kind: "head".into(),
                    name: "HEAD".into(),
                    current: false,
                });
            }
            if let Some(target) = entry.strip_prefix("HEAD -> ") {
                return parse_full_ref_name(target).map(|graph_ref| GitGraphRef {
                    current: true,
                    ..graph_ref
                });
            }
            parse_full_ref_name(entry.strip_prefix("tag: ").unwrap_or(entry))
        })
        .collect()
}

// Markers that are not refs, such as "grafted" or "replaced", yield None.
fn parse_full_ref_name(full_name: &str) -> Option<GitGraphRef> {
    let (kind, name) = if let Some(name) = full_name.strip_prefix("refs/heads/") {
        ("branch", name)
    } else if let Some(name) = full_name.strip_prefix("refs/remotes/") {
        ("remote", name)
    } else if let Some(name) = full_name.strip_prefix("refs/tags/") {
        ("tag", name)
    } else if let Some(name) = full_name.strip_prefix("refs/") {
        ("other", name)
    } else {
        return None;
    };
    if name.is_empty() {
        return None;
    }
    Some(GitGraphRef {
        kind: kind.into(),
        name: name.into(),
        current: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_github_ssh_url() {
        assert_eq!(
            parse_github_url("git@github.com:owner/repo.git"),
            Some("https://github.com/owner/repo".into()),
        );
        assert_eq!(
            parse_github_url("git@github.com:owner/repo"),
            Some("https://github.com/owner/repo".into()),
        );
    }

    #[test]
    fn parse_github_https_url() {
        assert_eq!(
            parse_github_url("https://github.com/owner/repo.git"),
            Some("https://github.com/owner/repo".into()),
        );
        assert_eq!(
            parse_github_url("http://github.com/owner/repo"),
            Some("https://github.com/owner/repo".into()),
        );
    }

    #[test]
    fn parse_github_url_rejects_non_github() {
        assert_eq!(parse_github_url("git@gitlab.com:owner/repo.git"), None);
        assert_eq!(parse_github_url("https://bitbucket.org/owner/repo"), None);
        assert_eq!(parse_github_url(""), None);
    }

    #[test]
    fn parse_log_handles_empty() {
        assert_eq!(parse_log(""), Vec::<GitLogEntry>::new());
        assert_eq!(parse_log("   \n  "), Vec::<GitLogEntry>::new());
    }

    #[test]
    fn parse_log_handles_messages_with_delimiter() {
        // The 4-way splitn means everything after the third "||" is
        // considered part of the message — preserving message
        // segments that contain "||".
        let raw = "abc123||Alice||2024-01-01 10:00||fix(pty): handle || edge case";
        let entries = parse_log(raw);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].hash, "abc123");
        assert_eq!(entries[0].author, "Alice");
        assert_eq!(entries[0].date, "2024-01-01 10:00");
        assert_eq!(entries[0].message, "fix(pty): handle || edge case");
    }

    #[test]
    fn parse_log_multiple_lines() {
        let raw = "h1||a||d1||m1\nh2||b||d2||m2";
        let entries = parse_log(raw);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[1].hash, "h2");
        assert_eq!(entries[1].message, "m2");
    }

    fn entry(status: &str, file: &str) -> GitFileEntry {
        GitFileEntry {
            status: status.into(),
            file: file.into(),
            old_file: None,
        }
    }

    fn renamed(status: &str, old: &str, new: &str) -> GitFileEntry {
        GitFileEntry {
            status: status.into(),
            file: new.into(),
            old_file: Some(old.into()),
        }
    }

    #[test]
    fn parse_diff_files_basic() {
        let raw = "M\0src/foo.rs\0A\0src/bar.rs\0D\0old/baz.rs\0";
        assert_eq!(
            parse_diff_files(raw),
            vec![
                entry("M", "src/foo.rs"),
                entry("A", "src/bar.rs"),
                entry("D", "old/baz.rs"),
            ]
        );
    }

    #[test]
    fn parse_diff_files_splits_renames_and_copies() {
        let raw = "R098\0old/a.cpp\0new/dir/a.cpp\0C075\0src/b.h\0src/c.h\0M\0keep.rs\0";
        assert_eq!(
            parse_diff_files(raw),
            vec![
                renamed("R098", "old/a.cpp", "new/dir/a.cpp"),
                renamed("C075", "src/b.h", "src/c.h"),
                entry("M", "keep.rs"),
            ]
        );
    }

    #[test]
    fn parse_diff_files_keeps_paths_verbatim() {
        let raw = "M\0中文.txt\0R100\0é.txt\0dir/sp ace\ttab\"q\".txt\0A\0sub/st*ar[1]?.txt\0";
        assert_eq!(
            parse_diff_files(raw),
            vec![
                entry("M", "中文.txt"),
                renamed("R100", "é.txt", "dir/sp ace\ttab\"q\".txt"),
                entry("A", "sub/st*ar[1]?.txt"),
            ]
        );
    }

    #[test]
    fn parse_diff_files_handles_empty_and_truncated_output() {
        assert_eq!(parse_diff_files(""), Vec::<GitFileEntry>::new());
        assert_eq!(parse_diff_files("M\0a.rs\0R090\0old.rs"), vec![entry("M", "a.rs")]);
    }

    #[test]
    fn git_file_entry_json_omits_missing_old_file() {
        let plain = serde_json::to_value(entry("M", "a.rs")).unwrap();
        assert_eq!(plain, serde_json::json!({ "status": "M", "file": "a.rs" }));
        assert_eq!(
            serde_json::to_value(renamed("R100", "a.rs", "b.rs")).unwrap(),
            serde_json::json!({ "status": "R100", "file": "b.rs", "oldFile": "a.rs" })
        );
        let from_old_host: GitFileEntry =
            serde_json::from_value(serde_json::json!({ "status": "M", "file": "a.rs" })).unwrap();
        assert_eq!(from_old_host, entry("M", "a.rs"));
    }

    #[test]
    fn parse_status_porcelain() {
        let raw = " M src/foo.rs\0?? new.txt\0MM both.rs\0";
        assert_eq!(
            parse_status(raw),
            vec![
                entry("M", "src/foo.rs"),
                entry("??", "new.txt"),
                entry("MM", "both.rs"),
            ]
        );
    }

    #[test]
    fn parse_status_reads_staged_renames_and_copies() {
        let raw = "R  中文.txt\0é.txt\0RM new.rs\0old.rs\0C  copy.rs\0src.rs\0 M sp ace.txt\0";
        assert_eq!(
            parse_status(raw),
            vec![
                renamed("R", "é.txt", "中文.txt"),
                renamed("RM", "old.rs", "new.rs"),
                renamed("C", "src.rs", "copy.rs"),
                entry("M", "sp ace.txt"),
            ]
        );
    }

    #[test]
    fn parse_status_keeps_paths_verbatim() {
        let raw = "?? generated/output.txt\0?? tab\tq\"uote\".txt\0A  新 file*.txt\0";
        assert_eq!(
            parse_status(raw),
            vec![
                entry("??", "generated/output.txt"),
                entry("??", "tab\tq\"uote\".txt"),
                entry("A", "新 file*.txt"),
            ]
        );
    }

    #[test]
    fn parse_status_empty_returns_empty() {
        assert_eq!(parse_status(""), Vec::<GitFileEntry>::new());
        assert_eq!(parse_status("\0"), Vec::<GitFileEntry>::new());
    }

    #[test]
    fn build_status_args_use_nul_separated_porcelain() {
        assert_eq!(
            Some(build_status_args()),
            argv(&["status", "--porcelain", "-z", "--untracked-files=all"]),
        );
    }

    fn argv(items: &[&str]) -> Option<Vec<String>> {
        Some(items.iter().map(|item| item.to_string()).collect())
    }

    #[test]
    fn build_diff_args_default() {
        assert_eq!(build_diff_args(None, None, None), argv(&["diff", "HEAD"]));
    }

    #[test]
    fn build_diff_args_working_keyword() {
        // "working" is the renderer's sentinel for HEAD/uncommitted.
        assert_eq!(
            build_diff_args(Some("working"), None, None),
            argv(&["diff", "HEAD"]),
        );
        assert_eq!(
            build_diff_args(Some("working"), Some("src/a.rs"), None),
            argv(&["diff", "HEAD", "--", ":(top,literal)src/a.rs"]),
        );
    }

    #[test]
    fn build_diff_args_with_commit_and_path() {
        assert_eq!(
            build_diff_args(Some("abc123"), Some("src/foo.rs"), None),
            argv(&["diff", "abc123~1..abc123", "--", ":(top,literal)src/foo.rs"]),
        );
    }

    #[test]
    fn build_diff_args_pathspec_is_root_relative_and_literal() {
        assert_eq!(
            build_diff_args(Some("abc"), Some("中文 dir/st*ar[1]?.txt"), None),
            argv(&["diff", "abc~1..abc", "--", ":(top,literal)中文 dir/st*ar[1]?.txt"]),
        );
    }

    #[test]
    fn build_diff_args_empty_commit_falls_back_to_head() {
        assert_eq!(
            build_diff_args(Some(""), None, None),
            argv(&["diff", "HEAD"]),
        );
    }

    #[test]
    fn build_diff_args_pairs_a_rename() {
        assert_eq!(
            build_diff_args(Some("abc"), Some("new/a.cpp"), Some("old/a.cpp")),
            argv(&["diff", "abc~1:old/a.cpp", "abc:new/a.cpp"]),
        );
        assert_eq!(build_diff_args(Some("abc"), Some("b.rs"), Some("a\0b")), None);
        assert_eq!(build_diff_args(Some("-p"), Some("b.rs"), Some("a.rs")), None);
        assert_eq!(
            build_diff_args(Some("working"), Some("b.rs"), Some("a.rs")),
            argv(&[
                "diff",
                "-M",
                "HEAD",
                "--",
                ":(top,literal)a.rs",
                ":(top,literal)b.rs",
            ]),
        );
    }

    #[test]
    fn build_diff_args_ignores_unusable_old_path() {
        let expected = argv(&["diff", "abc~1..abc", "--", ":(top,literal)a.rs"]);
        assert_eq!(build_diff_args(Some("abc"), Some("a.rs"), Some("")), expected);
        assert_eq!(build_diff_args(Some("abc"), Some("a.rs"), Some("a.rs")), expected);
        assert_eq!(
            build_diff_args(Some("abc"), None, Some("a.rs")),
            argv(&["diff", "abc~1..abc"]),
        );
    }

    #[test]
    fn build_diff_args_rejects_option_like_revisions() {
        assert_eq!(build_diff_args(Some("--output=x"), Some("a.rs"), None), None);
        assert_eq!(build_diff_args(Some("-p"), None, None), None);
        assert_eq!(build_diff_args(Some("abc\n--output=x"), None, None), None);
    }

    #[test]
    fn build_diff_files_args_variants() {
        assert_eq!(
            build_diff_files_args(None),
            argv(&["diff", "--name-status", "-z", "HEAD"]),
        );
        assert_eq!(
            build_diff_files_args(Some("abc")),
            argv(&["diff", "--name-status", "-z", "abc~1..abc"]),
        );
        assert_eq!(
            build_diff_files_args(Some("working")),
            argv(&["diff", "--name-status", "-z", "HEAD"]),
        );
        assert_eq!(build_diff_files_args(Some("--output=x")), None);
    }

    #[test]
    fn build_show_file_args_reads_the_blob_at_a_revision() {
        assert_eq!(
            build_show_file_args("abc123", "src/foo.rs"),
            argv(&["cat-file", "blob", "abc123:src/foo.rs"]),
        );
        assert_eq!(
            build_show_file_args("abc123~1", "中文 dir/a\tb*.txt"),
            argv(&["cat-file", "blob", "abc123~1:中文 dir/a\tb*.txt"]),
        );
    }

    #[test]
    fn decode_text_blob_enforces_the_size_cap() {
        assert_eq!(decode_text_blob(b"abcd".to_vec(), 4), Some("abcd".into()));
        assert_eq!(decode_text_blob(b"abcde".to_vec(), 4), None);
        assert_eq!(decode_text_blob(Vec::new(), 4), Some(String::new()));
    }

    #[test]
    fn decode_text_blob_rejects_binary_and_invalid_utf8() {
        assert_eq!(decode_text_blob(b"a\0b".to_vec(), 64), None);
        assert_eq!(decode_text_blob(vec![0xA4, 0xA4, 0xA4, 0xE5], 64), None);
        assert_eq!(
            decode_text_blob("中文 line\r\n".as_bytes().to_vec(), 64),
            Some("中文 line\r\n".into())
        );
    }

    #[test]
    fn run_git_bytes_returns_none_over_the_cap() {
        let cwd = env!("CARGO_MANIFEST_DIR");
        let full = run_git_bytes(cwd, &["--version"], Duration::from_secs(30), 1024)
            .expect("git --version");
        assert!(run_git_bytes(cwd, &["--version"], Duration::from_secs(30), full.len() - 1).is_none());
    }

    #[test]
    fn build_show_file_args_rejects_unsafe_input() {
        assert_eq!(build_show_file_args("", "a.rs"), None);
        assert_eq!(build_show_file_args("-p", "a.rs"), None);
        assert_eq!(build_show_file_args("--batch", "a.rs"), None);
        assert_eq!(build_show_file_args("abc", ""), None);
        assert_eq!(build_show_file_args("abc", "a\0b"), None);
        assert_eq!(build_show_file_args("abc\r", "a.rs"), None);
    }

    const DETAILS_HASH_A: &str = "0123456789abcdef0123456789abcdef01234567";
    const DETAILS_HASH_B: &str = "89abcdef0123456789abcdef0123456789abcdef";
    const DETAILS_HASH_C: &str = "fedcba9876543210fedcba9876543210fedcba98";

    fn details_raw(parents: &str, message: &str) -> String {
        [
            DETAILS_HASH_A,
            parents,
            "Ada Lovelace",
            "ada@example.com",
            "2026-11-13T15:28:21+08:00",
            "Grace Hopper",
            "grace@example.com",
            "2026-11-13T16:00:00+08:00",
            message,
        ]
        .join("\0")
    }

    #[test]
    fn build_commit_details_args_validates_the_revision() {
        assert_eq!(
            build_commit_details_args(DETAILS_HASH_A),
            argv(&[
                "log",
                "-1",
                "--no-show-signature",
                "--no-color",
                "--encoding=UTF-8",
                "--format=%H%x00%P%x00%an%x00%ae%x00%aI%x00%cn%x00%ce%x00%cI%x00%B",
                DETAILS_HASH_A,
                "--",
            ]),
        );
        assert_eq!(build_commit_details_args(""), None);
        assert_eq!(build_commit_details_args("working"), None);
        assert_eq!(build_commit_details_args("--output=x"), None);
        assert_eq!(build_commit_details_args("abc\n-p"), None);
    }

    #[test]
    fn parse_commit_details_keeps_a_multi_line_body() {
        let message = "feat: subject line\n\nBody line one\n  indented: two\n\nTrailer: x\n\n";
        let details = parse_commit_details(&details_raw(DETAILS_HASH_B, message)).unwrap();
        assert_eq!(details.hash, DETAILS_HASH_A);
        assert_eq!(details.parents, vec![DETAILS_HASH_B.to_string()]);
        assert_eq!(details.author_name, "Ada Lovelace");
        assert_eq!(details.author_email, "ada@example.com");
        assert_eq!(details.author_date, "2026-11-13T15:28:21+08:00");
        assert_eq!(details.committer_name, "Grace Hopper");
        assert_eq!(details.committer_email, "grace@example.com");
        assert_eq!(details.committer_date, "2026-11-13T16:00:00+08:00");
        assert_eq!(
            details.message,
            "feat: subject line\n\nBody line one\n  indented: two\n\nTrailer: x"
        );
    }

    #[test]
    fn parse_commit_details_handles_an_empty_body() {
        let details = parse_commit_details(&details_raw(DETAILS_HASH_B, "fix: only a subject\n\n")).unwrap();
        assert_eq!(details.message, "fix: only a subject");
    }

    #[test]
    fn parse_commit_details_lists_both_parents_of_a_merge() {
        let details =
            parse_commit_details(&details_raw(&format!("{DETAILS_HASH_B} {DETAILS_HASH_C}"), "Merge x\n")).unwrap();
        assert_eq!(details.parents, vec![DETAILS_HASH_B.to_string(), DETAILS_HASH_C.to_string()]);
    }

    #[test]
    fn parse_commit_details_root_commit_has_no_parents() {
        let details = parse_commit_details(&details_raw("", "initial\n")).unwrap();
        assert!(details.parents.is_empty());
    }

    #[test]
    fn parse_commit_details_keeps_non_ascii_text() {
        let raw = [
            DETAILS_HASH_A, "", "王小明", "ming@例子.tw", "2026-01-02T03:04:05+08:00", "Zoë", "z@x",
            "2026-01-02T03:04:05+08:00", "修正：中文訊息\n\n內文 NUL-free\tTab\n",
        ]
        .join("\0");
        let details = parse_commit_details(&raw).unwrap();
        assert_eq!(details.author_name, "王小明");
        assert_eq!(details.author_email, "ming@例子.tw");
        assert_eq!(details.committer_name, "Zoë");
        assert_eq!(details.message, "修正：中文訊息\n\n內文 NUL-free\tTab");
    }

    #[test]
    fn parse_commit_details_rejects_malformed_output() {
        assert_eq!(parse_commit_details(""), None);
        assert_eq!(parse_commit_details("not-a-hash\0\0a\0b\0c\0d\0e\0f\0m"), None);
        assert_eq!(parse_commit_details(&format!("{DETAILS_HASH_A}\0bogus\0a\0b\0c\0d\0e\0f\0m")), None);
        assert_eq!(parse_commit_details(&format!("{DETAILS_HASH_A}\0\0a\0b")), None);
    }

    #[test]
    fn git_commit_details_json_is_camel_case() {
        let details = parse_commit_details(&details_raw("", "m")).unwrap();
        let value = serde_json::to_value(&details).unwrap();
        assert_eq!(value["authorName"], "Ada Lovelace");
        assert_eq!(value["committerDate"], "2026-11-13T16:00:00+08:00");
        assert_eq!(value["parents"], serde_json::json!([]));
    }

    #[test]
    fn clamp_log_count_bounds() {
        assert_eq!(clamp_log_count(None), 50);
        assert_eq!(clamp_log_count(Some(0)), 1);
        assert_eq!(clamp_log_count(Some(-5)), 1);
        assert_eq!(clamp_log_count(Some(10)), 10);
        assert_eq!(clamp_log_count(Some(99999)), 500);
    }

    #[test]
    fn run_git_rejects_invalid_cwd() {
        assert!(run_git("", &["status"], Duration::from_secs(1)).is_none());
        assert!(run_git(
            "C:/this/path/should/never/exist/abc123",
            &["status"],
            Duration::from_secs(1),
        )
        .is_none());
    }

    const HASH_A: &str = "1111111111111111111111111111111111111111";
    const HASH_B: &str = "2222222222222222222222222222222222222222";
    const HASH_C: &str = "3333333333333333333333333333333333333333";

    fn graph_line(hash: &str, parents: &str, refs: &str, message: &str) -> String {
        format!("{hash}\x1f{parents}\x1f{refs}\x1fAlice\x1f2026-11-10T10:00:00+08:00\x1f{message}")
    }

    fn graph_ref(kind: &str, name: &str, current: bool) -> GitGraphRef {
        GitGraphRef {
            kind: kind.into(),
            name: name.into(),
            current,
        }
    }

    #[test]
    fn build_graph_log_args_for_current_scope() {
        let args = build_graph_log_args(None, None);
        assert_eq!(
            args,
            vec![
                "log",
                "--date-order",
                "--decorate=full",
                "--no-show-signature",
                "--pretty=format:%H%x1f%P%x1f%D%x1f%an%x1f%aI%x1f%s",
                "-n",
                "50",
                "--encoding=UTF-8",
                "--ignore-missing",
                "HEAD",
                "--",
            ]
        );
        assert_eq!(build_graph_log_args(Some(10), Some("current"))[6], "10");
        assert_eq!(
            build_graph_log_args(Some(10), Some("bogus")),
            build_graph_log_args(Some(10), None)
        );
        assert_eq!(build_graph_log_args(Some(99_999), None)[6], "500");
    }

    #[test]
    fn build_graph_log_args_for_all_scope_orders_ignore_missing_first() {
        let args = build_graph_log_args(Some(100), Some("all"));
        let position = |needle: &str| args.iter().position(|arg| arg == needle).unwrap();
        assert_eq!(args[6], "100");
        for revision in ["--branches", "--remotes", "--tags", "HEAD"] {
            assert!(position("--ignore-missing") < position(revision), "{revision}");
        }
        assert_eq!(args.last().map(String::as_str), Some("--"));
        assert!(!args.iter().any(|arg| arg == "--all"));
    }

    #[test]
    fn parse_graph_log_reads_parents_and_message() {
        let raw = [
            graph_line(HASH_A, &format!("{HASH_B} {HASH_C}"), "", "Merge branch 'x'"),
            graph_line(HASH_B, HASH_C, "", "feat: a \x1f b || c"),
            graph_line(HASH_C, "", "", "root"),
        ]
        .join("\n");
        let entries = parse_graph_log(&raw);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].parents, vec![HASH_B.to_string(), HASH_C.to_string()]);
        assert_eq!(entries[0].author, "Alice");
        assert_eq!(entries[0].date, "2026-11-10T10:00:00+08:00");
        assert_eq!(entries[1].parents, vec![HASH_C.to_string()]);
        assert_eq!(entries[1].message, "feat: a \x1f b || c");
        assert!(entries[2].parents.is_empty());
        assert_eq!(parse_graph_log(""), Vec::<GitGraphLogEntry>::new());
    }

    #[test]
    fn parse_graph_log_classifies_refs() {
        let refs = "HEAD -> refs/heads/feat/git-graph, refs/remotes/origin/HEAD, \
                    refs/remotes/origin/main, tag: refs/tags/v3.2.18, refs/heads/main, \
                    refs/stash, grafted, replaced";
        let entries = parse_graph_log(&graph_line(HASH_A, "", refs, "m"));
        assert_eq!(
            entries[0].refs,
            vec![
                graph_ref("branch", "feat/git-graph", true),
                graph_ref("remote", "origin/HEAD", false),
                graph_ref("remote", "origin/main", false),
                graph_ref("tag", "v3.2.18", false),
                graph_ref("branch", "main", false),
                graph_ref("other", "stash", false),
            ]
        );
        let detached = parse_graph_log(&graph_line(HASH_B, "", "HEAD, refs/heads/main", "m"));
        assert_eq!(
            detached[0].refs,
            vec![graph_ref("head", "HEAD", false), graph_ref("branch", "main", false)]
        );
    }

    #[test]
    fn parse_graph_log_skips_lines_that_are_not_records() {
        let raw = [
            "gpg: Signature made Mon Nov 10 10:00:00 2026".to_string(),
            graph_line(HASH_A, "", "", "kept"),
            graph_line("not-a-hash", "", "", "dropped"),
            format!("{HASH_B}\x1f{HASH_C}\x1fmissing fields"),
        ]
        .join("\n");
        let entries = parse_graph_log(&raw);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].message, "kept");
    }

    #[test]
    fn parse_graph_log_accepts_sha256_ids_and_crlf() {
        let sha256 = "a".repeat(64);
        let raw = format!(
            "{}\r\n{}",
            graph_line(&sha256, "", "", "x"),
            graph_line(HASH_A, &sha256, "", "y")
        );
        let entries = parse_graph_log(&raw);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].message, "x");
        assert_eq!(entries[1].parents, vec![sha256]);
    }

    #[test]
    fn run_git_reads_output_larger_than_the_pipe_buffer() {
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
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or(0);
        let repo = std::env::temp_dir().join(format!(
            "bat-git-large-output-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&repo).expect("create fixture repo");
        let repo_dir = repo.to_str().expect("utf-8 temp path");
        let message_path = repo.join("message.txt");
        let big_file = repo.join("big.txt");
        let original: String = (0..8_000).map(|n| format!("line {n} original\n")).collect();
        let changed: String = (0..8_000).map(|n| format!("line {n} changed\n")).collect();
        std::fs::write(&message_path, "large output line\n".repeat(8_000)).expect("write message");
        std::fs::write(&big_file, &original).expect("write file");
        let hooks_path = format!("core.hooksPath={}", repo.join("no-hooks").display());
        let message_file = message_path.to_str().expect("utf-8 message path");
        let setup_timeout = Duration::from_secs(60);
        // A stalled read only ends at this timeout, so it must stay well below
        // the setup timeout yet far above a normal read.
        let read_timeout = Duration::from_secs(20);

        let init = run_git(repo_dir, &["init", "-q"], setup_timeout);
        let add = run_git(repo_dir, &["add", "big.txt"], setup_timeout);
        let mut commit_args = vec![
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "-c",
            "commit.gpgsign=false",
            "-c",
            hooks_path.as_str(),
        ];
        commit_args.extend(["commit", "-q", "-F", message_file]);
        let commit = run_git(repo_dir, &commit_args, setup_timeout);
        std::fs::write(&big_file, &changed).expect("rewrite file");
        let log = run_git(repo_dir, &["log", "-n", "1", "--pretty=format:%B"], read_timeout);
        let diff = run_git(repo_dir, &["diff", "HEAD", "--", "big.txt"], read_timeout);
        let _ = std::fs::remove_dir_all(&repo);

        assert!(init.is_some(), "git init failed");
        assert!(add.is_some(), "git add failed");
        assert!(commit.is_some(), "git commit failed");
        let log = log.expect("a large log is read in full");
        assert!(log.len() > 100_000, "log captured {} bytes", log.len());
        assert!(log.starts_with("large output line"));
        let diff = diff.expect("a large diff is read in full");
        assert!(diff.len() > 100_000, "diff captured {} bytes", diff.len());
        assert!(diff.contains("+line 7999 changed"));
    }
}
