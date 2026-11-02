#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathStyle {
    Windows,
    Posix { case_insensitive: bool },
}

impl PathStyle {
    pub fn host() -> Self {
        if cfg!(windows) {
            PathStyle::Windows
        } else {
            PathStyle::Posix {
                case_insensitive: cfg!(target_os = "macos"),
            }
        }
    }

    pub fn case_insensitive(self) -> bool {
        match self {
            PathStyle::Windows => true,
            PathStyle::Posix { case_insensitive } => case_insensitive,
        }
    }
}

pub fn normalize_path(path: &str, style: PathStyle) -> String {
    normalize_host_path(path, style.case_insensitive())
}

/// Must stay byte-identical to `normalizeHostPath` in renderer/src/utils/session-history-actions.ts: the rule
/// is picked by the path's shape (drive letter or leading `\\`), not by the host.
pub fn normalize_host_path(path: &str, case_insensitive: bool) -> String {
    if is_windows_shaped(path) {
        let mut out = String::with_capacity(path.len() + 1);
        if path.starts_with("\\\\") {
            out.push('\\');
        }
        out.push_str(&collapse_separators(&path.replace('/', "\\"), '\\'));
        let trimmed_len = out.trim_end_matches('\\').len();
        out.truncate(trimmed_len);
        if is_bare_drive(&out) {
            out.push('\\');
        }
        return out.to_lowercase();
    }
    let mut out = collapse_separators(path, '/');
    if out.len() > 1 {
        let trimmed_len = out.trim_end_matches('/').len();
        out.truncate(trimmed_len);
        if out.is_empty() {
            out.push('/');
        }
    }
    if case_insensitive {
        out.to_lowercase()
    } else {
        out
    }
}

fn is_windows_shaped(path: &str) -> bool {
    let bytes = path.as_bytes();
    let drive = bytes.len() >= 2
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes.get(2), None | Some(b'\\' | b'/'));
    drive || path.starts_with("\\\\")
}

fn is_bare_drive(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() == 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

fn collapse_separators(path: &str, separator: char) -> String {
    let mut out = String::with_capacity(path.len());
    for c in path.chars() {
        if !(c == separator && out.ends_with(separator)) {
            out.push(c);
        }
    }
    out
}

/// The last path component, accepting either separator; a bare root keeps its full text.
pub(crate) fn display_name(dir: &str) -> String {
    let trimmed = dir.trim_end_matches(['\\', '/']);
    let name = trimmed.rsplit(['\\', '/']).next().unwrap_or("");
    if trimmed.is_empty() {
        dir.to_string()
    } else if name.is_empty() {
        trimmed.to_string()
    } else {
        name.to_string()
    }
}

/// Git prints `/` on every platform; the workspace path should use the host's own separator.
pub(crate) fn to_host_separators(path: &str, style: PathStyle) -> String {
    match style {
        PathStyle::Windows => path.replace('/', "\\"),
        PathStyle::Posix { .. } => path.to_string(),
    }
}

/// Some Windows APIs return `\\?\C:\…` or `\\?\UNC\server\…`; workspace paths never carry that prefix.
pub(crate) fn strip_verbatim_prefix(path: &str) -> String {
    if let Some(rest) = path
        .strip_prefix("\\\\?\\UNC\\")
        .or_else(|| path.strip_prefix("//?/UNC/"))
    {
        format!("\\\\{rest}")
    } else if let Some(rest) = path
        .strip_prefix("\\\\?\\")
        .or_else(|| path.strip_prefix("//?/"))
    {
        rest.to_string()
    } else {
        path.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const POSIX: PathStyle = PathStyle::Posix {
        case_insensitive: false,
    };
    const MAC: PathStyle = PathStyle::Posix {
        case_insensitive: true,
    };

    #[test]
    fn windows_rules_match_renderer() {
        let w = |p| normalize_host_path(p, true);
        assert_eq!(w("C:\\Users\\Me\\Repo\\"), "c:\\users\\me\\repo");
        assert_eq!(
            normalize_host_path("C:/Users//Me/Repo/", false),
            "c:\\users\\me\\repo"
        );
        assert_eq!(w("C:\\"), "c:\\");
        assert_eq!(w("C:/"), "c:\\");
        assert_eq!(w("C:\\\\"), "c:\\");
        assert_eq!(w("D:"), "d:\\");
        assert_eq!(w("\\\\Server\\Share\\\\Dir\\"), "\\\\server\\share\\dir");
        assert_eq!(w("\\\\Server/Share/"), "\\\\server\\share");
        assert_eq!(w("D:\\codebase\\Repo"), w("d:/CODEBASE/repo/"));
        assert_eq!(
            normalize_path("D:\\Code\\App\\", PathStyle::Windows),
            "d:\\code\\app"
        );
    }

    #[test]
    fn posix_rules_match_renderer() {
        assert_eq!(
            normalize_host_path("/Users//me/Repo/", false),
            "/Users/me/Repo"
        );
        assert_eq!(
            normalize_host_path("/Users/me/Repo", true),
            "/users/me/repo"
        );
        assert_eq!(normalize_host_path("/", false), "/");
        assert_eq!(normalize_host_path("//", false), "/");
        assert_ne!(
            normalize_host_path("/a/Repo", false),
            normalize_host_path("/a/repo", false)
        );
        assert_eq!(normalize_path("/home//a/App/", POSIX), "/home/a/App");
        assert_eq!(normalize_path("/Users/A/App/", MAC), "/users/a/app");
        assert_eq!(normalize_path("/a\\b", POSIX), "/a\\b");
        assert_eq!(normalize_host_path("//server/share", true), "/server/share");
        assert_eq!(normalize_host_path("CD:/x", false), "CD:/x");
    }

    #[test]
    fn verbatim_prefixes_are_stripped() {
        assert_eq!(strip_verbatim_prefix("\\\\?\\C:\\repo"), "C:\\repo");
        assert_eq!(strip_verbatim_prefix("//?/C:/repo"), "C:/repo");
        assert_eq!(
            strip_verbatim_prefix("\\\\?\\UNC\\srv\\share"),
            "\\\\srv\\share"
        );
        assert_eq!(strip_verbatim_prefix("C:/repo"), "C:/repo");
    }

    #[test]
    fn display_name_uses_last_component() {
        assert_eq!(display_name("D:\\code\\app\\"), "app");
        assert_eq!(display_name("D:/code/app"), "app");
        assert_eq!(display_name("/home/a"), "a");
        assert_eq!(display_name("/"), "/");
        assert_eq!(display_name("C:\\"), "C:");
    }
}
