use bat_host_support::subprocess::hide_console_window;
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
// stdout is drained on a reader thread so output beyond the pipe buffer cannot stall git.
pub fn run_git(cwd: &str, args: &[&str], timeout: Duration) -> Option<String> {
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
    hide_console_window(&mut command);
    let mut child = command.spawn().ok()?;
    let Some(stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return None;
    };
    let reader = std::thread::spawn(move || {
        let mut buffer = Vec::new();
        stdout
            .take(MAX_OUTPUT_BYTES as u64 + 1)
            .read_to_end(&mut buffer)
            .map(|_| buffer)
    });

    let start = std::time::Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if start.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                // Do not join the reader: on Windows `git` can be a wrapper whose
                // own child survives the kill and keeps the pipe open until it ends.
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(25)),
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };

    let stdout = reader.join().ok()?.ok()?;
    if !status.success() || stdout.len() > MAX_OUTPUT_BYTES {
        return None;
    }
    Some(String::from_utf8_lossy(&stdout).into_owned())
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

// Parse `git diff --name-status [range]`: each line is
// "<status>\t<file>" (renames use a longer two-tab form, but the
// Electron handler also flattens that to the first segment).
pub fn parse_diff_files(raw: &str) -> Vec<GitFileEntry> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    trimmed
        .lines()
        .map(|line| {
            if let Some(tab_idx) = line.find('\t') {
                GitFileEntry {
                    status: line[..tab_idx].trim().to_string(),
                    file: line[tab_idx + 1..].to_string(),
                }
            } else {
                let status = line
                    .chars()
                    .next()
                    .map(|c| c.to_string())
                    .unwrap_or_default();
                let file = if line.len() > 2 {
                    line[2..].to_string()
                } else {
                    String::new()
                };
                GitFileEntry { status, file }
            }
        })
        .collect()
}

// Parse `git status --porcelain --untracked-files=all`: each line begins with two
// status chars followed by a space and the path. Mirrors the
// Electron parser (which trims the status field).
pub fn parse_status(raw: &str) -> Vec<GitFileEntry> {
    if raw.trim().is_empty() {
        return Vec::new();
    }
    raw.split('\n')
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let status_end = line.len().min(2);
            let status = line[..status_end].trim().to_string();
            let file = if line.len() > 3 {
                line[3..].to_string()
            } else {
                String::new()
            };
            GitFileEntry { status, file }
        })
        .collect()
}

// Build the argv for `git diff` based on optional commit hash + path.
// "working" is a magic string the renderer uses to mean "uncommitted
// changes vs HEAD" — we map that to `git diff HEAD`.
pub fn build_diff_args<'a>(
    commit_hash: Option<&'a str>,
    file_path: Option<&'a str>,
) -> Vec<String> {
    let mut args: Vec<String> = vec!["diff".into()];
    match commit_hash {
        Some(hash) if !hash.is_empty() && hash != "working" => {
            args.push(format!("{hash}~1..{hash}"));
        }
        _ => args.push("HEAD".into()),
    }
    if let Some(p) = file_path {
        if !p.is_empty() {
            args.push("--".into());
            args.push(p.to_string());
        }
    }
    args
}

pub fn build_diff_files_args<'a>(commit_hash: Option<&'a str>) -> Vec<String> {
    match commit_hash {
        Some(hash) if !hash.is_empty() && hash != "working" => {
            vec![
                "diff".into(),
                "--name-status".into(),
                format!("{hash}~1..{hash}"),
            ]
        }
        _ => vec!["diff".into(), "--name-status".into(), "HEAD".into()],
    }
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

    #[test]
    fn parse_diff_files_basic() {
        let raw = "M\tsrc/foo.rs\nA\tsrc/bar.rs\nD\told/baz.rs";
        let entries = parse_diff_files(raw);
        assert_eq!(entries.len(), 3);
        assert_eq!(
            entries[0],
            GitFileEntry {
                status: "M".into(),
                file: "src/foo.rs".into()
            }
        );
        assert_eq!(
            entries[2],
            GitFileEntry {
                status: "D".into(),
                file: "old/baz.rs".into()
            }
        );
    }

    #[test]
    fn parse_diff_files_no_tab_fallback() {
        // Defensive: if the line lacks a tab, take first char as
        // status and skip a couple chars for the file. Mirrors the
        // Electron substring(2) fallback.
        let raw = "M  some/odd/file.rs";
        let entries = parse_diff_files(raw);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].status, "M");
    }

    #[test]
    fn parse_status_porcelain() {
        let raw = " M src/foo.rs\n?? new.txt\nMM both.rs\n";
        let entries = parse_status(raw);
        assert_eq!(entries.len(), 3);
        assert_eq!(
            entries[0],
            GitFileEntry {
                status: "M".into(),
                file: "src/foo.rs".into()
            }
        );
        assert_eq!(
            entries[1],
            GitFileEntry {
                status: "??".into(),
                file: "new.txt".into()
            }
        );
        assert_eq!(
            entries[2],
            GitFileEntry {
                status: "MM".into(),
                file: "both.rs".into()
            }
        );
    }

    #[test]
    fn parse_status_keeps_untracked_folder_files() {
        let raw = "?? generated/output.txt\n?? generated/nested/file.log\n";
        let entries = parse_status(raw);
        assert_eq!(
            entries,
            vec![
                GitFileEntry {
                    status: "??".into(),
                    file: "generated/output.txt".into(),
                },
                GitFileEntry {
                    status: "??".into(),
                    file: "generated/nested/file.log".into(),
                },
            ]
        );
    }

    #[test]
    fn parse_status_empty_returns_empty() {
        assert_eq!(parse_status(""), Vec::<GitFileEntry>::new());
        assert_eq!(parse_status("\n\n"), Vec::<GitFileEntry>::new());
    }

    #[test]
    fn build_diff_args_default() {
        assert_eq!(
            build_diff_args(None, None),
            vec!["diff".to_string(), "HEAD".into()]
        );
    }

    #[test]
    fn build_diff_args_working_keyword() {
        // "working" is the renderer's sentinel for HEAD/uncommitted.
        assert_eq!(
            build_diff_args(Some("working"), None),
            vec!["diff".to_string(), "HEAD".into()],
        );
    }

    #[test]
    fn build_diff_args_with_commit_and_path() {
        assert_eq!(
            build_diff_args(Some("abc123"), Some("src/foo.rs")),
            vec![
                "diff".to_string(),
                "abc123~1..abc123".into(),
                "--".into(),
                "src/foo.rs".into(),
            ],
        );
    }

    #[test]
    fn build_diff_args_empty_commit_falls_back_to_head() {
        assert_eq!(
            build_diff_args(Some(""), None),
            vec!["diff".to_string(), "HEAD".into()],
        );
    }

    #[test]
    fn build_diff_files_args_variants() {
        assert_eq!(
            build_diff_files_args(None),
            vec!["diff".to_string(), "--name-status".into(), "HEAD".into()],
        );
        assert_eq!(
            build_diff_files_args(Some("abc")),
            vec![
                "diff".to_string(),
                "--name-status".into(),
                "abc~1..abc".into()
            ],
        );
        assert_eq!(
            build_diff_files_args(Some("working")),
            vec!["diff".to_string(), "--name-status".into(), "HEAD".into()],
        );
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
        let original: String = (0..8_000).map(|n| format!("line {n} original
")).collect();
        let changed: String = (0..8_000).map(|n| format!("line {n} changed
")).collect();
        std::fs::write(&message_path, "large output line
".repeat(8_000)).expect("write message");
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
